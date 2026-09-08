use std::io;

use ag_orchestration::OrchestrationApprovalOutcome;
use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use tracing::warn;

use crate::app::session::{SessionTaskService, remote_branch_name_from_upstream_ref};
use crate::app::{self, App, AppEvent, ReviewCacheEntry};
use crate::domain::input::InputState;
use crate::domain::session::{FollowUpTaskAction, PublishBranchAction, SessionId, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::presentation::app_mode::{
    AppMode, ChatFocus, ConfirmationIntent, ConfirmationViewMode, DiffSidebarFocus, HelpContext,
};
use crate::presentation::help_action::{self, ViewSessionState};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState};
use crate::runtime::EventResult;
use crate::runtime::mode::chat_scroll::{self, ChatScrollMetrics};
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;
use crate::runtime::mode::input_key::is_insertable_char_key;
use crate::runtime::mode::prompt;
use crate::ui::RenderCacheStore;

#[derive(Clone)]
struct ViewContext {
    scroll_offset: Option<u16>,
    session_id: SessionId,
    session_index: usize,
}

/// Pending review and scroll updates produced by one key event in session-view
/// mode.
struct ViewPendingUpdate {
    scroll_offset: Option<u16>,
}

impl ViewPendingUpdate {
    /// Builds update state seeded from the current view scroll.
    fn from_context(view_context: &ViewContext) -> Self {
        Self {
            scroll_offset: view_context.scroll_offset,
        }
    }
}

/// Borrowed per-key context used while processing one session-view key event.
struct ViewKeyContext<'a> {
    context: &'a ViewContext,
    session_snapshot: &'a ViewSessionSnapshot,
}

/// Two-state action availability used in session-view snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewActionState {
    Disabled,
    Enabled,
}

impl ViewActionState {
    /// Returns the action state that corresponds to `is_enabled`.
    fn from_bool(is_enabled: bool) -> Self {
        if is_enabled {
            return Self::Enabled;
        }

        Self::Disabled
    }

    /// Returns whether the action is currently enabled.
    fn is_enabled(self) -> bool {
        self == Self::Enabled
    }
}

/// Snapshot of session-derived state used by view-mode key handling.
struct ViewSessionSnapshot {
    branch_actions: ViewActionState,
    continue_terminal_session: ViewActionState,
    fork_session: ViewActionState,
    follow_up_task_action: Option<FollowUpTaskAction>,
    inspect_diff: ViewActionState,
    is_managed: bool,
    is_orchestrator: bool,
    merge_session_branch: ViewActionState,
    mutate_session_branch: ViewActionState,
    open_worktree: ViewActionState,
    publish_pull_request_action: Option<PublishBranchAction>,
    rebase_session_branch: ViewActionState,
    reply_to_session: ViewActionState,
    review_comments: ViewActionState,
    session_state: ViewSessionState,
    session_status: Status,
    start_staged_session: ViewActionState,
}

impl ViewSessionSnapshot {
    /// Returns whether the active session can enter the merge queue from view
    /// mode.
    fn can_merge_session(&self) -> bool {
        self.branch_actions.is_enabled()
            && self.session_status.allows_session_actions()
            && self.can_merge_session_branch()
            && self.session_state != ViewSessionState::StackedDraft
    }

    /// Returns whether the active session can start the session sync action
    /// from view mode.
    fn can_rebase_session(&self) -> bool {
        self.branch_actions.is_enabled()
            && self.session_status.allows_rebase_action()
            && self.can_rebase_session_branch()
            && self.session_state != ViewSessionState::StackedDraft
    }

    /// Returns whether a terminal session can launch a continuation draft.
    fn can_continue_terminal_session(&self) -> bool {
        self.continue_terminal_session.is_enabled()
    }

    /// Returns whether this session can be forked from view mode.
    fn can_fork_session(&self) -> bool {
        self.fork_session.is_enabled()
    }

    /// Returns whether this session can start branch-mutating stack work.
    fn can_mutate_session_branch(&self) -> bool {
        self.mutate_session_branch.is_enabled()
    }

    /// Returns whether managed-worker-only keys may be handled.
    fn accepts_managed_keys(&self) -> bool {
        self.is_managed && self.session_state != ViewSessionState::ManagedResearch
    }

    /// Returns whether this session can enter the merge queue under stack
    /// rules.
    fn can_merge_session_branch(&self) -> bool {
        self.merge_session_branch.is_enabled()
    }

    /// Returns whether this session's local worktree can be opened.
    fn can_open_worktree(&self) -> bool {
        self.open_worktree.is_enabled()
    }

    /// Returns whether this session can start sync work under stack rules.
    fn can_rebase_session_branch(&self) -> bool {
        self.rebase_session_branch.is_enabled()
    }

    /// Returns whether this session can accept a reply under stack rules.
    fn can_reply_to_session(&self) -> bool {
        self.reply_to_session.is_enabled()
    }

    /// Returns whether the session has a linked forge review request whose
    /// comments can be opened read-only.
    fn can_open_review_comments(&self) -> bool {
        self.review_comments.is_enabled()
    }

    /// Returns whether this staged draft can start its first live turn.
    fn can_start_staged_session(&self) -> bool {
        self.start_staged_session.is_enabled()
    }

    /// Returns whether `Enter` may open a prompt composer from view mode.
    fn can_open_prompt_composer(&self) -> bool {
        if !self.session_status.allows_chat_composer() {
            return false;
        }

        self.can_edit_without_branch_work() || self.can_reply_to_session()
    }

    /// Returns whether `/` may open the slash-command composer from view mode.
    fn can_launch_configuration_composer(&self) -> bool {
        if !self.session_status.allows_session_actions() {
            return false;
        }

        self.can_edit_without_branch_work()
            || self.can_mutate_session_branch()
            || self.can_reply_to_session()
    }

    /// Returns whether image paste can open a draft prompt composer directly
    /// from view mode.
    fn can_paste_image_into_draft_composer(&self) -> bool {
        self.can_open_prompt_composer() && self.can_edit_without_branch_work()
    }

    /// Returns whether editing the viewed session only stages local draft
    /// content and therefore does not mutate a session branch.
    fn can_edit_without_branch_work(&self) -> bool {
        matches!(
            self.session_state,
            ViewSessionState::NewSession | ViewSessionState::StackedDraft
        )
    }
}

/// Processes view-mode key presses and keeps shortcut availability aligned with
/// session status (`o` disabled outside editable/review-ready local
/// worktrees, and diff/review available for review-ready statuses).
pub(crate) async fn handle_with_cache<B: Backend>(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let Some(view_context) = view_context(app) else {
        return Ok(EventResult::Continue);
    };
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    if chat_scroll::is_scroll_key(key) {
        let metrics = view_metrics(app, render_cache_store, terminal, &view_context)?;
        chat_scroll::apply_scroll_key(&mut pending_update.scroll_offset, metrics, key);
        apply_view_scroll_and_output_mode(app, pending_update.scroll_offset);

        return Ok(EventResult::Continue);
    }

    let Some(view_session_snapshot) = view_session_snapshot(app, &view_context) else {
        return Ok(EventResult::Continue);
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    if !handle_view_key(app, key, view_key_context, &mut pending_update).await {
        return Ok(EventResult::Continue);
    }

    apply_view_scroll_and_output_mode(app, pending_update.scroll_offset);

    Ok(EventResult::Continue)
}

#[cfg(test)]
async fn handle<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    handle_with_cache(app, &RenderCacheStore::default(), terminal, key).await
}

/// Applies one view-mode key press and updates pending output/scroll state.
///
/// Returns `false` when key handling already transitioned mode and should skip
/// applying pending view updates.
async fn handle_view_key(
    app: &mut App,
    key: KeyEvent,
    view_key_context: ViewKeyContext<'_>,
    pending_update: &mut ViewPendingUpdate,
) -> bool {
    let view_context = view_key_context.context;
    let view_session_snapshot = view_key_context.session_snapshot;

    if let Some(should_apply_pending_update) = handle_primary_view_key(
        app,
        key,
        view_context,
        view_session_snapshot,
        pending_update,
    )
    .await
    {
        return should_apply_pending_update;
    }

    if let Some(should_apply_pending_update) = handle_workflow_view_key(
        app,
        key,
        view_context,
        view_session_snapshot,
        pending_update,
    )
    .await
    {
        return should_apply_pending_update;
    }

    true
}

/// Handles primary session-view actions that do not need diff/review routing.
async fn handle_primary_view_key(
    app: &mut App,
    key: KeyEvent,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
    pending_update: &ViewPendingUpdate,
) -> Option<bool> {
    if view_session_snapshot.is_orchestrator
        && handle_orchestration_view_key(app, key, view_context).await
    {
        return Some(true);
    }
    let accepts_managed_keys = view_session_snapshot.accepts_managed_keys();
    if accepts_managed_keys && handle_managed_view_key(app, key, view_context) {
        return Some(false);
    }

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('o') if view_session_snapshot.can_open_worktree() => {
            return Some(handle_open_worktree_key(app, view_context, view_session_snapshot).await);
        }
        KeyCode::Char('l') if view_session_snapshot.follow_up_task_action.is_some() => {
            if let Err(error) = app
                .launch_or_open_selected_follow_up_task(&view_context.session_id)
                .await
            {
                app.append_output_for_session(
                    &view_context.session_id,
                    &TranscriptNotice::FollowUpTaskError.format(error),
                )
                .await;
            }

            return Some(false);
        }
        KeyCode::Char('s') if view_session_snapshot.can_start_staged_session() => {
            if let Err(error) = app.start_staged_session(&view_context.session_id).await {
                app.append_output_for_session(
                    &view_context.session_id,
                    &TranscriptNotice::StartError.format(error),
                )
                .await;
            }

            return Some(false);
        }
        KeyCode::Char('v' | 'V')
            if prompt::is_prompt_image_paste_key(key)
                && view_session_snapshot.can_paste_image_into_draft_composer() =>
        {
            open_draft_prompt_with_pasted_image(app, view_context, pending_update.scroll_offset)
                .await;

            return Some(false);
        }
        KeyCode::Char('c')
            if key.modifiers == event::KeyModifiers::NONE
                && view_session_snapshot.can_open_review_comments() =>
        {
            open_review_comments_in_diff(app, view_context);

            return Some(false);
        }
        KeyCode::Char('c')
            if key.modifiers == event::KeyModifiers::NONE
                && view_session_snapshot.can_continue_terminal_session() =>
        {
            open_continue_confirmation(app, view_context);

            return Some(false);
        }
        KeyCode::Char('[') if app.has_multiple_follow_up_tasks(&view_context.session_id) => {
            app.select_previous_follow_up_task(&view_context.session_id);
        }
        KeyCode::Char(']') if app.has_multiple_follow_up_tasks(&view_context.session_id) => {
            app.select_next_follow_up_task(&view_context.session_id);
        }
        KeyCode::Enter if view_session_snapshot.can_open_prompt_composer() => {
            switch_view_to_prompt(
                app,
                view_context,
                PromptHistoryState::new(session_prompt_history_entries(
                    app.sessions.session_at(view_context.session_index)?,
                )),
                InputState::default(),
                pending_update.scroll_offset,
            )
            .await;
        }
        KeyCode::Char('/')
            if view_session_snapshot.can_launch_configuration_composer()
                && is_insertable_char_key(key) =>
        {
            switch_view_to_prompt(
                app,
                view_context,
                PromptHistoryState::new(session_prompt_history_entries(
                    app.sessions.session_at(view_context.session_index)?,
                )),
                InputState::with_text("/".to_string()),
                pending_update.scroll_offset,
            )
            .await;
        }
        _ => return None,
    }

    Some(true)
}

fn open_review_comments_in_diff(app: &mut App, view_context: &ViewContext) {
    if app
        .sessions
        .session_at(view_context.session_index)
        .is_none_or(|session| session.id != view_context.session_id)
    {
        return;
    }

    app.start_diff_view_load(
        &view_context.session_id,
        None,
        DiffSidebarFocus::Comments,
        true,
    );
}

/// Opens a regular worktree immediately or warns for a managed worker.
async fn handle_open_worktree_key(
    app: &mut App,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
) -> bool {
    let restore_view = confirmation_view_mode(view_context);
    if view_session_snapshot.is_managed {
        open_managed_worktree_confirmation(app, restore_view);

        return false;
    }
    open_worktree_for_view_session(app, restore_view).await;

    true
}

