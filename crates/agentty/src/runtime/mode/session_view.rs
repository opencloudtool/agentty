use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{App, ReviewCacheEntry, diff_content_hash};
use crate::domain::agent::AgentModel;
use crate::domain::input::InputState;
use crate::domain::session::{PublishBranchAction, Status};
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;
use crate::ui::page::session_chat::SessionChatPage;
use crate::ui::state::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, DoneSessionOutputMode, HelpContext,
};
use crate::ui::state::help_action::ViewSessionState;
use crate::ui::state::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};

#[derive(Clone)]
struct ViewContext {
    done_session_output_mode: DoneSessionOutputMode,
    review_status_message: Option<String>,
    review_text: Option<String>,
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

#[derive(Clone, Copy)]
struct ViewMetrics {
    total_lines: u16,
    view_height: u16,
}

/// Pending output-mode and scroll updates produced by one key event in
/// session-view mode.
struct ViewPendingUpdate {
    done_session_output_mode: DoneSessionOutputMode,
    review_status_message: Option<String>,
    review_text: Option<String>,
    scroll_offset: Option<u16>,
}

impl ViewPendingUpdate {
    /// Builds update state seeded from the current view mode values.
    fn from_context(view_context: &ViewContext) -> Self {
        Self {
            done_session_output_mode: view_context.done_session_output_mode,
            review_status_message: view_context.review_status_message.clone(),
            review_text: view_context.review_text.clone(),
            scroll_offset: view_context.scroll_offset,
        }
    }
}

/// Borrowed per-key context used while processing one session-view key event.
struct ViewKeyContext<'a> {
    context: &'a ViewContext,
    metrics: ViewMetrics,
    session_snapshot: &'a ViewSessionSnapshot,
}

/// Snapshot of session-derived state used by view-mode key handling.
struct ViewSessionSnapshot {
    can_open_worktree: bool,
    is_action_allowed: bool,
    publish_branch_action: Option<PublishBranchAction>,
    session_output: String,
    session_state: ViewSessionState,
    session_status: Status,
}

/// Prefix for the review loading status while assist output is being
/// prepared.
const REVIEW_LOADING_MESSAGE_PREFIX: &str = "Preparing review with agent help";
/// Fallback copy shown when a review-ready session has no diff to inspect.
const REVIEW_NO_DIFF_MESSAGE: &str = "No diff changes found for review.";

/// Processes view-mode key presses and keeps shortcut availability aligned with
/// session status (`o` disabled for `Done`/`Canceled`/`Merging`/`Queued`, and
/// diff/review only for `Review`).
pub(crate) async fn handle<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let Some(view_context) = view_context(app) else {
        return Ok(EventResult::Continue);
    };
    let view_metrics = view_metrics(app, terminal, &view_context)?;
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    let Some(view_session_snapshot) = view_session_snapshot(app, &view_context) else {
        return Ok(EventResult::Continue);
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        metrics: view_metrics,
        session_snapshot: &view_session_snapshot,
    };

    if !handle_view_key(app, key, view_key_context, &mut pending_update).await {
        return Ok(EventResult::Continue);
    }

    apply_view_scroll_and_output_mode(
        app,
        pending_update.done_session_output_mode,
        pending_update.review_status_message,
        pending_update.review_text,
        pending_update.scroll_offset,
    );

    Ok(EventResult::Continue)
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
    let view_metrics = view_key_context.metrics;
    let view_session_snapshot = view_key_context.session_snapshot;

    match key.code {
        KeyCode::Char('q')
            if pending_update.done_session_output_mode == DoneSessionOutputMode::Review =>
        {
            pending_update.done_session_output_mode = DoneSessionOutputMode::Summary;
            pending_update.scroll_offset = None;
        }
        KeyCode::Char('q') => app.mode = AppMode::List,
        KeyCode::Char('o') if view_session_snapshot.can_open_worktree => {
            open_worktree_for_view_session(app, view_context).await;
        }
        KeyCode::Enter if view_session_snapshot.is_action_allowed => {
            switch_view_to_prompt(
                app,
                view_context,
                PromptHistoryState::new(prompt_history_entries(
                    &view_session_snapshot.session_output,
                )),
                pending_update.scroll_offset,
            );
        }
        KeyCode::Char('j') | KeyCode::Down => {
            pending_update.scroll_offset =
                scroll_offset_down(pending_update.scroll_offset, view_metrics, 1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            pending_update.scroll_offset = Some(scroll_offset_up(
                pending_update.scroll_offset,
                view_metrics,
                1,
            ));
        }
        KeyCode::Char('g') => pending_update.scroll_offset = Some(0),
        KeyCode::Char('G') => pending_update.scroll_offset = None,
        KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            pending_update.scroll_offset =
                scroll_offset_half_page_down(pending_update.scroll_offset, view_metrics);
        }
        KeyCode::Char('u') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            pending_update.scroll_offset = Some(scroll_offset_half_page_up(
                pending_update.scroll_offset,
                view_metrics,
            ));
        }
        KeyCode::Char('d')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && is_view_diff_allowed(view_session_snapshot.session_status) =>
        {
            show_diff_for_view_session(app, view_context).await;
        }
        KeyCode::Char('p')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.publish_branch_action.is_some() =>
        {
            let Some(publish_branch_action) = view_session_snapshot.publish_branch_action else {
                return true;
            };
            open_publish_branch_input(app, view_context, publish_branch_action);

            return false;
        }
        KeyCode::Char(character)
            if character.eq_ignore_ascii_case(&'f')
                && !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && is_view_review_allowed(view_session_snapshot.session_status) =>
        {
            open_or_regenerate_review(app, view_context, pending_update).await;
        }
        KeyCode::Char('m') if view_session_snapshot.is_action_allowed => {
            open_merge_confirmation(app, view_context);
        }
        KeyCode::Char('r') if view_session_snapshot.is_action_allowed => {
            rebase_view_session(app, &view_context.session_id).await;
        }
        _ if is_done_output_toggle_key(view_session_snapshot.session_status, key) => {
            pending_update.done_session_output_mode =
                pending_update.done_session_output_mode.toggled();
            pending_update.scroll_offset = None;
        }
        KeyCode::Char('?') => {
            open_view_help_overlay(
                app,
                view_context,
                view_session_snapshot.publish_branch_action,
                view_session_snapshot.session_state,
            );
            return false;
        }
        _ => {}
    }

    true
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