/// Applies campaign-board controls owned by an orchestrator session.
async fn handle_orchestration_view_key(
    app: &mut App,
    key: KeyEvent,
    view_context: &ViewContext,
) -> bool {
    match key.code {
        KeyCode::Char('a') => {
            let outcome = app
                .approve_orchestration(&view_context.session_id, None)
                .await;
            if outcome == OrchestrationApprovalOutcome::IntegrationApproachRequired {
                app.mode = AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
                    confirmation_message: "Choose how to integrate verified task branches."
                        .to_string(),
                    confirmation_title: "Integration Approach".to_string(),
                    restore_view: Some(confirmation_view_mode(view_context)),
                    session_id: Some(view_context.session_id.clone()),
                    selected_confirmation_index: 0,
                };
            }
        }
        _ => return false,
    }

    true
}

/// Opens the one-way ownership-transfer confirmation for a managed worker.
fn handle_managed_view_key(app: &mut App, key: KeyEvent, view_context: &ViewContext) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(event::KeyModifiers::CONTROL) {
        return true;
    }
    if key.code != KeyCode::Char('D') {
        return false;
    }
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::DetachManagedSession,
        confirmation_message: "Detach this worker from its campaign and take permanent ownership?"
            .to_string(),
        confirmation_title: "Confirm Detach".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };

    true
}

/// Handles workflow actions in session view such as diff, publish, review,
/// merge, session sync, cancellation, and help.
async fn handle_workflow_view_key(
    app: &mut App,
    key: KeyEvent,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
    pending_update: &mut ViewPendingUpdate,
) -> Option<bool> {
    match key.code {
        KeyCode::Char('d')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.inspect_diff.is_enabled() =>
        {
            show_diff_for_view_session(app, view_context);
        }
        KeyCode::Char(character)
            if character.eq_ignore_ascii_case(&'p')
                && !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.publish_pull_request_action.is_some() =>
        {
            let Some(publish_pull_request_action) =
                view_session_snapshot.publish_pull_request_action
            else {
                return Some(true);
            };
            open_publish_branch_input(app, view_context, publish_pull_request_action);

            return Some(false);
        }
        KeyCode::Char('F')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.can_fork_session() =>
        {
            open_fork_confirmation(app, view_context);

            return Some(false);
        }
        KeyCode::Char('f')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.branch_actions.is_enabled()
                && view_session_snapshot.session_status.allows_review_actions() =>
        {
            open_or_regenerate_review(app, view_context, pending_update);
        }
        KeyCode::Char('m') if view_session_snapshot.can_merge_session() => {
            open_merge_confirmation(app, view_context);
        }
        KeyCode::Char('r') if view_session_snapshot.can_rebase_session() => {
            rebase_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('c')
            if key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.session_status == Status::InProgress =>
        {
            end_in_progress_turn(app, &view_context.session_id).await;

            return Some(false);
        }
        KeyCode::Char('?') => {
            open_view_help_overlay(app, view_context, view_session_snapshot);
            return Some(false);
        }
        _ => return None,
    }

    Some(true)
}

/// Opens a fork confirmation overlay for the active view session.
///
/// The body explains that the new session keeps the current transcript history
/// while starting on a fresh session branch.
fn open_fork_confirmation(app: &mut App, view_context: &ViewContext) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ForkSession,
        confirmation_message: "Fork this session into a new session with the current transcript \
                               history?"
            .to_string(),
        confirmation_title: "Confirm Fork".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Opens a merge confirmation overlay for the active view session.
///
/// The body text asks whether the current session should be added to the
/// merge queue.
fn open_merge_confirmation(app: &mut App, view_context: &ViewContext) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeSession,
        confirmation_message: "Add this session to merge queue?".to_string(),
        confirmation_title: "Confirm Merge".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Opens a continuation confirmation overlay for one terminal session.
///
/// The confirmation explains that Agentty will create a new draft session
/// seeded with initial context so the user can add more notes before starting
/// it.
fn open_continue_confirmation(app: &mut App, view_context: &ViewContext) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ContinueSession,
        confirmation_message: "Create a new draft session with initial context from this session?"
            .to_string(),
        confirmation_title: "Confirm Continue".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Warns before opening a controller-managed worker's writable worktree.
fn open_managed_worktree_confirmation(app: &mut App, restore_view: ConfirmationViewMode) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::OpenManagedWorktree,
        confirmation_message: "This opens a writable shell in a controller-managed worktree. \
                               Edits can invalidate orchestration verification. Open anyway?"
            .to_string(),
        confirmation_title: "Open Managed Worktree".to_string(),
        session_id: Some(restore_view.session_id.clone()),
        restore_view: Some(restore_view),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Opens the viewed session worktree directly or shows a command selector when
/// multiple launch configurations are configured.
pub(crate) async fn open_worktree_for_view_session(
    app: &mut App,
    restore_view: ConfirmationViewMode,
) {
    let launch_configurations = app.configured_launch_configurations();
    if launch_configurations.len() > 1 {
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: launch_configurations,
            restore_view,
            selected_command_index: 0,
        };

        return;
    }

    let selected_launch_configuration = launch_configurations.first().map(String::as_str);
    app.mode = restore_view.into_view_mode();

    app.open_session_worktree_in_tmux_with_command(selected_launch_configuration)
        .await;
}

/// Builds the view-mode snapshot used to restore chat context when a merge
/// confirmation is dismissed.
fn confirmation_view_mode(view_context: &ViewContext) -> ConfirmationViewMode {
    ConfirmationViewMode {
        scroll_offset: view_context.scroll_offset,
        session_id: view_context.session_id.clone(),
    }
}

/// Opens focused review or shows a regeneration confirmation popup.
///
/// When a review result (or error) is already present, shows a confirmation
/// popup before regenerating. If a generation is already in flight (loading),
/// the press is ignored to avoid spawning duplicate background tasks.
/// Otherwise, loads or starts focused review output and resets scroll to
/// bottom-aligned mode.
fn open_or_regenerate_review(
    app: &mut App,
    view_context: &ViewContext,
    pending_update: &mut ViewPendingUpdate,
) {
    let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
    if app.review_is_loading(&view_context.session_id) {
        return;
    }

    if review_text.is_some() || review_status_message.is_some() {
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(confirmation_view_mode(view_context)),
            session_id: Some(view_context.session_id.clone()),
            selected_confirmation_index: DEFAULT_OPTION_INDEX,
        };

        return;
    }

    open_review_output_mode(app, view_context);

    pending_update.scroll_offset = None;
}

/// Collects session-specific values used by `handle()` from the active view
/// row.
fn view_session_snapshot(app: &App, view_context: &ViewContext) -> Option<ViewSessionSnapshot> {
    let session = app.sessions.session_at(view_context.session_index)?;
    let session_status = session.status;
    let can_open_worktree = app.is_tmux_session()
        && session.allows_worktree_open_action()
        && *app
            .sessions
            .session_worktree_availability()
            .get(view_context.session_id.as_str())
            .unwrap_or(&false);

    Some(ViewSessionSnapshot {
        branch_actions: ViewActionState::from_bool(
            session.owns_branch_changes() && session.accepts_user_turns(),
        ),
        continue_terminal_session: ViewActionState::from_bool(
            session.allows_terminal_continuation(),
        ),
        fork_session: ViewActionState::from_bool(session.allows_fork_action()),
        follow_up_task_action: app.selected_follow_up_task_action(&view_context.session_id),
        inspect_diff: ViewActionState::from_bool(
            session.stats.should_show_diff()
                && (session.is_managed()
                    || (session.owns_branch_changes() && session.status.allows_diff_view())),
        ),
        is_managed: session.is_managed(),
        is_orchestrator: session.role == crate::domain::session::SessionRole::Orchestrator,
        merge_session_branch: ViewActionState::from_bool(
            session.owns_branch_changes()
                && app
                    .sessions
                    .can_merge_session_branch_in_stack(view_context.session_id.as_str()),
        ),
        mutate_session_branch: ViewActionState::from_bool(
            session.owns_branch_changes()
                && app
                    .sessions
                    .can_mutate_session_branch_in_stack(view_context.session_id.as_str()),
        ),
        open_worktree: ViewActionState::from_bool(can_open_worktree),
        publish_pull_request_action: session.publish_pull_request_action(),
        rebase_session_branch: ViewActionState::from_bool(
            session.owns_branch_changes()
                && app
                    .sessions
                    .can_rebase_session_branch_in_stack(view_context.session_id.as_str()),
        ),
        reply_to_session: ViewActionState::from_bool(
            session.accepts_user_turns()
                && app
                    .sessions
                    .can_reply_to_session_in_stack(view_context.session_id.as_str()),
        ),
        review_comments: ViewActionState::from_bool(
            session.has_review_request() && session.allows_review_comment_reply(),
        ),
        session_state: help_action::session_view_state(session),
        session_status,
        start_staged_session: ViewActionState::from_bool(
            app.sessions
                .can_start_staged_session(view_context.session_id.as_str()),
        ),
    })
}

/// Applies in-place updates for active view review status/text and scroll
/// position.
fn apply_view_scroll_and_output_mode(app: &mut App, scroll_offset: Option<u16>) {
    if let AppMode::View {
        scroll_offset: view_scroll_offset,
        ..
    } = &mut app.mode
    {
        *view_scroll_offset = scroll_offset;
    }
}

/// Handles `Ctrl+C` while a session is `InProgress` with a per-press policy.
///
/// Each press first tries to retract the most recently queued chat message on
/// [`SessionHandles::queued_messages`] (LIFO `pop_back`) so the user can undo
/// queue entries one-by-one in the reverse order they were added, without
/// interrupting the running turn. The running turn keeps streaming, status
/// stays `InProgress`, and no cancellation token, database status update, or
/// auto-review suppression runs while a queued message is being dropped.
/// When the queue is already empty, the press falls through to
/// [`cancel_in_progress_turn`] which performs the existing
/// cancel-and-return-to-`Review` flow.
async fn end_in_progress_turn(app: &mut App, session_id: &str) {
    if pop_last_queued_chat_message_if_any(app, session_id).await {
        return;
    }

    cancel_in_progress_turn(app, session_id).await;
}

/// Pops the most recently queued chat message (LIFO) from the session's
/// handles and re-syncs the snapshot from the post-pop handle state,
/// returning `true` when one queued message was retracted.
///
/// Pops the entry from the shared [`SessionHandles::queued_messages`] deque
/// via `pop_back`. The handle is the source of truth: the worker may have
/// already drained the oldest entry via `pop_front` between snapshot
/// refreshes, so a position-based snapshot pop could remove the wrong
/// transcript row and leave a phantom queued message visible. The snapshot
/// is then re-projected from the handle through
/// [`SessionState::sync_session_from_handle`], so no additional manual
/// `queued_messages` mutation is needed here. Releases any local image
/// attachments owned by the popped prompt through
/// [`App::cleanup_prompt_attachment_files`] so retracted messages do not
/// leak temp files under `AGENTTY_ROOT/tmp/`, then emits
/// [`AppEvent::SessionUpdated`] so list and chat views redraw without paying
/// for a full DB-backed `RefreshSessions` reload. Leaves the cancellation
/// token, persisted status, and auto-review suppression untouched so the
/// running turn can keep streaming.
async fn pop_last_queued_chat_message_if_any(app: &mut App, session_id: &str) -> bool {
    let popped_message = app
        .sessions
        .session_handles()
        .get(session_id)
        .and_then(|handles| handles.queued_messages.lock().ok()?.pop_back());

    let Some(popped_message) = popped_message else {
        return false;
    };

    app.sessions.sync_session_from_handle(session_id);

    app.cleanup_prompt_attachment_files(popped_message.prompt())
        .await;

    app.services.emit_app_event(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: SessionTaskService::next_session_update_version(
            &app.services.session_update_versions(),
            session_id,
        ),
    });

    true
}