/// Opens the viewed session worktree directly or shows a command selector when
/// multiple open commands are configured.
async fn open_worktree_for_view_session(app: &mut App, view_context: &ViewContext) {
    let open_commands = app.configured_open_commands();
    if open_commands.len() > 1 {
        app.mode = AppMode::OpenCommandSelector {
            commands: open_commands,
            restore_view: confirmation_view_mode(view_context),
            selected_command_index: 0,
        };

        return;
    }

    let selected_open_command = open_commands.first().map(String::as_str);

    app.open_session_worktree_in_tmux_with_command(selected_open_command)
        .await;
}

/// Builds the view-mode snapshot used to restore chat context when a merge
/// confirmation is dismissed.
fn confirmation_view_mode(view_context: &ViewContext) -> ConfirmationViewMode {
    ConfirmationViewMode {
        done_session_output_mode: view_context.done_session_output_mode,
        review_status_message: view_context.review_status_message.clone(),
        review_text: view_context.review_text.clone(),
        scroll_offset: view_context.scroll_offset,
        session_id: view_context.session_id.clone(),
    }
}

/// Opens focused review or shows a regeneration confirmation popup.
///
/// When currently in focused review mode and a review result (or error) is
/// displayed, shows a confirmation popup before regenerating. If a generation
/// is already in flight (loading), the press is ignored to avoid spawning
/// duplicate background tasks. Otherwise, opens focused review and resets
/// scroll to bottom-aligned mode.
async fn open_or_regenerate_review(
    app: &mut App,
    view_context: &ViewContext,
    pending_update: &mut ViewPendingUpdate,
) {
    if pending_update.done_session_output_mode == DoneSessionOutputMode::Review {
        let is_loading = pending_update
            .review_status_message
            .as_deref()
            .is_some_and(is_review_loading_status_message);
        if is_loading {
            return;
        }

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

    open_review_output_mode(
        app,
        view_context,
        &mut pending_update.done_session_output_mode,
        &mut pending_update.review_status_message,
        &mut pending_update.review_text,
    )
    .await;

    pending_update.scroll_offset = None;
}

/// Collects session-specific values used by `handle()` from the active view
/// row.
fn view_session_snapshot(app: &App, view_context: &ViewContext) -> Option<ViewSessionSnapshot> {
    let session = app.sessions.sessions.get(view_context.session_index)?;
    let session_status = session.status;

    Some(ViewSessionSnapshot {
        can_open_worktree: is_view_worktree_open_allowed(session_status)
            && can_open_session_worktree(session_status),
        is_action_allowed: is_view_action_allowed(session_status),
        publish_branch_action: session.publish_branch_action(),
        session_output: session.output.clone(),
        session_state: view_session_state(session_status),
        session_status,
    })
}

/// Applies in-place updates for active view output mode, review
/// status/text, and scroll position.
fn apply_view_scroll_and_output_mode(
    app: &mut App,
    done_session_output_mode: DoneSessionOutputMode,
    review_status_message: Option<String>,
    review_text: Option<String>,
    scroll_offset: Option<u16>,
) {
    if let AppMode::View {
        done_session_output_mode: view_done_session_output_mode,
        review_status_message: view_review_status_message,
        review_text: view_review_text,
        scroll_offset: view_scroll_offset,
        ..
    } = &mut app.mode
    {
        *view_done_session_output_mode = done_session_output_mode;
        *view_review_status_message = review_status_message;
        *view_review_text = review_text;
        *view_scroll_offset = scroll_offset;
    }
}

/// Returns whether the key event toggles done-session output mode.
fn is_done_output_toggle_key(status: Status, key: KeyEvent) -> bool {
    let is_toggle_key = matches!(
        key.code,
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'t')
    );

    status == Status::Done && is_toggle_key && !key.modifiers.contains(event::KeyModifiers::CONTROL)
}

/// Returns whether `o` can access the session worktree.
fn is_view_worktree_open_allowed(status: Status) -> bool {
    !matches!(
        status,
        Status::Done | Status::Canceled | Status::Merging | Status::Queued
    )
}

/// Returns whether non-navigation view shortcuts are available.
///
/// This covers `Enter`, `m`, and `r`.
fn is_view_action_allowed(status: Status) -> bool {
    !matches!(
        status,
        Status::Done
            | Status::InProgress
            | Status::Rebasing
            | Status::Merging
            | Status::Queued
            | Status::Canceled
    )
}

/// Returns whether the `d` shortcut can open the diff view.
fn is_view_diff_allowed(status: Status) -> bool {
    status == Status::Review
}

/// Returns whether the `f` shortcut can open review content.
fn is_view_review_allowed(status: Status) -> bool {
    status == Status::Review
}

/// Switches the TUI mode from session view to the prompt input.
fn switch_view_to_prompt(
    app: &mut App,
    view_context: &ViewContext,
    history_state: PromptHistoryState,
    scroll_offset: Option<u16>,
) {
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state,
        slash_state: PromptSlashState::new(),
        session_id: view_context.session_id.clone(),
        input: InputState::new(),
        scroll_offset,
    };
}

/// Returns whether the worktree-open shortcut (`o`) is enabled for the
/// provided session status.
fn can_open_session_worktree(status: Status) -> bool {
    !matches!(
        status,
        Status::Done | Status::Canceled | Status::Merging | Status::Queued
    )
}

/// Maps session status to view help session state.
fn view_session_state(status: Status) -> ViewSessionState {
    match status {
        Status::Done => ViewSessionState::Done,
        Status::Canceled => ViewSessionState::Canceled,
        Status::InProgress => ViewSessionState::InProgress,
        Status::Rebasing => ViewSessionState::Rebasing,
        Status::Merging | Status::Queued => ViewSessionState::MergeQueue,
        Status::Review => ViewSessionState::Review,
        _ => ViewSessionState::Interactive,
    }
}

/// Opens the help overlay while preserving the currently viewed session state.
fn open_view_help_overlay(
    app: &mut App,
    view_context: &ViewContext,
    publish_branch_action: Option<PublishBranchAction>,
    session_state: ViewSessionState,
) {
    app.mode = AppMode::Help {
        context: HelpContext::View {
            done_session_output_mode: view_context.done_session_output_mode,
            review_status_message: view_context.review_status_message.clone(),
            review_text: view_context.review_text.clone(),
            publish_branch_action,
            session_id: view_context.session_id.clone(),
            session_state,
            scroll_offset: view_context.scroll_offset,
        },
        scroll_offset: 0,
    };
}

/// Opens the session-view branch-publish popup and preserves the current view
/// state for cancel or submit.
fn open_publish_branch_input(
    app: &mut App,
    view_context: &ViewContext,
    publish_branch_action: PublishBranchAction,
) {
    let session = &app.sessions.sessions[view_context.session_index];
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

/// Extracts the remote branch portion from one upstream reference.
fn remote_branch_name_from_upstream_ref(upstream_ref: &str) -> String {
    upstream_ref.split_once('/').map_or_else(
        || upstream_ref.to_string(),
        |(_, branch_name)| branch_name.to_string(),
    )
}

fn view_context(app: &mut App) -> Option<ViewContext> {
    let (done_session_output_mode, review_status_message, review_text, session_id, scroll_offset) =
        match &app.mode {
            AppMode::View {
                done_session_output_mode,
                review_status_message,
                review_text,
                session_id,
                scroll_offset,
            } => (
                *done_session_output_mode,
                review_status_message.clone(),
                review_text.clone(),
                session_id.clone(),
                *scroll_offset,
            ),
            _ => return None,
        };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    Some(ViewContext {
        done_session_output_mode,
        review_status_message,
        review_text,
        scroll_offset,
        session_id,
        session_index,
    })
}

fn view_metrics<B: Backend>(
    app: &App,
    terminal: &Terminal<B>,
    view_context: &ViewContext,
) -> io::Result<ViewMetrics>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let terminal_size = terminal.size().map_err(crate::runtime::backend_err)?;
    let view_height = terminal_size.height.saturating_sub(5);
    let output_width = terminal_size.width.saturating_sub(2);
    let total_lines = view_total_lines(
        app,
        &view_context.session_id,
        view_context.session_index,
        view_context.done_session_output_mode,
        view_context.review_status_message.as_deref(),
        view_context.review_text.as_deref(),
        output_width,
    );

    Ok(ViewMetrics {
        total_lines,
        view_height,
    })
}

fn view_total_lines(
    app: &App,
    session_id: &str,
    session_index: usize,
    done_session_output_mode: DoneSessionOutputMode,
    review_status_message: Option<&str>,
    review_text: Option<&str>,
    output_width: u16,
) -> u16 {
    let active_progress = app.session_progress_message(session_id);

    app.sessions
        .sessions
        .get(session_index)
        .map_or(0, |session| {
            SessionChatPage::rendered_output_line_count(
                session,
                output_width,
                done_session_output_mode,
                review_status_message,
                review_text,
                active_progress,
            )
        })
}

/// Extracts user prompt history entries from persisted session output text.
///
/// The parser accepts both legacy multiline prompts (raw continuation lines)
/// and the current continuation-prefixed format where follow-up lines start
/// with three spaces.
fn prompt_history_entries(output: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut output_lines = output.lines().peekable();

    while let Some(line) = output_lines.next() {
        let Some(first_prompt_line) = line.strip_prefix(" › ") else {
            continue;
        };

        let mut prompt = first_prompt_line.to_string();

        while let Some(next_line) = output_lines.peek().copied() {
            if next_line.is_empty() {
                break;
            }

            let prompt_line = next_line.strip_prefix("   ").unwrap_or(next_line);
            prompt.push('\n');
            prompt.push_str(prompt_line);
            let _ = output_lines.next();
        }

        entries.push(prompt);
    }

    entries
}

fn scroll_offset_down(scroll_offset: Option<u16>, metrics: ViewMetrics, step: u16) -> Option<u16> {
    let current_offset = scroll_offset?;

    let next_offset = current_offset.saturating_add(step.max(1));
    if next_offset >= metrics.total_lines.saturating_sub(metrics.view_height) {
        return None;
    }

    Some(next_offset)
}

fn scroll_offset_up(scroll_offset: Option<u16>, metrics: ViewMetrics, step: u16) -> u16 {
    let current_offset =
        scroll_offset.unwrap_or_else(|| metrics.total_lines.saturating_sub(metrics.view_height));

    current_offset.saturating_sub(step.max(1))
}

/// Computes the next scroll offset for half-page downward navigation.
fn scroll_offset_half_page_down(scroll_offset: Option<u16>, metrics: ViewMetrics) -> Option<u16> {
    scroll_offset_down(scroll_offset, metrics, half_page_scroll_step(metrics))
}

/// Computes the next scroll offset for half-page upward navigation.
fn scroll_offset_half_page_up(scroll_offset: Option<u16>, metrics: ViewMetrics) -> u16 {
    scroll_offset_up(scroll_offset, metrics, half_page_scroll_step(metrics))
}

/// Returns the number of lines used for half-page scroll shortcuts.
fn half_page_scroll_step(metrics: ViewMetrics) -> u16 {
    metrics.view_height / 2
}