/// Interrupts the active turn of a running `InProgress` session and returns it
/// to `Review`.
///
/// Cancels queued operations in the database, then fires the per-turn
/// [`CancellationToken`] so the worker's `select!` branch triggers
/// graceful channel shutdown. The worker owns process termination:
/// CLI channels receive `SIGTERM` inside the cancellation branch
/// (where the child is guaranteed alive because `run_turn` has not
/// returned yet), and app-server channels shut down through
/// `shutdown_session`. Both paths converge on the worker returning a
/// `[Stopped]` error. After signalling, the persisted status is updated to
/// `Review`, the in-memory snapshot and shared handle are refreshed, and UI
/// events are emitted so the user can inspect or continue the session instead
/// of treating it as canceled.
async fn cancel_in_progress_turn(app: &mut App, session_id: &str) {
    let timestamp_seconds =
        app::session::unix_timestamp_from_system_time(app.services.clock().now_system_time());

    if let Err(error) = app
        .services
        .db()
        .operations()
        .request_cancel_for_session_operations(session_id)
        .await
    {
        warn!(
            session_id = session_id,
            error = %error,
            "failed to request cancellation for queued session operations"
        );
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id) {
        // Cancel the current turn's token so the worker's `select!`
        // branch fires and triggers graceful channel shutdown. The
        // worker sends SIGTERM to CLI child processes inside the
        // cancellation path where the PID is guaranteed valid.
        match handles.cancel_token.lock() {
            Ok(cancel_token) => cancel_token.cancel(),
            Err(error) => {
                warn!(
                    session_id = session_id,
                    error = %error,
                    "failed to lock session cancel token"
                );
            }
        }
    }

    if let Err(error) = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(
            session_id,
            &Status::Review.to_string(),
            timestamp_seconds,
        )
        .await
    {
        warn!(
            session_id = session_id,
            error = %error,
            "failed to persist review status after interrupting session turn"
        );

        return;
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id)
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::Review;
    }

    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        let previous_status = session.status;
        session.status = Status::Review;
        session.reconcile_status_transition(previous_status);
    }

    suppress_auto_review_for_stopped_turn(app, session_id);

    app.services.emit_app_event(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: SessionTaskService::next_session_update_version(
            &app.services.session_update_versions(),
            session_id,
        ),
    });
    app.services.emit_session_and_project_refresh_events();
}

/// Marks automatic focused review as suppressed after a user stops one active
/// turn.
///
/// The session remains review-ready, but the reducer's automatic focused
/// review pass should not immediately start an agent review for the partially
/// stopped turn. The marker is intentionally inserted without loading a diff so
/// `Ctrl+C` returns to the event loop quickly; the next submitted turn clears
/// the cache, and pressing `f` still starts manual focused review because
/// view-mode review handling replaces suppressed entries.
fn suppress_auto_review_for_stopped_turn(app: &mut App, session_id: &str) {
    app.suppress_review_output(session_id);
}

/// Switches the TUI mode from session view to the prompt input.
///
/// Focused-review output/status is copied into prompt mode so canceling the
/// composer returns to the same session transcript state. The caller supplies
/// the initial composer buffer so session-view shortcuts like `/` can open the
/// prompt with prefilled slash-command text. A non-empty initial buffer
/// intentionally replaces any saved composer for the session.
async fn switch_view_to_prompt(
    app: &mut App,
    view_context: &ViewContext,
    history_state: PromptHistoryState,
    input: InputState,
    scroll_offset: Option<u16>,
) {
    if input.is_empty() && app.restore_prompt_progress(&view_context.session_id).await {
        return;
    }

    app.discard_prompt_progress(&view_context.session_id).await;

    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state,
        slash_state: app.prompt_slash_state(),
        session_id: view_context.session_id.clone(),
        input,
        scroll_offset,
    };
}

/// Opens a draft composer from view mode and immediately applies the existing
/// prompt image-paste intent.
async fn open_draft_prompt_with_pasted_image(
    app: &mut App,
    view_context: &ViewContext,
    scroll_offset: Option<u16>,
) {
    let Some(session) = app.sessions.session_at(view_context.session_index) else {
        return;
    };
    let history_state = PromptHistoryState::new(session_prompt_history_entries(session));

    switch_view_to_prompt(
        app,
        view_context,
        history_state,
        InputState::default(),
        scroll_offset,
    )
    .await;

    prompt::paste_image_into_active_prompt(app, &view_context.session_id).await;
}

/// Opens the help overlay while preserving the currently viewed session state.
fn open_view_help_overlay(
    app: &mut App,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
) {
    app.mode = AppMode::Help {
        context: HelpContext::View {
            can_fork_session: view_session_snapshot.can_fork_session(),
            can_merge_session_branch: view_session_snapshot.can_merge_session_branch(),
            can_mutate_session_branch: view_session_snapshot.can_mutate_session_branch(),
            can_open_worktree: view_session_snapshot.can_open_worktree(),
            can_rebase_session_branch: view_session_snapshot.can_rebase_session_branch(),
            can_reply_to_session: view_session_snapshot.can_reply_to_session(),
            can_show_diff: view_session_snapshot.inspect_diff.is_enabled(),
            can_start_staged_session: view_session_snapshot.can_start_staged_session(),
            can_view_review_comments: view_session_snapshot.can_open_review_comments(),
            publish_pull_request_action: view_session_snapshot.publish_pull_request_action,
            session_id: view_context.session_id.clone(),
            session_state: view_session_snapshot.session_state,
            scroll_offset: view_context.scroll_offset,
        },
        scroll_offset: 0,
    };
}

/// Opens the session-view publish popup and preserves the current view state
/// for cancel or submit.
fn open_publish_branch_input(
    app: &mut App,
    view_context: &ViewContext,
    publish_branch_action: PublishBranchAction,
) {
    let Some(session) = app.sessions.session_at(view_context.session_index) else {
        return;
    };
    let default_branch_name = crate::app::session::session_branch(&session.id);
    let locked_upstream_ref = session.published_upstream_ref.clone();
    let input = locked_upstream_ref
        .as_deref()
        .map(remote_branch_name_from_upstream_ref)
        .map(InputState::with_text)
        .unwrap_or_default();

    app.mode = AppMode::PublishBranchInput {
        default_branch_name,
        input,
        locked_upstream_ref,
        publish_branch_action,
        restore_view: confirmation_view_mode(view_context),
    };
}

fn view_context(app: &mut App) -> Option<ViewContext> {
    let (session_id, scroll_offset) = match &app.mode {
        AppMode::View {
            session_id,
            scroll_offset,
        } => (session_id.clone(), *scroll_offset),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    Some(ViewContext {
        scroll_offset,
        session_id,
        session_index,
    })
}

fn view_metrics<B: Backend>(
    app: &App,
    render_cache_store: &RenderCacheStore,
    terminal: &Terminal<B>,
    view_context: &ViewContext,
) -> io::Result<ChatScrollMetrics>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let terminal_size = terminal.size().map_err(crate::runtime::backend_err)?;
    let terminal_rect = Rect::new(0, 0, terminal_size.width, terminal_size.height);

    Ok(ChatScrollMetrics::new(
        app,
        render_cache_store,
        &view_context.session_id,
        view_context.session_index,
        terminal_rect,
    ))
}