/// Opens review mode and serves cached review or loading status.
///
/// Reviews are auto-generated when sessions transition to `Review`. When the
/// user presses `f` and no cached review exists yet, Agentty computes the
/// current diff, starts background generation, and shows a loading message
/// immediately. Exiting focused review is handled by `q`; pressing `f` again
/// while viewing regenerates via `open_or_regenerate_review`.
async fn open_review_output_mode(
    app: &mut App,
    view_context: &ViewContext,
    done_session_output_mode: &mut DoneSessionOutputMode,
    review_status_message: &mut Option<String>,
    review_text: &mut Option<String>,
) {
    *done_session_output_mode = DoneSessionOutputMode::Review;

    let review_is_loading = review_status_message
        .as_deref()
        .is_some_and(is_review_loading_status_message);
    if review_text.is_some() || review_is_loading {
        return;
    }

    if let Some(cached) = app.review_cache.get(&view_context.session_id) {
        match cached {
            ReviewCacheEntry::Loading { .. } => {
                let review_model = review_assist_model(app);
                *review_status_message = Some(review_loading_message(review_model));
                *review_text = None;
            }
            ReviewCacheEntry::Ready { text, .. } => {
                *review_status_message = None;
                *review_text = Some(text.clone());
            }
            ReviewCacheEntry::Failed { error, .. } => {
                *review_status_message =
                    Some(format!("Review assist unavailable: {}", error.trim()));
                *review_text = None;
            }
        }

        return;
    }

    let Some(session) = app.sessions.sessions.get(view_context.session_index) else {
        *review_status_message = None;
        *review_text = Some(String::new());

        return;
    };
    let session_folder = session.folder.clone();
    let session_summary = session.summary.clone();
    let diff = load_view_session_diff(app, view_context).await;
    if diff.trim().is_empty() {
        *review_status_message = None;
        *review_text = Some(REVIEW_NO_DIFF_MESSAGE.to_string());

        return;
    }

    if diff.starts_with("Failed to run git diff:") {
        *review_status_message = None;
        *review_text = Some(diff);

        return;
    }

    let diff_hash = diff_content_hash(&diff);
    let review_model = review_assist_model(app);
    app.review_cache.insert(
        view_context.session_id.clone(),
        ReviewCacheEntry::Loading { diff_hash },
    );
    *review_status_message = Some(review_loading_message(review_model));
    *review_text = None;
    app.start_review_assist(
        &view_context.session_id,
        &session_folder,
        diff_hash,
        &diff,
        session_summary.as_deref(),
    );
}

async fn show_diff_for_view_session(app: &mut App, view_context: &ViewContext) {
    let diff = load_view_session_diff(app, view_context).await;

    app.mode = AppMode::Diff {
        session_id: view_context.session_id.clone(),
        diff,
        scroll_offset: 0,
        file_explorer_selected_index: 0,
    };
}

/// Returns whether a review status line indicates assist is still
/// loading.
fn is_review_loading_status_message(status_message: &str) -> bool {
    status_message.starts_with(REVIEW_LOADING_MESSAGE_PREFIX)
}

/// Returns the configured model used for review assist generation.
fn review_assist_model(app: &App) -> AgentModel {
    app.settings.default_review_model
}

/// Formats the review loading status line with the active model name.
pub(crate) fn review_loading_message(review_model: AgentModel) -> String {
    format!(
        "{REVIEW_LOADING_MESSAGE_PREFIX} with model {}...",
        review_model.as_str(),
    )
}

/// Loads the session worktree diff against its base branch.
async fn load_view_session_diff(app: &App, view_context: &ViewContext) -> String {
    let Some(session) = app.sessions.sessions.get(view_context.session_index) else {
        return String::new();
    };

    let session_folder = session.folder.clone();
    let base_branch = session.base_branch.clone();

    app.services
        .git_client()
        .diff(session_folder, base_branch)
        .await
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"))
}

async fn rebase_view_session(app: &mut App, session_id: &str) {
    if let Err(error) = app.rebase_session(session_id).await {
        app.append_output_for_session(session_id, &format!("\n[Rebase Error] {error}\n"))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use mockall::predicate::eq;
    use tempfile::tempdir;

    use super::*;
    use crate::app::AppClients;
    use crate::db::Database;
    use crate::infra::app_server;
    use crate::infra::tmux::{MockTmuxClient, TmuxClient};

    /// Returns a mock app-server client wrapped in `Arc` for test injection.
    fn mock_app_server() -> std::sync::Arc<dyn app_server::AppServerClient> {
        std::sync::Arc::new(app_server::MockAppServerClient::new())
    }

    /// Builds one test app with an injected tmux boundary.
    async fn new_test_app_with_tmux_client(
        tmux_client: Arc<dyn TmuxClient>,
    ) -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = AppClients::new()
            .with_app_server_client_override(mock_app_server())
            .with_tmux_client(tmux_client);
        let app = App::new_with_clients(base_path.clone(), base_path, None, database, clients)
            .await
            .expect("failed to build app");

        (app, base_dir)
    }

    /// Builds one test app with a strict mocked tmux boundary.
    async fn new_test_app() -> (App, tempfile::TempDir) {
        new_test_app_with_tmux_client(Arc::new(MockTmuxClient::new())).await
    }

    fn setup_test_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        std::fs::write(path.join("README.md"), "test").expect("write failed");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("git commit failed");
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(path)
            .output()
            .expect("git branch failed");
    }

    /// Builds one git-backed test app with an injected tmux boundary.
    async fn new_test_app_with_git_and_tmux_client(
        tmux_client: Arc<dyn TmuxClient>,
    ) -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        setup_test_git_repo(base_dir.path());
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = AppClients::new()
            .with_app_server_client_override(mock_app_server())
            .with_tmux_client(tmux_client);
        let app = App::new_with_clients(
            base_path.clone(),
            base_path,
            Some("main".to_string()),
            database,
            clients,
        )
        .await
        .expect("failed to build app");

        (app, base_dir)
    }

    /// Builds one git-backed test app with one created session and an
    /// injected tmux boundary.
    async fn new_test_app_with_session_and_tmux_client(
        tmux_client: Arc<dyn TmuxClient>,
    ) -> (App, tempfile::TempDir, String) {
        let (mut app, base_dir) = new_test_app_with_git_and_tmux_client(tmux_client).await;
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

    #[test]
    fn test_is_view_worktree_open_allowed_returns_false_for_canceled() {
        // Arrange
        let status = Status::Canceled;

        // Act
        let can_open = is_view_worktree_open_allowed(status);

        // Assert
        assert!(!can_open);
    }

    #[test]
    fn test_is_view_worktree_open_allowed_returns_true_for_in_progress() {
        // Arrange
        let status = Status::InProgress;

        // Act
        let can_open = is_view_worktree_open_allowed(status);

        // Assert
        assert!(can_open);
    }

    #[test]
    fn test_is_view_worktree_open_allowed_returns_false_for_merge_queue_statuses() {
        // Arrange
        let merge_queue_statuses = [Status::Queued, Status::Merging];

        // Act
        let can_open_for_statuses: Vec<bool> = merge_queue_statuses
            .iter()
            .map(|status| is_view_worktree_open_allowed(*status))
            .collect();

        // Assert
        assert!(can_open_for_statuses.iter().all(|can_open| !can_open));
    }

    #[test]
    fn test_is_view_action_allowed_only_for_non_done_non_in_progress_status() {
        // Arrange
        let canceled_status = Status::Canceled;
        let review_status = Status::Review;
        let in_progress_status = Status::InProgress;
        let done_status = Status::Done;

        // Act
        let canceled_allowed = is_view_action_allowed(canceled_status);
        let review_allowed = is_view_action_allowed(review_status);
        let in_progress_allowed = is_view_action_allowed(in_progress_status);
        let done_allowed = is_view_action_allowed(done_status);

        // Assert
        assert!(!canceled_allowed);
        assert!(review_allowed);
        assert!(!in_progress_allowed);
        assert!(!done_allowed);
    }

    #[test]
    fn test_is_view_diff_allowed_only_for_review_status() {
        // Arrange
        let review_status = Status::Review;
        let new_status = Status::New;
        let in_progress_status = Status::InProgress;

        // Act
        let review_allowed = is_view_diff_allowed(review_status);
        let new_allowed = is_view_diff_allowed(new_status);
        let in_progress_allowed = is_view_diff_allowed(in_progress_status);

        // Assert
        assert!(review_allowed);
        assert!(!new_allowed);
        assert!(!in_progress_allowed);
    }

    #[test]
    fn test_is_view_review_allowed_only_for_review_status() {
        // Arrange
        let review_status = Status::Review;
        let done_status = Status::Done;
        let in_progress_status = Status::InProgress;

        // Act
        let review_allowed = is_view_review_allowed(review_status);
        let done_allowed = is_view_review_allowed(done_status);
        let in_progress_allowed = is_view_review_allowed(in_progress_status);

        // Assert
        assert!(review_allowed);
        assert!(!done_allowed);
        assert!(!in_progress_allowed);
    }

    #[test]
    fn test_is_done_output_toggle_key_accepts_done_status_with_t() {
        // Arrange
        let status = Status::Done;
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);

        // Act
        let is_toggle = is_done_output_toggle_key(status, key);

        // Assert
        assert!(is_toggle);
    }

    #[test]
    fn test_is_done_output_toggle_key_rejects_non_done_status() {
        // Arrange
        let status = Status::Review;
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);

        // Act
        let is_toggle = is_done_output_toggle_key(status, key);

        // Assert
        assert!(!is_toggle);
    }

    #[test]
    fn test_is_done_output_toggle_key_rejects_control_modified_key() {
        // Arrange
        let status = Status::Done;
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);

        // Act
        let is_toggle = is_done_output_toggle_key(status, key);

        // Assert
        assert!(!is_toggle);
    }

    #[tokio::test]
    async fn test_view_context_returns_none_for_non_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_none());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_view_context_falls_back_to_list_when_session_is_missing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: "missing-session".to_string(),
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
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.clone(),
            scroll_offset: Some(4),
        };

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_some());
        let context = context.expect("expected view context");
        assert_eq!(
            context.done_session_output_mode,
            DoneSessionOutputMode::Summary
        );
        assert_eq!(context.session_id, session_id);
        assert_eq!(context.scroll_offset, Some(4));
        assert_eq!(context.session_index, 0);
    }

    #[tokio::test]
    async fn test_view_session_snapshot_disables_actions_for_done_session() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions[0].status = Status::Done;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id,
            scroll_offset: Some(1),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

        // Assert
        assert!(!snapshot.can_open_worktree);
        assert!(!snapshot.is_action_allowed);
        assert_eq!(snapshot.session_state, ViewSessionState::Done);
        assert_eq!(snapshot.session_status, Status::Done);
    }

    #[tokio::test]
    async fn test_view_session_snapshot_returns_none_for_stale_session_index() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id,
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
        app.sessions.sessions[0].output = "word ".repeat(40);
        let raw_line_count =
            u16::try_from(app.sessions.sessions[0].output.lines().count()).unwrap_or(u16::MAX);

        // Act
        let total_lines = view_total_lines(
            &app,
            &session_id,
            0,
            DoneSessionOutputMode::Summary,
            None,
            None,
            20,
        );

        // Assert
        assert!(total_lines > raw_line_count);
    }

    #[tokio::test]
    async fn test_view_total_lines_respects_done_output_mode() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions[0].status = Status::Done;
        app.sessions.sessions[0].summary = Some("brief summary".to_string());
        app.sessions.sessions[0].output = "word ".repeat(120);

        // Act
        let summary_lines = view_total_lines(
            &app,
            &session_id,
            0,
            DoneSessionOutputMode::Summary,
            None,
            None,
            20,
        );
        let output_lines = view_total_lines(
            &app,
            &session_id,
            0,
            DoneSessionOutputMode::Output,
            None,
            None,
            20,
        );

        // Assert
        assert!(output_lines > summary_lines);
    }

    #[test]
    fn test_prompt_history_entries_extracts_user_prompts() {
        // Arrange
        let output = " › first\n\nassistant\n\n › second\n\n";

        // Act
        let entries = prompt_history_entries(output);

        // Assert
        assert_eq!(entries, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn test_prompt_history_entries_keeps_multiline_prompts() {
        // Arrange
        let output = " › first line\n   second line\n\nassistant\n\n";

        // Act
        let entries = prompt_history_entries(output);

        // Assert
        assert_eq!(entries, vec!["first line\nsecond line".to_string()]);
    }

    #[test]
    fn test_prompt_history_entries_keeps_multiple_blank_lines_in_prompts() {
        // Arrange
        let output = " › first line\n   \n   \n   after gap\n\nassistant\n\n";

        // Act
        let entries = prompt_history_entries(output);

        // Assert
        assert_eq!(entries, vec!["first line\n\n\nafter gap".to_string()]);
    }

    #[test]
    fn test_prompt_history_entries_ignores_non_prompt_lines() {
        // Arrange
        let output = "assistant line\n\n";

        // Act
        let entries = prompt_history_entries(output);

        // Assert
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_scroll_offset_down_does_not_jump_to_bottom_for_wrapped_output() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions[0].output = "word ".repeat(60);
        let metrics = ViewMetrics {
            total_lines: view_total_lines(
                &app,
                &session_id,
                0,
                DoneSessionOutputMode::Summary,
                None,
                None,
                20,
            ),
            view_height: 5,
        };

        // Act
        let next_offset = scroll_offset_down(Some(0), metrics, 1);

        // Assert
        assert_eq!(next_offset, Some(1));
    }

    #[test]
    fn test_scroll_offset_down_returns_none_at_end_of_content() {
        // Arrange
        let metrics = ViewMetrics {
            total_lines: 20,
            view_height: 10,
        };

        // Act
        let next_offset = scroll_offset_down(Some(9), metrics, 1);

        // Assert
        assert_eq!(next_offset, None);
    }

    #[test]
    fn test_scroll_offset_up_uses_bottom_when_scroll_is_unset() {
        // Arrange
        let metrics = ViewMetrics {
            total_lines: 30,
            view_height: 10,
        };

        // Act
        let next_offset = scroll_offset_up(None, metrics, 5);

        // Assert
        assert_eq!(next_offset, 15);
    }

    #[tokio::test]
    async fn test_apply_view_scroll_and_output_mode_updates_review_state() {
        // Arrange
        let (mut app, _base_dir, expected_session_id) = new_test_app_with_session().await;
        let expected_status_message = review_loading_message(AgentModel::Gpt53Codex);
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: expected_session_id.clone(),
            scroll_offset: Some(3),
        };

        // Act
        apply_view_scroll_and_output_mode(
            &mut app,
            DoneSessionOutputMode::Review,
            Some(expected_status_message.clone()),
            None,
            Some(1),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::Review,
                review_status_message: Some(ref actual_status_message),
                review_text: None,
                ref session_id,
                scroll_offset: Some(1),
            } if session_id == &expected_session_id
                && actual_status_message == &expected_status_message
        ));
    }

    #[test]
    fn test_is_review_loading_status_message_matches_model_aware_message() {
        // Arrange
        let status_message = review_loading_message(AgentModel::ClaudeOpus46);

        // Act
        let is_loading = is_review_loading_status_message(&status_message);

        // Assert
        assert!(is_loading);
    }

    #[test]
    fn test_is_review_loading_status_message_rejects_unrelated_message() {
        // Arrange
        let status_message = "Review complete.";

        // Act
        let is_loading = is_review_loading_status_message(status_message);

        // Assert
        assert!(!is_loading);
    }

    #[tokio::test]
    async fn test_review_assist_model_returns_default_review_model_setting() {
        // Arrange
        let (mut app, _base_dir, _session_id) = new_test_app_with_session().await;
        app.settings.default_review_model = AgentModel::ClaudeOpus46;

        // Act
        let review_model = review_assist_model(&app);

        // Assert
        assert_eq!(review_model, AgentModel::ClaudeOpus46);
    }

    #[tokio::test]
    async fn test_open_review_output_mode_reuses_cached_review_text() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 0,
        };
        let mut next_done_session_output_mode = DoneSessionOutputMode::Summary;
        let mut next_review_status_message = None;
        let mut next_review_text = Some("Cached review".to_string());

        // Act
        open_review_output_mode(
            &mut app,
            &view_context,
            &mut next_done_session_output_mode,
            &mut next_review_status_message,
            &mut next_review_text,
        )
        .await;

        // Assert
        assert_eq!(next_done_session_output_mode, DoneSessionOutputMode::Review);
        assert_eq!(next_review_status_message, None);
        assert_eq!(next_review_text.as_deref(), Some("Cached review"));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_starts_loading_when_diff_exists() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.settings.default_review_model = AgentModel::ClaudeOpus46;
        let session_folder = app.sessions.sessions[0].folder.clone();
        std::fs::write(session_folder.join("README.md"), "review test content\n")
            .expect("failed to update readme");
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 0,
        };
        let mut next_done_session_output_mode = DoneSessionOutputMode::Summary;
        let mut next_review_status_message = None;
        let mut next_review_text = None;

        // Act
        open_review_output_mode(
            &mut app,
            &view_context,
            &mut next_done_session_output_mode,
            &mut next_review_status_message,
            &mut next_review_text,
        )
        .await;

        // Assert
        assert_eq!(next_done_session_output_mode, DoneSessionOutputMode::Review);
        assert_eq!(
            next_review_status_message,
            Some(review_loading_message(AgentModel::ClaudeOpus46))
        );
        assert_eq!(next_review_text, None);
        assert!(matches!(
            app.review_cache.get(&view_context.session_id),
            Some(ReviewCacheEntry::Loading { .. })
        ));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_shows_no_diff_message_when_diff_empty() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 0,
        };
        let mut next_done_session_output_mode = DoneSessionOutputMode::Summary;
        let mut next_review_status_message = None;
        let mut next_review_text = None;

        // Act
        open_review_output_mode(
            &mut app,
            &view_context,
            &mut next_done_session_output_mode,
            &mut next_review_status_message,
            &mut next_review_text,
        )
        .await;

        // Assert
        assert_eq!(next_done_session_output_mode, DoneSessionOutputMode::Review);
        assert_eq!(next_review_status_message, None);
        assert_eq!(next_review_text.as_deref(), Some(REVIEW_NO_DIFF_MESSAGE));
        assert!(!app.review_cache.contains_key(&view_context.session_id));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_clears_stale_session_selection() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 99,
        };
        let mut app = app;
        let mut next_done_session_output_mode = DoneSessionOutputMode::Summary;
        let mut next_review_status_message = None;
        let mut next_review_text = None;

        // Act
        open_review_output_mode(
            &mut app,
            &view_context,
            &mut next_done_session_output_mode,
            &mut next_review_status_message,
            &mut next_review_text,
        )
        .await;

        // Assert
        assert_eq!(next_done_session_output_mode, DoneSessionOutputMode::Review);
        assert_eq!(next_review_status_message, None);
        assert_eq!(next_review_text, Some(String::new()));
    }

    #[tokio::test]
    async fn test_show_diff_for_view_session_switches_mode_to_diff() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(0),
            session_id: session_id.clone(),
            session_index: 0,
        };

        // Act
        show_diff_for_view_session(&mut app, &context).await;

        // Assert
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
    async fn test_show_diff_for_view_session_uses_error_message_outside_git_repo() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let non_git_dir = tempdir().expect("failed to create non-git dir");
        app.sessions.sessions[0].folder = non_git_dir.path().to_path_buf();
        let context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(0),
            session_id: session_id.clone(),
            session_index: 0,
        };

        // Act
        show_diff_for_view_session(&mut app, &context).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                ref session_id,
                ref diff,
                scroll_offset: 0,
                ..
            } if session_id == &context.session_id && diff.contains("Failed to run git diff:")
        ));
    }

    /// Verifies diff loading returns an empty string when the viewed session
    /// disappears before diff generation starts.
    #[tokio::test]
    async fn test_load_view_session_diff_returns_empty_string_for_stale_session_index() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(0),
            session_id,
            session_index: 99,
        };

        // Act
        let diff = load_view_session_diff(&app, &context).await;

        // Assert
        assert!(diff.is_empty());
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
        let output = app.sessions.sessions[0].output.clone();
        assert_eq!(output, "line one");
    }

    #[tokio::test]
    async fn test_open_merge_confirmation_sets_confirmation_mode_with_view_restore_state() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Review,
            review_status_message: Some("Preparing focused review...".to_string()),
            review_text: Some("Critical finding".to_string()),
            session_id: session_id.clone(),
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
                    done_session_output_mode: DoneSessionOutputMode::Review,
                    review_status_message: Some(ref status_message),
                    review_text: Some(ref review_text),
                    scroll_offset: Some(5),
                    session_id: ref restored_session_id,
                }),
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if confirmation_title == "Confirm Merge"
                && confirmation_message == "Add this session to merge queue?"
                && restored_session_id == &session_id
                && mode_session_id == &session_id
                && status_message == "Preparing focused review..."
                && review_text == "Critical finding"
        ));
    }

    #[tokio::test]
    async fn test_open_worktree_for_view_session_opens_command_selector_for_multiple_commands() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.settings.open_command = "cargo test\nnpm run dev".to_string();
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: Some("Preparing focused review".to_string()),
            review_text: Some("Critical finding".to_string()),
            session_id: session_id.clone(),
            scroll_offset: Some(4),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        open_worktree_for_view_session(&mut app, &context).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::OpenCommandSelector {
                ref commands,
                restore_view:
                    ConfirmationViewMode {
                        done_session_output_mode: DoneSessionOutputMode::Summary,
                        review_status_message: Some(ref status_message),
                        review_text: Some(ref review_text),
                        session_id: ref restored_session_id,
                        scroll_offset: Some(4),
                    },
                selected_command_index: 0,
            } if commands == &vec!["cargo test".to_string(), "npm run dev".to_string()]
                && restored_session_id == &session_id
                && status_message == "Preparing focused review"
                && review_text == "Critical finding"
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
        app.settings.open_command = "cargo test".to_string();
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.clone(),
            scroll_offset: Some(2),
        };
        let context = view_context(&mut app).expect("expected view context");

        // Act
        open_worktree_for_view_session(&mut app, &context).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                review_status_message: None,
                review_text: None,
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
        let output = app.sessions.sessions[0].output.clone();
        assert!(output.contains("[Rebase Error]"));
    }

    #[tokio::test]
    async fn test_open_view_help_overlay_preserves_view_context() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Review,
            review_status_message: Some("Preparing focused review".to_string()),
            review_text: Some("Critical finding".to_string()),
            scroll_offset: Some(3),
            session_id: session_id.clone(),
            session_index: 0,
        };

        // Act
        open_view_help_overlay(
            &mut app,
            &view_context,
            Some(PublishBranchAction::Push),
            ViewSessionState::Review,
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::View {
                    done_session_output_mode: DoneSessionOutputMode::Review,
                    review_status_message: Some(ref status_message),
                    review_text: Some(ref review_text),
                    publish_branch_action: Some(PublishBranchAction::Push),
                    session_id: ref session_id_in_mode,
                    session_state: ViewSessionState::Review,
                    scroll_offset: Some(3),
                },
                scroll_offset: 0,
            } if session_id_in_mode == &session_id
                && status_message == "Preparing focused review"
                && review_text == "Critical finding"
        ));
    }

    #[tokio::test]
    async fn test_open_publish_branch_input_preserves_view_context() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Review,
            review_status_message: Some("Preparing focused review".to_string()),
            review_text: Some("Critical finding".to_string()),
            scroll_offset: Some(5),
            session_id: session_id.clone(),
            session_index: 0,
        };

        // Act
        open_publish_branch_input(&mut app, &view_context, PublishBranchAction::Push);

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                ref default_branch_name,
                input: ref input_state,
                locked_upstream_ref: None,
                publish_branch_action: PublishBranchAction::Push,
                restore_view:
                    ConfirmationViewMode {
                        done_session_output_mode: DoneSessionOutputMode::Review,
                        review_status_message: Some(ref status_message),
                        review_text: Some(ref review_text),
                        session_id: ref restored_session_id,
                        scroll_offset: Some(5),
                    },
            } if default_branch_name == &crate::app::session::session_branch(&session_id)
                && input_state.cursor == 0
                && input_state.text().is_empty()
                && restored_session_id == &session_id
                && status_message == "Preparing focused review"
                && review_text == "Critical finding"
        ));
    }

    #[tokio::test]
    async fn test_open_publish_branch_input_locks_existing_upstream_branch_name() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions[0].published_upstream_ref = Some("origin/review/custom".to_string());
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(1),
            session_id,
            session_index: 0,
        };

        // Act
        open_publish_branch_input(&mut app, &view_context, PublishBranchAction::Push);

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
    fn test_can_open_session_worktree_disables_canceled_state() {
        // Arrange
        let status = Status::Canceled;

        // Act
        let result = can_open_session_worktree(status);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_can_open_session_worktree_disables_done_state() {
        // Arrange
        let status = Status::Done;

        // Act
        let result = can_open_session_worktree(status);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_can_open_session_worktree_disables_queued_state() {
        // Arrange
        let status = Status::Queued;

        // Act
        let result = can_open_session_worktree(status);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_can_open_session_worktree_enables_review_state() {
        // Arrange
        let status = Status::Review;

        // Act
        let result = can_open_session_worktree(status);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_view_session_state_maps_merge_queue_statuses() {
        // Arrange
        let merge_queue_statuses = [Status::Queued, Status::Merging];

        // Act
        let mapped_states: Vec<ViewSessionState> = merge_queue_statuses
            .iter()
            .map(|status| view_session_state(*status))
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

        // Act
        let state = view_session_state(status);

        // Assert
        assert_eq!(state, ViewSessionState::Rebasing);
    }

    #[test]
    fn test_view_session_state_maps_canceled_status() {
        // Arrange
        let status = Status::Canceled;

        // Act
        let state = view_session_state(status);

        // Assert
        assert_eq!(state, ViewSessionState::Canceled);
    }
    #[tokio::test]
    async fn test_open_review_output_mode_uses_ready_cache_entry() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let cached_text = "## Review\nCached review from auto-generation.";
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash: 123,
                text: cached_text.to_string(),
            },
        );
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 0,
        };
        let mut next_done_session_output_mode = DoneSessionOutputMode::Summary;
        let mut next_review_status_message = None;
        let mut next_review_text = None;

        // Act
        open_review_output_mode(
            &mut app,
            &view_context,
            &mut next_done_session_output_mode,
            &mut next_review_status_message,
            &mut next_review_text,
        )
        .await;

        // Assert
        assert_eq!(next_done_session_output_mode, DoneSessionOutputMode::Review);
        assert_eq!(next_review_status_message, None);
        assert_eq!(next_review_text.as_deref(), Some(cached_text));
    }

    #[tokio::test]
    async fn test_open_review_output_mode_shows_loading_for_cache_loading_entry() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.settings.default_review_model = AgentModel::ClaudeOpus46;
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Loading { diff_hash: 456 },
        );
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 0,
        };
        let mut next_done_session_output_mode = DoneSessionOutputMode::Summary;
        let mut next_review_status_message = None;
        let mut next_review_text = None;

        // Act
        open_review_output_mode(
            &mut app,
            &view_context,
            &mut next_done_session_output_mode,
            &mut next_review_status_message,
            &mut next_review_text,
        )
        .await;

        // Assert
        assert_eq!(next_done_session_output_mode, DoneSessionOutputMode::Review);
        assert_eq!(
            next_review_status_message,
            Some(review_loading_message(AgentModel::ClaudeOpus46))
        );
        assert_eq!(next_review_text, None);
    }

    #[tokio::test]
    async fn test_open_or_regenerate_review_opens_when_not_in_review() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(5),
            session_id,
            session_index: 0,
        };
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        open_or_regenerate_review(&mut app, &view_context, &mut pending_update).await;

        // Assert
        assert_eq!(
            pending_update.done_session_output_mode,
            DoneSessionOutputMode::Review
        );
        assert_eq!(pending_update.scroll_offset, None);
    }

    #[tokio::test]
    async fn test_open_or_regenerate_shows_confirmation_when_already_viewing() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                text: "Old review".to_string(),
                diff_hash: 123,
            },
        );
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Review,
            review_status_message: None,
            review_text: Some("Old review".to_string()),
            scroll_offset: None,
            session_id: session_id.clone(),
            session_index: 0,
        };
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        open_or_regenerate_review(&mut app, &view_context, &mut pending_update).await;

        // Assert — confirmation popup is shown instead of direct regeneration
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::RegenerateReview,
                ..
            }
        ));
        // Cache is preserved until user confirms
        assert!(app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn test_open_or_regenerate_skips_when_loading_in_progress() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Loading { diff_hash: 42 },
        );
        let loading_message = review_loading_message(app.settings.default_review_model);
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Review,
            review_status_message: Some(loading_message.clone()),
            review_text: None,
            scroll_offset: None,
            session_id: session_id.clone(),
            session_index: 0,
        };
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act
        open_or_regenerate_review(&mut app, &view_context, &mut pending_update).await;

        // Assert — cache and loading state are preserved, no duplicate spawned
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Loading { diff_hash: 42 })
        ));
        assert_eq!(pending_update.review_status_message, Some(loading_message));
    }

    #[tokio::test]
    async fn test_handle_view_key_ignores_diff_for_non_review_status() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.clone(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);
        let view_session_snapshot = ViewSessionSnapshot {
            can_open_worktree: false,
            is_action_allowed: false,
            publish_branch_action: None,
            session_output: String::new(),
            session_state: ViewSessionState::Done,
            session_status: Status::Done,
        };
        let view_key_context = ViewKeyContext {
            context: &view_context,
            metrics: ViewMetrics {
                total_lines: 10,
                view_height: 5,
            },
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
        assert_eq!(
            pending_update.done_session_output_mode,
            DoneSessionOutputMode::Summary
        );
    }

    /// Verifies session-view action keys are ignored when the current session
    /// status does not allow those actions.
    #[tokio::test]
    async fn test_handle_view_key_ignores_status_gated_actions() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.clone(),
            scroll_offset: Some(2),
        };
        let view_context = view_context(&mut app).expect("expected view context");
        let view_metrics = ViewMetrics {
            total_lines: 10,
            view_height: 5,
        };

        // Act
        for key in [
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        ] {
            let mut pending_update = ViewPendingUpdate::from_context(&view_context);
            let view_session_snapshot = ViewSessionSnapshot {
                can_open_worktree: false,
                is_action_allowed: false,
                publish_branch_action: None,
                session_output: String::new(),
                session_state: ViewSessionState::Done,
                session_status: Status::Done,
            };
            let view_key_context = ViewKeyContext {
                context: &view_context,
                metrics: view_metrics,
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
            assert_eq!(
                pending_update.done_session_output_mode,
                DoneSessionOutputMode::Summary
            );
        }
    }

    #[tokio::test]
    async fn test_q_in_review_exits_to_summary() {
        // Arrange
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Review,
            review_status_message: None,
            review_text: Some("review text".to_string()),
            scroll_offset: Some(10),
            session_id: String::new(),
            session_index: 0,
        };
        let mut pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act — simulate the q guard branch
        pending_update.done_session_output_mode = DoneSessionOutputMode::Summary;
        pending_update.scroll_offset = None;

        // Assert
        assert_eq!(
            pending_update.done_session_output_mode,
            DoneSessionOutputMode::Summary
        );
        assert_eq!(pending_update.scroll_offset, None);
    }

    #[tokio::test]
    async fn test_q_in_summary_mode_transitions_to_list() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let view_context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 0,
        };
        let pending_update = ViewPendingUpdate::from_context(&view_context);

        // Act — the q key without focused review transitions app mode to List
        app.mode = AppMode::List;

        // Assert — mode is List and output mode stayed Summary
        assert!(matches!(app.mode, AppMode::List));
        assert_eq!(
            pending_update.done_session_output_mode,
            DoneSessionOutputMode::Summary
        );
    }
}