/// Returns prompt-history entries for the session-view prompt composer.
///
/// Draft sessions use the staged prompt stored in `prompt` directly because
/// they have not yet written user prompts into the persisted transcript.
/// Started sessions read typed user rows so generated agent prompts remain
/// available for provider replay without entering user-facing history.
pub(super) fn session_prompt_history_entries(
    session: &crate::domain::session::Session,
) -> Vec<String> {
    if session.status == Status::Draft && session.is_draft_session() {
        return vec![session.prompt.clone()];
    }

    session
        .transcript
        .as_ref()
        .map(|transcript| {
            transcript
                .messages()
                .iter()
                .filter(|message| message.kind == SessionMessageKind::UserPrompt)
                .map(|message| message.content.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Opens review mode and serves cached review or loading status.
///
/// Reviews are auto-generated when sessions transition to `Review`. When the
/// user presses `f` and no cached review exists yet, Agentty requests the
/// current diff in the background, starts generation, and shows a loading
/// message immediately. The resulting review is appended into the normal
/// session output panel instead of replacing it, and successful review text is
/// persisted for restart hydration.
fn open_review_output_mode(app: &mut App, view_context: &ViewContext) {
    if let Some(cached) = app.review_cache.get(view_context.session_id.as_str())
        && !matches!(cached, ReviewCacheEntry::Suppressed)
    {
        return;
    }
    if app
        .sessions
        .session_at(view_context.session_index)
        .is_none_or(|session| session.id != view_context.session_id)
    {
        return;
    }

    app.start_manual_review_diff_load(&view_context.session_id);
}

/// Opens diff mode only when the viewed session has actual worktree changes.
///
/// Returns `true` when diff mode was opened and `false` when the session diff
/// is empty, which keeps the view page in place so the `d` shortcut behaves as
/// unavailable for unchanged review sessions.
fn show_diff_for_view_session(app: &mut App, view_context: &ViewContext) -> bool {
    if app
        .sessions
        .session_at(view_context.session_index)
        .is_none_or(|session| session.id != view_context.session_id)
    {
        return false;
    }

    app.start_diff_view_load(
        &view_context.session_id,
        None,
        DiffSidebarFocus::Files,
        false,
    )
}

/// Starts session sync and reports whether the rebase command was accepted.
async fn rebase_view_session(app: &mut App, session_id: &str) -> bool {
    if let Err(error) = app.rebase_session(session_id).await {
        app.append_output_for_session(session_id, &TranscriptNotice::RebaseError.format(error))
            .await;

        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use mockall::predicate::eq;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::app::{REVIEW_NO_DIFF_MESSAGE, diff_content_hash, review_loading_message};
    use crate::domain::agent::AgentModel;
    use crate::domain::orchestration::OrchestrationStatus;
    use crate::domain::session::{
        ForgeKind, QueuedMessage, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
        SessionDiffState, SessionRole,
    };
    use crate::domain::session_message::{SessionMessage, SessionTranscript};
    use crate::domain::transient_message::{
        TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
        TransientMessageSlot,
    };
    use crate::domain::turn_prompt::TurnPrompt;
    use crate::infra::tmux::{MockTmuxClient, TmuxClient};
    use crate::presentation::app_mode::{DiffCommentTarget, DiffLineComments, PromptModeSnapshot};
    use crate::presentation::prompt::PromptSlashState;
    use crate::runtime::mode::session_output_metric;
    use crate::ui::component::session_output::SessionOutputLineContext;
    use crate::ui::page::session_chat::SessionChatPage;

    fn queued_message(order: u64, text: &str) -> QueuedMessage {
        QueuedMessage::new(order, TurnPrompt::from_text(text.to_string()))
    }

    fn session_replay_text(session: &crate::domain::session::Session) -> String {
        session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .unwrap_or_default()
    }

    /// Applies queued app events through the first completed full-diff load.
    async fn apply_next_session_diff(app: &mut App) {
        loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(10), app.next_app_event())
                    .await
                    .expect("session diff event should arrive")
                    .expect("app event channel should remain open");
            let is_session_diff = matches!(event, AppEvent::SessionDiffLoaded { .. });
            app.apply_app_events(event).await;
            if is_session_diff {
                return;
            }
        }
    }

    /// Applies a deterministic completion for the one pending session-diff
    /// request.
    async fn apply_pending_session_diff(
        app: &mut App,
        session_id: &SessionId,
        result: Result<&str, &str>,
    ) {
        let request_id = app
            .pending_session_diff_requests
            .keys()
            .copied()
            .next()
            .expect("session diff request should be pending");
        app.apply_app_events(AppEvent::SessionDiffLoaded {
            request_id,
            result: result.map(str::to_string).map_err(str::to_string),
            session_id: session_id.clone(),
        })
        .await;
    }

    /// Builds one git-backed test app with one created session and an
    /// injected tmux boundary.
    async fn new_test_app_with_session_and_tmux_client(
        tmux_client: Arc<dyn TmuxClient>,
    ) -> (App, tempfile::TempDir, String) {
        let (mut app, base_dir) =
            crate::test_support::new_git_test_app_with_tmux_client(tmux_client).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        (app, base_dir, session_id)
    }

    /// Builds one git-backed test app with one created session and a strict
    /// mocked tmux boundary.
    async fn new_test_app_with_session() -> (App, tempfile::TempDir, String) {
        new_test_app_with_session_and_tmux_client(Arc::new(MockTmuxClient::new())).await
    }

    /// Attaches one open GitHub review request to a session fixture.
    fn attach_open_review_request(session: &mut crate::domain::session::Session) {
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/linked-terminal".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Linked terminal session".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        });
    }

    /// Builds one reply-enabled review snapshot for primary-key routing tests.
    fn reply_enabled_review_snapshot() -> ViewSessionSnapshot {
        ViewSessionSnapshot {
            branch_actions: ViewActionState::Enabled,
            continue_terminal_session: ViewActionState::Disabled,
            fork_session: ViewActionState::Enabled,
            inspect_diff: ViewActionState::Enabled,
            is_managed: false,
            is_orchestrator: false,
            merge_session_branch: ViewActionState::Enabled,
            mutate_session_branch: ViewActionState::Enabled,
            rebase_session_branch: ViewActionState::Enabled,
            open_worktree: ViewActionState::Enabled,
            reply_to_session: ViewActionState::Enabled,
            review_comments: ViewActionState::Disabled,
            start_staged_session: ViewActionState::Disabled,
            follow_up_task_action: None,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Review,
            session_status: Status::Review,
        }
    }

    /// Replaces the app-level clipboard-image dependency with one
    /// caller-provided mock.
    fn install_mock_clipboard_image_client(
        app: &mut App,
        mock_clipboard_image_client: crate::infra::clipboard_image::MockClipboardImageClient,
    ) {
        let clipboard_image_client: Arc<dyn crate::infra::clipboard_image::ClipboardImageClient> =
            Arc::new(mock_clipboard_image_client);
        let base_path = app.services.base_path().to_path_buf();
        let db = app.services.db().clone();
        let event_sender = app.services.event_sender();
        let available_agent_kinds = app.services.available_agent_kinds();
        let available_agent_clis =
            crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
        let app_server_client_override = app.services.app_server_client_override();
        let fs_client = app.services.fs_client();
        let git_client = app.services.git_client();
        let review_request_client = app.services.review_request_client();

        app.services = crate::app::AppServices::new_with_agent_clis(
            base_path,
            app.services.clock(),
            event_sender,
            crate::app::AppServiceDeps {
                app_server_client_override,
                available_agent_kinds,
                clipboard_image_client_override: Some(clipboard_image_client),
                fs_client,
                git_client,
                one_shot_client_override: None,
                personality_catalog_client_override: None,
                repositories: db,
                review_request_client,
            },
            available_agent_clis,
        );
    }

    /// Builds one minimal session snapshot for pure view-state tests.
    fn session_fixture(status: Status, is_draft: bool) -> crate::domain::session::Session {
        crate::test_support::SessionFixtureBuilder::new()
            .status(status)
            .draft(is_draft)
            .folder(std::env::temp_dir())
            .project_name("")
            .build()
    }

    #[tokio::test]
    async fn test_view_context_returns_none_for_non_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::List;

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_none());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_ignores_key_when_mode_is_not_view() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::List;
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let result = handle(
            &mut app,
            &mut terminal,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("non-view key should be handled");

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_view_context_falls_back_to_list_when_session_is_missing() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::View {
            session_id: "missing-session".into(),
            scroll_offset: Some(2),
        };

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_none());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_view_context_returns_existing_session_details() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(4),
        };

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_some());
        let context = context.expect("expected view context");
        assert_eq!(context.session_id, session_id);
        assert_eq!(context.scroll_offset, Some(4));
        assert_eq!(context.session_index, 0);
    }

    #[tokio::test]
    async fn test_view_session_snapshot_disables_actions_for_done_session() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::Done;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(snapshot.can_continue_terminal_session());
        assert!(!snapshot.can_open_worktree());
        assert_eq!(snapshot.session_state, ViewSessionState::Done);
        assert_eq!(snapshot.session_status, Status::Done);
    }

    #[tokio::test]
    async fn test_view_session_snapshot_enables_continue_for_canceled_session() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::Canceled;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(snapshot.can_continue_terminal_session());
        assert!(!snapshot.can_open_worktree());
        assert_eq!(snapshot.session_state, ViewSessionState::Canceled);
        assert_eq!(snapshot.session_status, Status::Canceled);
    }

    #[tokio::test]
    async fn managed_review_session_snapshot_allows_open_but_hides_review_comments() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let session = &mut app.sessions.sessions_mut()[0];
        attach_open_review_request(session);
        session.role = SessionRole::OrchestrationWorker;
        session.status = Status::Review;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(snapshot.can_open_worktree());
        assert!(!snapshot.can_open_review_comments());
    }

    #[tokio::test]
    async fn managed_review_session_snapshot_hides_open_outside_tmux() {
        // Arrange
        let clients = crate::test_support::test_app_clients_with_mock_app_server()
            .with_tmux_client(Arc::new(MockTmuxClient::new()))
            .with_tmux_session(false);
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session = &mut app.sessions.sessions_mut()[0];
        session.role = SessionRole::OrchestrationWorker;
        session.status = Status::Review;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(!snapshot.can_open_worktree());
    }

    #[tokio::test]
    async fn managed_running_session_snapshot_hides_worktree_open() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let session = &mut app.sessions.sessions_mut()[0];
        session.role = SessionRole::OrchestrationWorker;
        session.status = Status::InProgress;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(!snapshot.can_open_worktree());
    }

    #[tokio::test]
    async fn diff_snapshot_keeps_managed_worker_inspection_without_controller_action() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let session = &mut app.sessions.sessions_mut()[0];
        session.role = SessionRole::Orchestrator;
        session.status = Status::Review;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let controller_snapshot =
            view_session_snapshot(&app, &context).expect("expected controller snapshot");
        app.sessions.sessions_mut()[0].role = SessionRole::OrchestrationWorker;
        let managed_worker_snapshot =
            view_session_snapshot(&app, &context).expect("expected managed worker snapshot");

        // Assert
        assert_eq!(controller_snapshot.inspect_diff, ViewActionState::Disabled);
        assert_eq!(
            managed_worker_snapshot.inspect_diff,
            ViewActionState::Enabled
        );
    }

    #[tokio::test]
    async fn diff_snapshot_hides_known_empty_session_diff() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let session = &mut app.sessions.sessions_mut()[0];
        session.status = Status::Review;
        session.stats.diff_state = SessionDiffState::Empty;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert_eq!(snapshot.inspect_diff, ViewActionState::Disabled);
    }

    #[tokio::test]
    async fn test_view_session_snapshot_hides_worktree_open_for_unstarted_draft_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_tmux_client(Arc::new(MockTmuxClient::new()))
                .await;
        let session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(!snapshot.can_open_worktree());
        assert_eq!(snapshot.session_state, ViewSessionState::NewSession);
        assert!(snapshot.can_paste_image_into_draft_composer());
        assert!(!snapshot.can_rebase_session());
    }

    #[tokio::test]
    async fn test_view_session_snapshot_allows_start_for_stacked_draft() {
        // Arrange
        let (mut app, _base_dir, parent_session_id) = new_test_app_with_session().await;
        let session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        let parent_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == parent_session_id)
            .expect("expected parent session");
        parent_session.status = Status::Review;
        let session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("expected draft session");
        session.parent_session_id = Some(parent_session_id.clone().into());
        session.prompt = "staged child draft".to_string();
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert_eq!(snapshot.session_state, ViewSessionState::StackedDraft);
        assert!(snapshot.can_start_staged_session());
        assert!(snapshot.can_paste_image_into_draft_composer());
        assert!(!snapshot.can_merge_session());
        assert!(!snapshot.can_rebase_session());
    }

    #[tokio::test]
    async fn test_open_draft_prompt_with_pasted_image_inserts_clipboard_image() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_tmux_client(Arc::new(MockTmuxClient::new()))
                .await;
        let session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let expected_session_id = session_id.clone();
        let expected_session_id_for_mock = expected_session_id.clone();
        let mut clipboard_image_client =
            crate::infra::clipboard_image::MockClipboardImageClient::new();
        clipboard_image_client
            .expect_persist_clipboard_image()
            .once()
            .withf(move |session_id, attachment_number| {
                session_id == &expected_session_id_for_mock && *attachment_number == 1
            })
            .returning(|_, _| {
                Box::pin(async {
                    Ok(crate::infra::clipboard_image::PersistedClipboardImage {
                        local_image_path: std::path::PathBuf::from("/tmp/draft-image.png"),
                    })
                })
            });
        install_mock_clipboard_image_client(&mut app, clipboard_image_client);

        // Act
        open_draft_prompt_with_pasted_image(&mut app, &view_context, Some(2)).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref input,
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if input.text() == "[Image #1]"
                && session_id.as_str() == expected_session_id.as_str()
        ));
    }

    #[tokio::test]
    async fn test_open_draft_prompt_with_pasted_image_ignores_missing_session() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let view_context = ViewContext {
            scroll_offset: Some(2),
            session_id: "missing-session".into(),
            session_index: usize::MAX,
        };

        // Act
        open_draft_prompt_with_pasted_image(&mut app, &view_context, Some(2)).await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_view_session_snapshot_blocks_stacked_draft_start_until_parent_review() {
        // Arrange
        let (mut app, _base_dir, parent_session_id) = new_test_app_with_session().await;
        let session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        let parent_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == parent_session_id)
            .expect("expected parent session");
        parent_session.status = Status::InProgress;
        let session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("expected draft session");
        session.parent_session_id = Some(parent_session_id.into());
        session.prompt = "staged child draft".to_string();
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert_eq!(snapshot.session_state, ViewSessionState::StackedDraft);
        assert!(!snapshot.can_start_staged_session());
        assert!(snapshot.can_open_prompt_composer());
    }

    #[tokio::test]
    async fn test_view_session_snapshot_keeps_parent_reply_with_review_child() {
        // Arrange
        let (mut app, _base_dir, parent_session_id) = new_test_app_with_session().await;
        let child_session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        let parent_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == parent_session_id)
            .expect("expected parent session");
        parent_session.status = Status::Review;
        let child_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == child_session_id)
            .expect("expected child session");
        child_session.parent_session_id = Some(parent_session_id.clone().into());
        child_session.status = Status::Review;
        app.mode = AppMode::View {
            session_id: parent_session_id.clone().into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert_eq!(snapshot.session_state, ViewSessionState::Review);
        assert!(snapshot.can_open_prompt_composer());
        assert!(snapshot.can_merge_session());
        assert!(snapshot.can_rebase_session());
    }

    #[tokio::test]
    async fn test_view_session_snapshot_blocks_parent_reply_with_running_child() {
        // Arrange
        let (mut app, _base_dir, parent_session_id) = new_test_app_with_session().await;
        let child_session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        let parent_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == parent_session_id)
            .expect("expected parent session");
        parent_session.status = Status::Review;
        let child_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == child_session_id)
            .expect("expected child session");
        child_session.parent_session_id = Some(parent_session_id.clone().into());
        child_session.status = Status::InProgress;
        app.mode = AppMode::View {
            session_id: parent_session_id.clone().into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert_eq!(snapshot.session_state, ViewSessionState::Review);
        assert!(!snapshot.can_open_prompt_composer());
        assert!(!snapshot.can_merge_session());
        assert!(!snapshot.can_rebase_session());
    }

    #[tokio::test]
    async fn test_view_session_snapshot_reads_cached_worktree_availability() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions
            .set_session_worktree_available(&session_id, false);
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(!snapshot.can_open_worktree());
    }

    #[tokio::test]
    async fn test_view_session_snapshot_returns_none_for_stale_session_index() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(1),
        };
        let mut context = view_context(&mut app).expect("expected view context");
        context.session_index = 99;

        // Act
        let snapshot = view_session_snapshot(&app, &context);

        // Assert
        assert!(snapshot.is_none());
    }

    #[tokio::test]
    async fn test_view_total_lines_counts_wrapped_output_lines() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].transcript = Some(
            crate::test_support::assistant_transcript("word ".repeat(40)),
        );
        let raw_line_count = u16::try_from(
            session_replay_text(&app.sessions.sessions()[0])
                .lines()
                .count(),
        )
        .unwrap_or(u16::MAX);

        // Act
        let total_lines =
            session_output_metric::rendered_output_line_count(&app, &session_id, 0, 20, 5);

        // Assert
        assert!(total_lines > raw_line_count);
    }

    #[test]
    fn test_session_prompt_history_entries_excludes_generated_agent_prompts() {
        // Arrange
        let mut session = crate::test_support::SessionFixtureBuilder::new()
            .status(Status::Review)
            .build();
        session.transcript = Some(SessionTranscript::new(vec![
            SessionMessage::conversation(
                0,
                SessionMessageKind::UserPrompt,
                "first line\n\nsecond line",
            ),
            SessionMessage::conversation(
                1,
                SessionMessageKind::AgentPrompt,
                "Process the selected review comments",
            ),
            SessionMessage::conversation(
                2,
                SessionMessageKind::AssistantAnswer,
                "Resolved the review comments",
            ),
            SessionMessage::conversation(3, SessionMessageKind::UserPrompt, "latest prompt"),
        ]));

        // Act
        let entries = session_prompt_history_entries(&session);

        // Assert
        assert_eq!(
            entries,
            vec![
                "first line\n\nsecond line".to_string(),
                "latest prompt".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn test_scroll_offset_down_does_not_jump_to_bottom_for_wrapped_output() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            "word ".repeat(60),
        )]);
        app.sessions.sessions_mut()[0].transcript = Some(transcript);
        let metrics = ChatScrollMetrics {
            total_lines: session_output_metric::rendered_output_line_count(
                &app,
                &session_id,
                0,
                20,
                5,
            ),
            view_height: 5,
        };

        // Act
        let next_offset = chat_scroll::scroll_offset_down(Some(0), metrics, 1);

        // Assert
        assert_eq!(next_offset, Some(1));
    }

    #[tokio::test]
    async fn test_apply_view_scroll_and_output_mode_updates_scroll_state() {
        // Arrange
        let (mut app, _base_dir, expected_session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: expected_session_id.clone().into(),
            scroll_offset: Some(3),
        };

        // Act
        apply_view_scroll_and_output_mode(&mut app, Some(1));

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(1),
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_view_total_lines_uses_default_review_model_for_loading_fallback() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Claude,
            AgentModel::ClaudeHaiku4520251001,
        );
        app.sessions.sessions_mut()[0].agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            AgentModel::Gpt56Sol,
        );
        app.sessions.sessions_mut()[0].status = Status::AgentReview;
        let output_width = 14;
        let viewport_height = 5;
        let render_cache_store = RenderCacheStore::default();
        let session = &app.sessions.sessions()[0];
        let expected = SessionChatPage::rendered_output_line_count(
            session,
            output_width,
            viewport_height,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                session_update_version: app.session_update_version(&session_id),
            },
            render_cache_store.markdown_render_cache(),
            render_cache_store.session_output_layout_cache(),
        );

        // Act
        let total_lines = session_output_metric::rendered_output_line_count(
            &app,
            &session_id,
            0,
            output_width,
            viewport_height,
        );

        // Assert
        assert_eq!(total_lines, expected);
    }

    #[tokio::test]
    async fn test_open_review_output_mode_leaves_existing_cache_unchanged() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.review_cache.insert(
            session_id.clone().into(),
            ReviewCacheEntry::Ready {
                diff_hash: 123,
                text: "Cached review".to_string(),
            },
        );
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        open_review_output_mode(&mut app, &view_context);

        // Assert
        let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
        assert_eq!(review_status_message, None);
        assert_eq!(review_text, Some("Cached review"));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_starts_loading_when_diff_exists() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Claude,
            AgentModel::ClaudeOpus5,
        );
        app.sessions.sessions_mut()[0].status = Status::Review;
        let session_folder = app.sessions.sessions()[0].folder.clone();
        std::fs::write(session_folder.join("README.md"), "review test content\n")
            .expect("failed to update readme");
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        open_review_output_mode(&mut app, &view_context);

        // Assert
        let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
        assert_eq!(
            review_status_message,
            Some(review_loading_message(app.review_agent()))
        );
        assert_eq!(review_text, None);
        assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
        assert!(matches!(
            app.review_cache.get(&view_context.session_id),
            Some(ReviewCacheEntry::Loading { .. })
        ));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_shows_no_diff_message_when_diff_empty() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::Review;
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        open_review_output_mode(&mut app, &view_context);
        apply_pending_session_diff(&mut app, &view_context.session_id, Ok("")).await;

        // Assert
        let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
        assert_eq!(review_status_message, None);
        assert_eq!(review_text, Some(REVIEW_NO_DIFF_MESSAGE));
        assert!(matches!(
            app.review_cache.get(&view_context.session_id),
            Some(ReviewCacheEntry::Ready {
                diff_hash,
                text,
            }) if *diff_hash == diff_content_hash("") && text == REVIEW_NO_DIFF_MESSAGE
        ));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_ignores_stale_session_selection() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.into(),
            session_index: 99,
        };
        let mut app = app;

        // Act
        open_review_output_mode(&mut app, &view_context);

        // Assert
        assert!(!app.review_cache.contains_key(&view_context.session_id));
    }

    #[tokio::test]
    async fn test_show_diff_for_view_session_switches_mode_to_diff() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.clone().into(),
            session_index: 0,
        };

        // Act
        let opened = show_diff_for_view_session(&mut app, &context);
        let loading = matches!(app.mode, AppMode::DiffLoading { .. });
        apply_pending_session_diff(
            &mut app,
            &context.session_id,
            Ok("diff --git a/README.md b/README.md\n+updated content\n"),
        )
        .await;

        // Assert
        assert!(opened);
        assert!(loading);
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                ref session_id,
            scroll_offset: 0,
                ..
            } if session_id == &context.session_id
        ));
    }

    #[tokio::test]
    async fn test_show_diff_for_view_session_keeps_view_mode_when_diff_is_empty() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.clone().into(),
            session_index: 0,
        };

        // Act
        let opened = show_diff_for_view_session(&mut app, &context);
        let loading = matches!(app.mode, AppMode::DiffLoading { .. });
        apply_pending_session_diff(&mut app, &context.session_id, Ok("")).await;

        // Assert
        assert!(opened);
        assert!(loading);
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(0),
                ..
            } if session_id == &context.session_id
        ));
    }

    #[tokio::test]
    async fn test_show_diff_for_view_session_restores_view_after_git_error() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.clone().into(),
            session_index: 0,
        };

        // Act
        let opened = show_diff_for_view_session(&mut app, &context);
        apply_pending_session_diff(
            &mut app,
            &context.session_id,
            Err("Failed to run git diff: repository unavailable"),
        )
        .await;

        // Assert
        assert!(opened);
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(0),
                ..
            } if session_id == &context.session_id
        ));
        let workflow_notice = app.sessions.sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
            .expect("diff load failure should be visible in the restored session view");
        assert!(workflow_notice.body.text().contains("Unable to load diff:"));
        assert!(
            workflow_notice
                .body
                .text()
                .contains("Failed to run git diff:")
        );
    }

    /// Verifies a stale view index cannot start a background diff load.
    #[tokio::test]
    async fn test_show_diff_for_view_session_rejects_stale_session_index() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.into(),
            session_index: 99,
        };

        // Act
        let opened = show_diff_for_view_session(&mut app, &context);

        // Assert
        assert!(!opened);
        assert!(!matches!(app.mode, AppMode::DiffLoading { .. }));
    }

    #[tokio::test]
    async fn managed_done_session_loads_archived_diff_without_worktree() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let archived_diff = "diff --git a/worker.txt b/worker.txt\n+new work\n";
        app.services
            .db()
            .sessions()
            .update_session_archived_diff(&session_id, Some(archived_diff.to_string()))
            .await
            .expect("failed to persist archived diff");
        let session = &mut app.sessions.sessions_mut()[0];
        session.folder = PathBuf::from("missing-managed-worktree");
        session.role = SessionRole::OrchestrationWorker;
        session.status = Status::Done;
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        let opened = show_diff_for_view_session(&mut app, &context);
        apply_next_session_diff(&mut app).await;

        // Assert
        assert!(opened);
        assert!(matches!(
            app.mode,
            AppMode::Diff { ref diff, .. } if diff == archived_diff
        ));

        // Act
        app.services
            .db()
            .sessions()
            .update_session_archived_diff(&context.session_id, None)
            .await
            .expect("failed to clear archived diff");
        let missing_opened = show_diff_for_view_session(&mut app, &context);
        apply_next_session_diff(&mut app).await;

        // Assert
        assert!(missing_opened);
        assert!(matches!(app.mode, AppMode::View { .. }));
    }

    #[tokio::test]
    async fn canceled_research_session_loads_archived_diff_without_worktree() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let archived_diff = "diff --git a/policy.txt b/policy.txt\n+unexpected write\n";
        app.services
            .db()
            .sessions()
            .update_session_archived_diff(&session_id, Some(archived_diff.to_string()))
            .await
            .expect("failed to persist archived diff");
        let session = &mut app.sessions.sessions_mut()[0];
        session.folder = PathBuf::from("reclaimed-research-worktree");
        session.role = SessionRole::OrchestrationResearcher;
        session.status = Status::Canceled;
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        let opened = show_diff_for_view_session(&mut app, &context);
        apply_next_session_diff(&mut app).await;

        // Assert
        assert!(opened);
        assert!(matches!(app.mode, AppMode::Diff { ref diff, .. } if diff == archived_diff));
    }

    #[tokio::test]
    async fn managed_done_session_restores_view_after_archived_diff_load_failure() {
        // Arrange
        let (mut app, _base_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session = &mut app.sessions.sessions_mut()[0];
        session.role = SessionRole::OrchestrationWorker;
        session.status = Status::Done;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.into(),
            session_index: 0,
        };
        pool.close().await;

        // Act
        let opened = show_diff_for_view_session(&mut app, &context);
        apply_next_session_diff(&mut app)
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert!(opened);
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(0),
                ..
            } if session_id == &context.session_id
        ));
        let workflow_notice = app.sessions.sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
            .expect("archived diff load failure should be visible in the restored view");
        assert!(
            workflow_notice
                .body
                .text()
                .contains("Failed to load archived diff:")
        );
    }

    #[tokio::test]
    async fn test_append_output_for_session_appends_text() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let mut app = app;

        // Act
        app.append_output_for_session(&session_id, "line one").await;

        // Assert
        app.sessions.sync_from_handles();
        let output = session_replay_text(&app.sessions.sessions()[0]);
        assert_eq!(output, "line one");
    }

    #[tokio::test]
    async fn test_open_merge_confirmation_sets_confirmation_mode_with_view_restore_state() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(5),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        open_merge_confirmation(&mut app, &context);

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::MergeSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: Some(ConfirmationViewMode {
                    scroll_offset: Some(5),
                    session_id: ref restored_session_id,
                }),
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if confirmation_title == "Confirm Merge"
                && confirmation_message == "Add this session to merge queue?"
                && restored_session_id == &session_id
                && mode_session_id == &session_id
        ));
    }

    #[tokio::test]
    async fn test_open_worktree_for_view_session_opens_command_selector_for_multiple_commands() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(4),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        open_worktree_for_view_session(&mut app, confirmation_view_mode(&context)).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                ref commands,
                restore_view:
                    ConfirmationViewMode {
                        session_id: ref restored_session_id,
            scroll_offset: Some(4),
                    },
                selected_command_index: 0,
            } if commands == &vec!["cargo test".to_string(), "npm run dev".to_string()]
                && restored_session_id == &session_id
        ));
    }

    #[tokio::test]
    async fn test_open_worktree_for_view_session_keeps_view_mode_for_single_command() {
        // Arrange
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client
            .expect_open_window_for_folder()
            .times(1)
            .returning(|_| Box::pin(async { Some("@42".to_string()) }));
        mock_tmux_client
            .expect_run_command_in_window()
            .with(eq("@42".to_string()), eq("cargo test".to_string()))
            .times(1)
            .returning(|_, _| Box::pin(async {}));
        let (mut app, _base_dir, session_id) =
            new_test_app_with_session_and_tmux_client(Arc::new(mock_tmux_client)).await;
        app.settings.launch_configuration = "cargo test".to_string();
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        open_worktree_for_view_session(&mut app, confirmation_view_mode(&context)).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref mode_session_id,
            scroll_offset: Some(2),
            } if mode_session_id == &session_id
        ));
    }

    #[tokio::test]
    async fn test_rebase_view_session_appends_error_output_without_review_status() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let mut app = app;

        // Act
        rebase_view_session(&mut app, &session_id).await;

        // Assert
        app.sessions.sync_from_handles();
        let output = session_replay_text(&app.sessions.sessions()[0]);
        assert!(output.contains("[Sync Error]"));
    }

    #[tokio::test]
    async fn test_open_view_help_overlay_preserves_view_context() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            scroll_offset: Some(3),
            session_id: session_id.clone().into(),
            session_index: 0,
        };
        let view_session_snapshot = ViewSessionSnapshot {
            branch_actions: ViewActionState::Enabled,
            continue_terminal_session: ViewActionState::Disabled,
            fork_session: ViewActionState::Enabled,
            inspect_diff: ViewActionState::Enabled,
            is_managed: false,
            is_orchestrator: false,
            merge_session_branch: ViewActionState::Enabled,
            mutate_session_branch: ViewActionState::Enabled,
            rebase_session_branch: ViewActionState::Enabled,
            open_worktree: ViewActionState::Enabled,
            reply_to_session: ViewActionState::Enabled,
            review_comments: ViewActionState::Disabled,
            start_staged_session: ViewActionState::Disabled,
            follow_up_task_action: None,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
            session_status: Status::Review,
        };

        // Act
        open_view_help_overlay(&mut app, &view_context, &view_session_snapshot);

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::View {
                    can_fork_session: true,
                    can_merge_session_branch: true,
                    can_mutate_session_branch: true,
                    can_open_worktree: true,
                    can_start_staged_session: false,
                    publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
                    session_id: ref session_id_in_mode,
                    session_state: ViewSessionState::Review,
                    scroll_offset: Some(3),
                    ..
                },
                scroll_offset: 0,
            } if session_id_in_mode == &session_id
        ));
    }

    #[tokio::test]
    async fn test_open_publish_branch_input_preserves_view_context() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            scroll_offset: Some(5),
            session_id: session_id.clone().into(),
            session_index: 0,
        };

        // Act
        open_publish_branch_input(
            &mut app,
            &view_context,
            PublishBranchAction::PublishPullRequest,
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                ref default_branch_name,
                input: ref input_state,
                locked_upstream_ref: None,
                publish_branch_action: PublishBranchAction::PublishPullRequest,
                restore_view:
                    ConfirmationViewMode {
                        session_id: ref restored_session_id,
            scroll_offset: Some(5),
                    },
            } if default_branch_name == &crate::app::session::session_branch(&session_id)
                && input_state.cursor == 0
                && input_state.text().is_empty()
                && restored_session_id == &session_id
        ));
    }

    #[tokio::test]
    async fn test_open_publish_branch_input_locks_existing_upstream_branch_name() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].published_upstream_ref =
            Some("origin/review/custom".to_string());
        let view_context = ViewContext {
            scroll_offset: Some(1),
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        open_publish_branch_input(
            &mut app,
            &view_context,
            PublishBranchAction::PublishPullRequest,
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                input: ref input_state,
                locked_upstream_ref: Some(ref upstream_ref),
                ..
            } if upstream_ref == "origin/review/custom"
                && input_state.text() == "review/custom"
        ));
    }

    #[test]
    fn test_view_session_state_maps_merge_queue_statuses() {
        // Arrange
        let merge_queue_statuses = [Status::Queued, Status::Merging];

        // Act
        let mapped_states: Vec<ViewSessionState> = merge_queue_statuses
            .iter()
            .map(|status| help_action::session_view_state(&session_fixture(*status, false)))
            .collect();

        // Assert
        assert!(
            mapped_states
                .iter()
                .all(|state| *state == ViewSessionState::MergeQueue)
        );
    }

    #[test]
    fn test_view_session_state_maps_rebasing_status() {
        // Arrange
        let status = Status::Rebasing;
        let session = session_fixture(status, false);

        // Act
        let state = help_action::session_view_state(&session);

        // Assert
        assert_eq!(state, ViewSessionState::Rebasing);
    }

    #[test]
    fn test_view_session_state_maps_stacked_draft_status() {
        // Arrange
        let session = crate::test_support::SessionFixtureBuilder::new()
            .status(Status::Draft)
            .draft(true)
            .parent_session_id(Some("parent-session".into()))
            .folder(std::env::temp_dir())
            .project_name("")
            .build();

        // Act
        let state = help_action::session_view_state(&session);

        // Assert
        assert_eq!(state, ViewSessionState::StackedDraft);
    }

    #[test]
    fn test_view_session_state_maps_canceled_status() {
        // Arrange
        let status = Status::Canceled;
        let session = session_fixture(status, false);

        // Act
        let state = help_action::session_view_state(&session);

        // Assert
        assert_eq!(state, ViewSessionState::Canceled);
    }
    #[tokio::test]
    async fn test_open_review_output_mode_uses_ready_cache_entry() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let cached_text = "## Review\nCached review from auto-generation.";
        app.review_cache.insert(
            session_id.clone().into(),
            ReviewCacheEntry::Ready {
                diff_hash: 123,
                text: cached_text.to_string(),
            },
        );
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        open_review_output_mode(&mut app, &view_context);

        // Assert
        let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
        assert_eq!(review_status_message, None);
        assert_eq!(review_text, Some(cached_text));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_shows_loading_for_cache_loading_entry() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Claude,
            AgentModel::ClaudeOpus5,
        );
        let review_agent = app.review_agent();
        app.review_cache.insert(
            session_id.clone().into(),
            ReviewCacheEntry::Loading {
                diff_hash: 456,
                review_agent,
            },
        );
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.into(),
            session_index: 0,
        };

        // Act
        open_review_output_mode(&mut app, &view_context);

        // Assert
        let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
        assert_eq!(
            review_status_message,
            Some(review_loading_message(review_agent))
        );
        assert_eq!(review_text, None);
    }

    #[tokio::test]
    async fn test_open_or_regenerate_review_opens_when_review_output_is_missing() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            scroll_offset: Some(5),
            session_id: session_id.into(),
            session_index: 0,
        };
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        open_or_regenerate_review(&mut app, &view_context, &mut pending_update);

        // Assert
        assert_eq!(pending_update.scroll_offset, None);
    }

    #[tokio::test]
    async fn test_open_or_regenerate_shows_confirmation_when_review_output_exists() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.review_cache.insert(
            session_id.clone().into(),
            ReviewCacheEntry::Ready {
                text: "Old review".to_string(),
                diff_hash: 123,
            },
        );
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.clone().into(),
            session_index: 0,
        };
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        open_or_regenerate_review(&mut app, &view_context, &mut pending_update);

        // Assert — confirmation popup is shown instead of direct regeneration
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::RegenerateReview,
                ..
            }
        ));
        // Cache is preserved until user confirms
        assert!(app.review_cache.contains_key(session_id.as_str()));
    }

    #[tokio::test]
    async fn test_open_or_regenerate_skips_when_loading_in_progress() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let review_agent = app.review_agent();
        app.review_cache.insert(
            session_id.clone().into(),
            ReviewCacheEntry::Loading {
                diff_hash: 42,
                review_agent,
            },
        );
        app.mode = AppMode::View {
            scroll_offset: None,
            session_id: session_id.clone().into(),
        };
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.clone().into(),
            session_index: 0,
        };
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        open_or_regenerate_review(&mut app, &view_context, &mut pending_update);

        // Assert — cache and loading state are preserved, no duplicate spawned
        assert!(matches!(
            app.review_cache.get(session_id.as_str()),
            Some(ReviewCacheEntry::Loading { diff_hash: 42, .. })
        ));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                ..
            } if session_id == &view_context.session_id
        ));
    }

    #[tokio::test]
    async fn workflow_diff_and_review_keys_start_background_loads() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            scroll_offset: Some(2),
            session_id: session_id.clone().into(),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let view_session_snapshot = reply_enabled_review_snapshot();
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        let diff_result = handle_workflow_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &view_context,
            &view_session_snapshot,
            &mut pending_update,
        )
        .await;
        let diff_is_loading = matches!(app.mode, AppMode::DiffLoading { .. });
        app.cancel_diff_view_load();
        let review_result = handle_workflow_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            &view_context,
            &view_session_snapshot,
            &mut pending_update,
        )
        .await;

        // Assert
        assert_eq!(diff_result, Some(true));
        assert!(diff_is_loading);
        assert_eq!(review_result, Some(true));
        assert!(matches!(
            app.review_cache.get(&view_context.session_id),
            Some(ReviewCacheEntry::Loading { .. })
        ));
    }

    #[tokio::test]
    async fn stale_view_context_cannot_open_review_comments() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            scroll_offset: None,
            session_id: session_id.into(),
            session_index: usize::MAX,
        };

        // Act
        open_review_comments_in_diff(&mut app, &view_context);

        // Assert
        assert!(!matches!(app.mode, AppMode::DiffLoading { .. }));
    }

    #[tokio::test]
    async fn test_handle_view_key_ignores_diff_for_non_review_status() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = ViewSessionSnapshot {
            branch_actions: ViewActionState::Enabled,
            continue_terminal_session: ViewActionState::Disabled,
            fork_session: ViewActionState::Disabled,
            inspect_diff: ViewActionState::Disabled,
            is_managed: false,
            is_orchestrator: false,
            merge_session_branch: ViewActionState::Enabled,
            mutate_session_branch: ViewActionState::Enabled,
            rebase_session_branch: ViewActionState::Enabled,
            open_worktree: ViewActionState::Disabled,
            reply_to_session: ViewActionState::Enabled,
            review_comments: ViewActionState::Disabled,
            start_staged_session: ViewActionState::Disabled,
            follow_up_task_action: None,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Done,
            session_status: Status::Done,
        };
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            view_key_context,
            &mut pending_update,
        )
        .await;

        // Assert
        assert!(should_apply);
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
            scroll_offset: Some(2),
                ..
            } if session_id == &view_context.session_id
        ));
        assert_eq!(pending_update.scroll_offset, Some(2));
    }

    #[tokio::test]
    async fn test_handle_view_key_uppercase_f_does_not_start_review_when_fork_unavailable() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::Review;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = ViewSessionSnapshot {
            branch_actions: ViewActionState::Enabled,
            continue_terminal_session: ViewActionState::Disabled,
            fork_session: ViewActionState::Disabled,
            inspect_diff: ViewActionState::Enabled,
            is_managed: false,
            is_orchestrator: false,
            merge_session_branch: ViewActionState::Enabled,
            mutate_session_branch: ViewActionState::Enabled,
            rebase_session_branch: ViewActionState::Enabled,
            open_worktree: ViewActionState::Enabled,
            reply_to_session: ViewActionState::Enabled,
            review_comments: ViewActionState::Disabled,
            start_staged_session: ViewActionState::Disabled,
            follow_up_task_action: None,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Review,
            session_status: Status::Review,
        };
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE),
            view_key_context,
            &mut pending_update,
        )
        .await;

        // Assert
        assert!(should_apply);
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(2),
            } if session_id == &view_context.session_id
        ));
        assert!(!app.review_cache.contains_key(session_id.as_str()));
    }

    #[tokio::test]
    async fn test_handle_launch_follow_up_task_key_opens_linked_sibling_session() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let sibling_session_id = app
            .create_session()
            .await
            .expect("failed to create sibling session");
        let source_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("expected source session in session list");
        source_session.follow_up_tasks = vec![crate::domain::session::SessionFollowUpTask {
            id: 1,
            launched_session_id: Some(sibling_session_id.clone().into()),
            position: 0,
            text: "Open the sibling session.".to_string(),
        }];
        app.mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: Some(0),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let result = handle(
            &mut app,
            &mut terminal,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        )
        .await
        .expect("launch/open key should be handled");

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some(sibling_session_id.as_str())
        );
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                ..
            } if session_id == &sibling_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_continue_key_opens_confirmation_for_done_session() {
        // Arrange
        let (mut app, _base_dir, source_session_id) = new_test_app_with_session().await;
        let source_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == source_session_id)
            .expect("expected source session in session list");
        source_session.status = Status::Done;
        source_session.title = Some("Done source".to_string());
        app.mode = AppMode::View {
            session_id: source_session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let result = handle(
            &mut app,
            &mut terminal,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("continue key should be handled");

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ContinueSession,
                ref confirmation_title,
                ref confirmation_message,
                ref restore_view,
                ref session_id,
                ..
            } if confirmation_title == "Confirm Continue"
                && confirmation_message
                    == "Create a new draft session with initial context from this session?"
                && matches!(restore_view, Some(restore_view) if restore_view.session_id == source_session_id)
                && matches!(session_id, Some(session_id) if session_id.as_str() == source_session_id)
        ));
    }

    #[tokio::test]
    async fn test_linked_done_session_routes_c_to_continue_without_comments() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        attach_open_review_request(session);
        session.status = Status::Done;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot =
            view_session_snapshot(&app, &view_context).expect("expected session snapshot");

        // Act
        let uppercase_result = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
            &view_context,
            &view_session_snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert_eq!(uppercase_result, None);
        assert!(!view_session_snapshot.can_open_review_comments());
        assert!(matches!(app.mode, AppMode::View { .. }));

        // Act
        let continue_result = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
            &view_context,
            &view_session_snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert_eq!(continue_result, Some(false));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ContinueSession,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn review_comment_key_opens_diff_from_view() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        attach_open_review_request(session);
        session.status = Status::Review;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot =
            view_session_snapshot(&app, &view_context).expect("expected session snapshot");

        // Act
        let result = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
            &view_context,
            &view_session_snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert_eq!(result, Some(false));
        assert!(matches!(
            app.mode,
            AppMode::DiffLoading {
                ref session_id,
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            } if session_id == &view_context.session_id
        ));
    }

    #[tokio::test]
    async fn campaign_and_managed_worker_keys_route_through_primary_view_actions() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
        let view_context = ViewContext {
            scroll_offset: Some(2),
            session_id: SessionId::from("campaign"),
            session_index: 0,
        };
        let pending_update = ViewPendingUpdate::from_context(&view_context);
        let mut snapshot = reply_enabled_review_snapshot();
        snapshot.is_orchestrator = true;
        let campaign_keys = ['a'];

        // Act
        let mut results = Vec::new();
        for key in campaign_keys {
            results.push(
                handle_primary_view_key(
                    &mut app,
                    KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                    &view_context,
                    &snapshot,
                    &pending_update,
                )
                .await,
            );
        }
        let unknown_campaign_key = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &view_context,
            &snapshot,
            &pending_update,
        )
        .await;
        snapshot.is_orchestrator = false;
        snapshot.is_managed = true;
        let unrelated = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &view_context,
            &snapshot,
            &pending_update,
        )
        .await;
        let direct_cancel = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &view_context,
            &snapshot,
            &pending_update,
        )
        .await;
        let detach = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT),
            &view_context,
            &snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert_eq!(results, vec![Some(true); campaign_keys.len()]);
        assert_eq!(unknown_campaign_key, None);
        assert_eq!(unrelated, None);
        assert_eq!(direct_cancel, Some(false));
        assert_eq!(detach, Some(false));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::DetachManagedSession,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn managed_worktree_open_requires_write_access_confirmation() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
        let view_context = ViewContext {
            scroll_offset: Some(2),
            session_id: SessionId::from("managed-worker"),
            session_index: 0,
        };
        let pending_update = ViewPendingUpdate::from_context(&view_context);
        let mut snapshot = reply_enabled_review_snapshot();
        snapshot.is_managed = true;

        // Act
        let result = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            &view_context,
            &snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert_eq!(result, Some(false));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::OpenManagedWorktree,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: Some(ConfirmationViewMode {
                    scroll_offset: Some(2),
                    ref session_id,
                }),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
                ..
            } if confirmation_title == "Open Managed Worktree"
                && confirmation_message.contains("writable shell")
                && session_id == "managed-worker"
        ));
    }

    #[tokio::test]
    async fn regular_worktree_open_skips_warning_and_opens_selector() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        let view_context = ViewContext {
            scroll_offset: Some(2),
            session_id: SessionId::from("regular-worker"),
            session_index: 0,
        };
        let snapshot = reply_enabled_review_snapshot();

        // Act
        let should_apply_pending_update =
            handle_open_worktree_key(&mut app, &view_context, &snapshot).await;

        // Assert
        assert!(should_apply_pending_update);
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                ref commands,
                restore_view: ConfirmationViewMode {
                    scroll_offset: Some(2),
                    ref session_id,
                },
                selected_command_index: 0,
            } if commands == &["cargo test".to_string(), "npm run dev".to_string()]
                && session_id == "regular-worker"
        ));
    }

    #[tokio::test]
    async fn integration_approval_opens_approach_choice() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.services
            .db()
            .orchestrations()
            .insert_orchestration(
                &session_id,
                &OrchestrationStatus::AwaitingIntegration.to_string(),
                2,
            )
            .await
            .expect("failed to insert orchestration");
        let view_context = ViewContext {
            scroll_offset: Some(2),
            session_id: session_id.clone().into(),
            session_index: 0,
        };
        let pending_update = ViewPendingUpdate::from_context(&view_context);
        let mut snapshot = reply_enabled_review_snapshot();
        snapshot.is_orchestrator = true;

        // Act
        let result = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &view_context,
            &snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert_eq!(result, Some(true));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
                ref confirmation_title,
                restore_view: Some(ConfirmationViewMode {
                    scroll_offset: Some(2),
                    ref session_id,
                }),
                selected_confirmation_index: 0,
                ..
            } if confirmation_title == "Integration Approach" && session_id == &view_context.session_id
        ));
    }

    #[tokio::test]
    async fn test_linked_canceled_session_routes_c_to_continue_without_comments() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        attach_open_review_request(session);
        session.status = Status::Canceled;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot =
            view_session_snapshot(&app, &view_context).expect("expected session snapshot");

        // Act
        let continue_result = handle_primary_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
            &view_context,
            &view_session_snapshot,
            &pending_update,
        )
        .await;

        // Assert
        assert!(!view_session_snapshot.can_open_review_comments());
        assert_eq!(continue_result, Some(false));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ContinueSession,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_continue_key_opens_confirmation_for_canceled_session() {
        // Arrange
        let (mut app, _base_dir, source_session_id) = new_test_app_with_session().await;
        app.sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == source_session_id)
            .expect("expected source session")
            .status = Status::Canceled;
        app.mode = AppMode::View {
            session_id: source_session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let result = handle(
            &mut app,
            &mut terminal,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("c key should be handled");

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ContinueSession,
                ref session_id,
                ..
            } if matches!(session_id, Some(session_id) if session_id.as_str() == source_session_id)
        ));
    }

    #[tokio::test]
    async fn test_handle_view_key_enter_opens_empty_prompt_composer() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = reply_enabled_review_snapshot();
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            view_key_context,
            &mut pending_update,
        )
        .await;

        // Assert
        assert!(should_apply);
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref input,
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if input.is_empty() && session_id == &view_context.session_id
        ));
    }

    #[tokio::test]
    async fn test_slash_prompt_replacement_prevents_saved_prompt_restoration() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        app.save_prompt_progress(PromptModeSnapshot {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::new(Vec::new()),
            input: InputState::with_text("saved reply".to_string()),
            scroll_offset: Some(4),
            session_id: session_id.into(),
            slash_state: PromptSlashState::default(),
        });
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = reply_enabled_review_snapshot();
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            view_key_context,
            &mut pending_update,
        )
        .await;
        let opened_prefilled_slash = matches!(
            app.mode,
            AppMode::Prompt {
                ref input,
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if input.text() == "/"
                && input.cursor == 1
                && session_id == &view_context.session_id
        );
        app.mode = AppMode::View {
            session_id: view_context.session_id.clone(),
            scroll_offset: Some(2),
        };
        switch_view_to_prompt(
            &mut app,
            &view_context,
            PromptHistoryState::new(Vec::new()),
            InputState::default(),
            Some(2),
        )
        .await;

        // Assert
        assert!(should_apply);
        assert!(opened_prefilled_slash);
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref input,
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if input.is_empty()
                && session_id == &view_context.session_id
        ));
        assert!(app.prompt_progress.is_empty());
    }

    #[tokio::test]
    async fn test_handle_view_key_slash_opens_for_reply_enabled_stacked_parent() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let mut view_session_snapshot = reply_enabled_review_snapshot();
        view_session_snapshot.mutate_session_branch = ViewActionState::Disabled;
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            view_key_context,
            &mut pending_update,
        )
        .await;

        // Assert
        assert!(should_apply);
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref input,
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if input.text() == "/"
                && input.cursor == 1
                && session_id == &view_context.session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_view_key_slash_stays_closed_when_mutation_and_reply_are_blocked() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let mut view_session_snapshot = reply_enabled_review_snapshot();
        view_session_snapshot.mutate_session_branch = ViewActionState::Disabled;
        view_session_snapshot.reply_to_session = ViewActionState::Disabled;
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            view_key_context,
            &mut pending_update,
        )
        .await;

        // Assert
        assert!(should_apply);
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(2),
            } if session_id == &view_context.session_id
        ));
        assert_eq!(pending_update.scroll_offset, Some(2));
    }

    #[tokio::test]
    async fn test_switch_view_to_prompt_restores_saved_progress() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        app.save_prompt_progress(PromptModeSnapshot {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::new(vec!["previous".to_string()]),
            input: InputState::with_text("saved reply".to_string()),
            scroll_offset: Some(4),
            session_id: session_id.into(),
            slash_state: PromptSlashState::default(),
        });

        // Act
        switch_view_to_prompt(
            &mut app,
            &view_context,
            PromptHistoryState::new(Vec::new()),
            InputState::default(),
            Some(2),
        )
        .await;

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Prompt {
                focus: ChatFocus::Input,
                history_state,
                input,
                scroll_offset: Some(4),
                ..
            } if history_state.entries == vec!["previous".to_string()]
                && input.text() == "saved reply"
        ));
        assert!(app.prompt_progress.is_empty());
    }

    #[tokio::test]
    async fn test_handle_view_key_p_opens_review_request_publish_input() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = ViewSessionSnapshot {
            branch_actions: ViewActionState::Enabled,
            continue_terminal_session: ViewActionState::Disabled,
            fork_session: ViewActionState::Enabled,
            inspect_diff: ViewActionState::Enabled,
            is_managed: false,
            is_orchestrator: false,
            merge_session_branch: ViewActionState::Enabled,
            mutate_session_branch: ViewActionState::Enabled,
            rebase_session_branch: ViewActionState::Enabled,
            open_worktree: ViewActionState::Enabled,
            reply_to_session: ViewActionState::Enabled,
            review_comments: ViewActionState::Disabled,
            start_staged_session: ViewActionState::Disabled,
            follow_up_task_action: None,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
            session_status: Status::Review,
        };
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
            view_key_context,
            &mut pending_update,
        )
        .await;

        // Assert
        assert!(!should_apply);
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                publish_branch_action: PublishBranchAction::PublishPullRequest,
                ref restore_view,
                ..
            } if restore_view.session_id == session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_view_key_shift_p_opens_review_request_publish_input() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = ViewSessionSnapshot {
            branch_actions: ViewActionState::Enabled,
            continue_terminal_session: ViewActionState::Disabled,
            fork_session: ViewActionState::Enabled,
            inspect_diff: ViewActionState::Enabled,
            is_managed: false,
            is_orchestrator: false,
            merge_session_branch: ViewActionState::Enabled,
            mutate_session_branch: ViewActionState::Enabled,
            rebase_session_branch: ViewActionState::Enabled,
            open_worktree: ViewActionState::Enabled,
            reply_to_session: ViewActionState::Enabled,
            review_comments: ViewActionState::Disabled,
            start_staged_session: ViewActionState::Disabled,
            follow_up_task_action: None,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
            session_status: Status::Review,
        };
        let view_key_context = ViewKeyContext {
            context: &view_context,
            session_snapshot: &view_session_snapshot,
        };

        // Act
        let should_apply = handle_view_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
            view_key_context,
            &mut pending_update,
        )
        .await;

        // Assert
        assert!(!should_apply);
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                publish_branch_action: PublishBranchAction::PublishPullRequest,
                ref restore_view,
                ..
            } if restore_view.session_id == session_id
        ));
    }

    /// Verifies session-view action keys are ignored when the current session
    /// status does not allow those actions.
    #[tokio::test]
    async fn test_handle_view_key_ignores_status_gated_actions() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");

        // Act
        for key in [
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
        ] {
            let mut pending_update = ViewPendingUpdate::from_context(&view_context);
            let view_session_snapshot = ViewSessionSnapshot {
                branch_actions: ViewActionState::Enabled,
                continue_terminal_session: ViewActionState::Disabled,
                fork_session: ViewActionState::Disabled,
                inspect_diff: ViewActionState::Disabled,
                is_managed: false,
                is_orchestrator: false,
                merge_session_branch: ViewActionState::Enabled,
                mutate_session_branch: ViewActionState::Enabled,
                rebase_session_branch: ViewActionState::Enabled,
                open_worktree: ViewActionState::Disabled,
                reply_to_session: ViewActionState::Enabled,
                review_comments: ViewActionState::Disabled,
                start_staged_session: ViewActionState::Disabled,
                follow_up_task_action: None,
                publish_pull_request_action: None,
                session_state: ViewSessionState::Done,
                session_status: Status::Done,
            };
            let view_key_context = ViewKeyContext {
                context: &view_context,
                session_snapshot: &view_session_snapshot,
            };
            let should_apply =
                handle_view_key(&mut app, key, view_key_context, &mut pending_update).await;

            // Assert
            assert!(should_apply);
            assert!(matches!(
                app.mode,
                AppMode::View {
                    ref session_id,
            scroll_offset: Some(2),
                    ..
                } if session_id == &view_context.session_id
            ));
            assert_eq!(pending_update.scroll_offset, Some(2));
        }
    }

    #[tokio::test]
    async fn test_q_always_transitions_to_list() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            scroll_offset: Some(10),
            session_id: session_id.into(),
            session_index: 0,
        };
        let pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        app.mode = AppMode::List;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert_eq!(pending_update.scroll_offset, Some(10));
    }

    #[tokio::test]
    async fn test_end_in_progress_turn_transitions_session_to_review() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
            .await;
        app.sessions.session_handles_mut().insert(
            session_id.clone().into(),
            crate::domain::session::SessionHandles::new(Status::InProgress),
        );

        // Act
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        let handle_status = *app
            .sessions
            .session_handles()
            .get(session_id.as_str())
            .expect("handles missing")
            .status
            .lock()
            .expect("lock failed");
        assert_eq!(handle_status, Status::Review);
    }

    #[tokio::test]
    async fn test_end_in_progress_turn_does_not_send_sigterm_directly() {
        // Arrange — spawn a child and store its PID in the handles.
        // SIGTERM is now sent by the worker's cancellation path, not
        // `end_in_progress_turn`, so the child should remain alive.
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep");
        let child_pid = child.id().expect("child has no pid");

        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
            .await;
        let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
        if let Ok(mut guard) = handles.child_pid.lock() {
            *guard = Some(child_pid);
        }
        app.sessions
            .session_handles_mut()
            .insert(session_id.clone().into(), handles);

        // Act
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert — child is still alive because the UI no longer sends
        // SIGTERM; the worker owns process termination.
        assert!(
            child.try_wait().expect("try_wait failed").is_none(),
            "child should still be running — UI must not send SIGTERM"
        );

        // Cleanup
        child.kill().await.expect("failed to kill child");
    }

    #[tokio::test]
    async fn test_end_in_progress_turn_cancels_token() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
            .await;
        let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
        let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
        app.sessions
            .session_handles_mut()
            .insert(session_id.clone().into(), handles);

        // Act
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert — the token must be cancelled so the worker's `select!`
        // branch fires.
        let is_cancelled = cancel_token
            .lock()
            .expect("cancel token lock")
            .is_cancelled();
        assert!(
            is_cancelled,
            "cancel_token should be cancelled by end_in_progress_turn"
        );
    }

    #[tokio::test]
    async fn test_end_in_progress_turn_keeps_review_session_review_ready() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::Review;
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::Review.to_string(), 0)
            .await;
        app.sessions.session_handles_mut().insert(
            session_id.clone().into(),
            crate::domain::session::SessionHandles::new(Status::Review),
        );

        // Act
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        let handle_status = *app
            .sessions
            .session_handles()
            .get(session_id.as_str())
            .expect("handles missing")
            .status
            .lock()
            .expect("lock failed");
        assert_eq!(handle_status, Status::Review);
    }

    #[tokio::test]
    async fn test_end_in_progress_turn_first_press_with_queue_pops_last_queued_message() {
        // Arrange — seed an InProgress session with two queued chat messages
        // so the LIFO pop is observable (the older entry must remain).
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
            .await;
        let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
        let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
        let queued_messages = std::sync::Arc::clone(&handles.queued_messages);
        {
            let mut queued = queued_messages.lock().expect("queued_messages lock");
            queued.push_back(queued_message(0, "first queued"));
            queued.push_back(queued_message(1, "second queued"));
        }
        app.sessions
            .session_handles_mut()
            .insert(session_id.clone().into(), handles);
        app.sessions.sessions_mut()[0].queued_messages = vec![
            queued_message(0, "first queued"),
            queued_message(1, "second queued"),
        ];
        let mut line_comments = DiffLineComments::default();
        line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
        app.save_diff_comment_progress(session_id.clone().into(), line_comments);
        let saved_line_comments = app.diff_comment_progress[session_id.as_str()].clone();

        // Act — first Ctrl+C while the queue is non-empty.
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert — the most recently queued message is popped (LIFO), the
        // older message remains, status stays InProgress, and the cancel
        // token is untouched so the running turn keeps streaming.
        let remaining_handle_queue: Vec<String> = queued_messages
            .lock()
            .expect("queued_messages lock")
            .iter()
            .map(|message| message.transcript_text().to_string())
            .collect();
        assert_eq!(
            remaining_handle_queue,
            vec!["first queued".to_string()],
            "only the most recently queued chat message should be popped on first Ctrl+C"
        );
        assert_eq!(
            app.sessions.sessions()[0].queued_messages[0].transcript_text(),
            "first queued",
            "snapshot queued_messages should mirror the handle after LIFO pop"
        );
        assert_eq!(
            app.sessions.sessions()[0].status,
            Status::InProgress,
            "status should stay InProgress while only a queued message is popped"
        );
        let handle_status = *app
            .sessions
            .session_handles()
            .get(session_id.as_str())
            .expect("handles missing")
            .status
            .lock()
            .expect("lock failed");
        assert_eq!(handle_status, Status::InProgress);
        assert!(
            !cancel_token
                .lock()
                .expect("cancel token lock")
                .is_cancelled(),
            "cancel_token must not be cancelled when only a queued message is popped"
        );
        assert_eq!(
            app.diff_comment_progress.get(session_id.as_str()),
            Some(&saved_line_comments),
            "retracting a queued message must preserve its diff comments"
        );
    }

    #[tokio::test]
    async fn test_end_in_progress_turn_drains_queue_one_press_at_a_time_then_cancels() {
        // Arrange — InProgress session with two queued chat messages so we
        // can observe LIFO drain across consecutive presses before falling
        // through to the cancel path.
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
            .await;
        let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
        let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
        let queued_messages = std::sync::Arc::clone(&handles.queued_messages);
        {
            let mut queued = queued_messages.lock().expect("queued_messages lock");
            queued.push_back(queued_message(0, "first queued"));
            queued.push_back(queued_message(1, "second queued"));
        }
        app.sessions
            .session_handles_mut()
            .insert(session_id.clone().into(), handles);
        app.sessions.sessions_mut()[0].queued_messages = vec![
            queued_message(0, "first queued"),
            queued_message(1, "second queued"),
        ];

        // Act — first press pops "second queued".
        end_in_progress_turn(&mut app, &session_id).await;
        // Act — second press pops "first queued".
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert — queue is empty after two presses, but the running turn
        // still has not been cancelled yet.
        assert!(
            queued_messages
                .lock()
                .expect("queued_messages lock")
                .is_empty(),
            "queue should be drained after one press per queued message"
        );
        assert!(
            app.sessions.sessions()[0].queued_messages.is_empty(),
            "snapshot queued_messages should be empty after LIFO drain"
        );
        assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
        assert!(
            !cancel_token
                .lock()
                .expect("cancel token lock")
                .is_cancelled(),
            "cancel_token must not be cancelled while queued messages are still being drained"
        );

        // Act — third press, with empty queue, falls through to cancel.
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert — cancel path engages and the session returns to Review.
        assert!(
            cancel_token
                .lock()
                .expect("cancel token lock")
                .is_cancelled(),
            "cancel_token must be cancelled once the queue is drained"
        );
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    }

    #[tokio::test]
    async fn test_end_in_progress_turn_second_press_after_empty_queue_cancels_turn() {
        // Arrange — InProgress session with an empty queue, mirroring the
        // state after the first press has already drained queued messages.
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        app.sessions.sessions_mut()[0]
            .transient_messages
            .upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Loading("Resolving 2 review comments...".to_string()),
                lifecycle: TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::ReviewCommentResolution,
                turn_position: None,
            });
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
            .await;
        let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
        let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
        app.sessions
            .session_handles_mut()
            .insert(session_id.clone().into(), handles);

        // Act — second Ctrl+C now that the queue is empty.
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert — falls through to the cancel path: cancel token fires and
        // the session transitions to Review.
        assert!(
            cancel_token
                .lock()
                .expect("cancel token lock")
                .is_cancelled(),
            "cancel_token must be cancelled when the queue is empty"
        );
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        let handle_status = *app
            .sessions
            .session_handles()
            .get(session_id.as_str())
            .expect("handles missing")
            .status
            .lock()
            .expect("lock failed");
        assert_eq!(handle_status, Status::Review);
        assert!(
            app.sessions.sessions()[0]
                .transient_messages
                .get(TransientMessageSlot::ReviewCommentResolution)
                .is_none()
        );
    }

    #[tokio::test]
    /// Regression: when the worker has already drained the oldest queued
    /// prompt via `pop_front` but the deferred `RefreshSessions` reducer
    /// has not yet rebuilt the snapshot, a `Ctrl+C` press must still align
    /// the snapshot with the current handle state instead of removing the
    /// snapshot's last entry positionally and leaving a phantom queued row.
    async fn test_pop_last_queued_chat_message_resyncs_snapshot_after_worker_drain() {
        // Arrange — two prompts queued; simulate the worker popping the
        // oldest entry off the handle without yet refreshing the snapshot.
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let _ = app
            .services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
            .await;
        let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
        let queued_messages = std::sync::Arc::clone(&handles.queued_messages);
        {
            let mut queued = queued_messages.lock().expect("queued_messages lock");
            queued.push_back(queued_message(0, "first queued"));
            queued.push_back(queued_message(1, "second queued"));
        }
        app.sessions
            .session_handles_mut()
            .insert(session_id.clone().into(), handles);
        app.sessions.sessions_mut()[0].queued_messages = vec![
            queued_message(0, "first queued"),
            queued_message(1, "second queued"),
        ];

        // Simulate the worker `pop_front` draining the oldest entry before
        // the snapshot has been refreshed.
        {
            let mut queued = queued_messages.lock().expect("queued_messages lock");
            queued.pop_front();
        }

        // Act — Ctrl+C while the handle has [second] but the snapshot still
        // reads [first, second].
        end_in_progress_turn(&mut app, &session_id).await;

        // Assert — handle is now empty (the user retracted "second"), and
        // the snapshot reflects the post-pop handle state instead of
        // positionally dropping the snapshot's last entry (which would
        // leave a phantom "first queued" row pointing at a turn the worker
        // is already running).
        assert!(
            queued_messages
                .lock()
                .expect("queued_messages lock")
                .is_empty(),
            "handle queue should be empty after retracting the only remaining entry"
        );
        assert!(
            app.sessions.sessions()[0].queued_messages.is_empty(),
            "snapshot must rebuild from the handle state and not show a phantom row for a prompt \
             the worker is already executing"
        );
    }

    #[tokio::test]
    async fn test_scroll_keys_bypass_action_snapshot_and_keep_navigation_working() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let session = crate::test_support::SessionFixtureBuilder::new()
            .status(Status::Review)
            .build();
        let session_id = session.id.clone();
        app.sessions.push_session(session);
        app.sessions.sessions_mut()[0].transcript =
            Some(SessionTranscript::new(vec![SessionMessage::conversation(
                0,
                SessionMessageKind::AssistantAnswer,
                "```mermaid\ngraph TD\nA --> B\n```\n".repeat(100),
            )]));
        app.mode = AppMode::View {
            session_id,
            scroll_offset: Some(0),
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("terminal");
        let cache = RenderCacheStore::default();

        // Act
        for key in ['j', 'j', 'k'] {
            handle_with_cache(
                &mut app,
                &cache,
                &mut terminal,
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
            )
            .await
            .expect("scroll");
        }

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                scroll_offset: Some(1),
                ..
            }
        ));
        handle_with_cache(
            &mut app,
            &cache,
            &mut terminal,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await
        .expect("leave session");
        assert!(matches!(app.mode, AppMode::List));
    }
}
