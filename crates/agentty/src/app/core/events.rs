//! Event types and reducer helpers for the app core module.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use app::branch_publish::{
    BranchPublishActionUpdate, BranchPublishTaskResult, BranchPublishTaskSuccess,
    branch_publish_loading_label as branch_publish_loading_label_text,
    branch_publish_loading_message as branch_publish_loading_message_text,
    branch_publish_loading_title as branch_publish_loading_title_text,
    branch_publish_success_title as branch_publish_success_title_text,
    detected_forge_kind_from_git_push_error, git_push_authentication_message,
    is_git_push_authentication_error,
    pull_request_publish_success_message as pull_request_publish_success_message_text,
};
use app::reducer::AppEventReducer;
use app::review::{ReviewUpdate, apply_review_updates, auto_start_reviews};

use super::state::{App, SyncPopupContext, SyncReviewRequestTaskResult, UpdateStatus};
use crate::app;
use crate::app::session::{
    SessionTaskService, SyncMainOutcome, SyncSessionStartError, TurnAppliedState,
};
use crate::app::session_state::SessionGitStatus;
use crate::domain::input::InputState;
use crate::domain::session::{
    PublishBranchAction, PublishedBranchSyncStatus, SessionId, SessionSize, Status,
};
use crate::infra::file_index::FileEntry;
use crate::runtime::mode::{question, sync_blocked};
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode, QuestionFocus};
use crate::ui::state::prompt::PromptAtMentionState;

/// Internal app events emitted by background workers and workflows.
///
/// Producers should emit events only; state mutation is centralized in
/// [`App::apply_app_events`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    /// Indicates background-loaded prompt at-mention entries for one session.
    AtMentionEntriesLoaded {
        entries: Vec<FileEntry>,
        session_id: SessionId,
    },
    /// Indicates the latest project-branch and session-branch ahead/behind
    /// information from the git status worker.
    GitStatusUpdated {
        session_statuses: HashMap<SessionId, SessionGitStatus>,
        status: Option<(u32, u32)>,
    },
    /// Indicates whether a newer stable `agentty` release is available.
    VersionAvailabilityUpdated {
        latest_available_version: Option<String>,
    },
    /// Indicates progress of the background auto-update.
    UpdateStatusChanged { update_status: UpdateStatus },
    /// Indicates a session model selection has been persisted.
    SessionModelUpdated {
        session_id: SessionId,
        session_model: crate::domain::agent::AgentModel,
    },
    /// Indicates a session reasoning override selection has been persisted.
    SessionReasoningLevelUpdated {
        reasoning_level_override: Option<crate::domain::agent::ReasoningLevel>,
        session_id: SessionId,
    },
    /// Requests a full session list refresh.
    RefreshSessions,
    /// Requests an immediate git-status refresh outside the periodic poll
    /// cadence.
    RefreshGitStatus,
    /// Indicates compact live thinking text for an in-progress session.
    SessionProgressUpdated {
        progress_message: Option<String>,
        session_id: SessionId,
    },
    /// Indicates completion of a list-mode sync workflow.
    SyncMainCompleted {
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    },
    /// Indicates recomputed diff-derived size and line-count totals for one
    /// session.
    SessionSizeUpdated {
        added_lines: u64,
        deleted_lines: u64,
        session_id: SessionId,
        session_size: SessionSize,
    },
    /// Indicates one tracked draft-title generation task reached a terminal
    /// outcome and can be pruned from in-memory task tracking.
    SessionTitleGenerationFinished {
        generation: u64,
        session_id: SessionId,
    },
    /// Indicates completion of a session-view branch-publish action.
    BranchPublishActionCompleted {
        restore_view: ConfirmationViewMode,
        result: Box<BranchPublishTaskResult>,
        session_id: SessionId,
    },
    /// Indicates review assist output became available for a session.
    ReviewPrepared {
        diff_hash: u64,
        review_text: String,
        session_id: SessionId,
    },
    /// Indicates review assist failed for a session.
    ReviewPreparationFailed {
        diff_hash: u64,
        error: String,
        session_id: SessionId,
    },
    /// Indicates that a session handle snapshot changed in-memory.
    SessionUpdated { session_id: SessionId },
    /// Indicates that an agent turn completed and persisted one reducer-ready
    /// projection.
    AgentResponseReceived {
        session_id: SessionId,
        turn_applied_state: TurnAppliedState,
    },
    /// Indicates that one published session branch started or finished a
    /// background auto-push after a completed turn.
    PublishedBranchSyncUpdated {
        session_id: SessionId,
        sync_operation_id: String,
        sync_status: PublishedBranchSyncStatus,
    },
    /// Indicates completion of one background review-request status refresh.
    ReviewRequestStatusUpdated {
        result: Result<SyncReviewRequestTaskResult, String>,
        session_id: SessionId,
    },
}

/// Reduced representation of all app events currently queued for one tick.
#[derive(Default)]
pub(super) struct AppEventBatch {
    pub(super) applied_turns: HashMap<SessionId, TurnAppliedState>,
    pub(super) at_mention_entries_updates: HashMap<SessionId, Vec<FileEntry>>,
    pub(super) branch_publish_action_update: Option<BranchPublishActionUpdate>,
    pub(super) git_status_update: Option<GitStatusBatchUpdate>,
    pub(super) latest_available_version_update: Option<LatestAvailableVersionUpdate>,
    pub(super) published_branch_sync_updates: Vec<(SessionId, PublishedBranchSyncUpdate)>,
    pub(super) review_updates: HashMap<SessionId, ReviewUpdate>,
    pub(super) session_git_status_updates: HashMap<SessionId, SessionGitStatus>,
    pub(super) session_ids: HashSet<SessionId>,
    pub(super) session_model_updates: HashMap<SessionId, crate::domain::agent::AgentModel>,
    pub(super) session_reasoning_level_updates:
        HashMap<SessionId, Option<crate::domain::agent::ReasoningLevel>>,
    pub(super) session_progress_updates: HashMap<SessionId, Option<String>>,
    pub(super) session_size_updates: HashMap<SessionId, (u64, u64, SessionSize)>,
    pub(super) session_title_generation_finished: HashMap<SessionId, u64>,
    pub(super) should_refresh_git_status: bool,
    pub(super) should_force_reload: bool,
    pub(super) review_request_status_updates: Vec<ReviewRequestStatusUpdate>,
    pub(super) sync_main_result: Option<Result<SyncMainOutcome, SyncSessionStartError>>,
    pub(super) update_status: Option<UpdateStatus>,
}

/// Optional aggregate git status payload from the latest status event in one
/// reducer batch.
pub(super) struct GitStatusBatchUpdate {
    /// Main worktree added/deleted line counts, when available.
    status: Option<(u32, u32)>,
}

/// Optional version-availability payload from the latest updater event in one
/// reducer batch.
pub(super) struct LatestAvailableVersionUpdate {
    /// Latest available version string, or `None` when no update is available.
    latest_available_version: Option<String>,
}

/// One ordered published-branch sync update queued for one session.
pub(super) struct PublishedBranchSyncUpdate {
    /// Operation identifier used to ignore stale terminal auto-push updates.
    sync_operation_id: String,
    /// Auto-push state carried by this update.
    sync_status: PublishedBranchSyncStatus,
}

/// Completed review-request status refresh payload ready for reducer
/// application.
pub(super) struct ReviewRequestStatusUpdate {
    pub(super) result: Result<SyncReviewRequestTaskResult, String>,
    pub(super) session_id: SessionId,
}

impl AppEventBatch {
    /// Collects one app event into the coalesced batch state.
    ///
    /// Most per-session projections use latest-wins semantics, but queued
    /// `AgentResponseReceived` events merge token-usage deltas so one reducer
    /// tick preserves cumulative usage from multiple completed turns.
    pub(super) fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => {
                self.at_mention_entries_updates.insert(session_id, entries);
            }
            AppEvent::GitStatusUpdated {
                session_statuses,
                status,
            } => self.collect_git_status_updated(session_statuses, status),
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            } => self.collect_version_availability_updated(latest_available_version),
            AppEvent::UpdateStatusChanged { update_status } => {
                self.update_status = Some(update_status);
            }
            AppEvent::SessionModelUpdated {
                session_id,
                session_model,
            } => {
                self.session_model_updates.insert(session_id, session_model);
            }
            AppEvent::SessionReasoningLevelUpdated {
                reasoning_level_override,
                session_id,
            } => {
                self.session_reasoning_level_updates
                    .insert(session_id, reasoning_level_override);
            }
            AppEvent::RefreshSessions => {
                self.should_force_reload = true;
            }
            AppEvent::RefreshGitStatus => {
                self.should_refresh_git_status = true;
            }
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => {
                self.session_progress_updates
                    .insert(session_id, progress_message);
            }
            AppEvent::SyncMainCompleted { result } => {
                self.collect_sync_main_completed(result);
            }
            AppEvent::SessionSizeUpdated {
                added_lines,
                deleted_lines,
                session_id,
                session_size,
            } => {
                self.session_size_updates
                    .insert(session_id, (added_lines, deleted_lines, session_size));
            }
            AppEvent::SessionTitleGenerationFinished {
                generation,
                session_id,
            } => {
                self.session_title_generation_finished
                    .insert(session_id, generation);
            }
            AppEvent::BranchPublishActionCompleted {
                restore_view,
                result,
                session_id,
            } => self.collect_branch_publish_action_completed(restore_view, *result, session_id),
            AppEvent::ReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            } => self.collect_review_prepared(diff_hash, review_text, session_id),
            AppEvent::ReviewPreparationFailed {
                diff_hash,
                error,
                session_id,
            } => self.collect_review_preparation_failed(diff_hash, error, session_id),
            AppEvent::SessionUpdated { session_id } => {
                self.session_ids.insert(session_id);
            }
            AppEvent::AgentResponseReceived {
                session_id,
                turn_applied_state,
            } => self.collect_agent_response_received(session_id, turn_applied_state),
            AppEvent::PublishedBranchSyncUpdated {
                session_id,
                sync_operation_id,
                sync_status,
            } => self.collect_published_branch_sync_updated(
                session_id,
                sync_operation_id,
                sync_status,
            ),
            AppEvent::ReviewRequestStatusUpdated { result, session_id } => {
                self.collect_review_request_status_updated(result, session_id);
            }
        }
    }

    /// Stores the latest git status event for this reducer batch.
    fn collect_git_status_updated(
        &mut self,
        session_statuses: HashMap<SessionId, SessionGitStatus>,
        status: Option<(u32, u32)>,
    ) {
        self.git_status_update = Some(GitStatusBatchUpdate { status });
        self.session_git_status_updates = session_statuses;
    }

    /// Stores the latest version availability event for this reducer batch.
    fn collect_version_availability_updated(&mut self, latest_available_version: Option<String>) {
        self.latest_available_version_update = Some(LatestAvailableVersionUpdate {
            latest_available_version,
        });
    }

    /// Stores the latest default-branch sync result for this reducer batch.
    fn collect_sync_main_completed(
        &mut self,
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    ) {
        if result.is_ok() {
            self.should_refresh_git_status = true;
        }

        self.sync_main_result = Some(result);
    }

    /// Stores the latest branch-publish action result for this reducer batch.
    fn collect_branch_publish_action_completed(
        &mut self,
        restore_view: ConfirmationViewMode,
        result: BranchPublishTaskResult,
        session_id: SessionId,
    ) {
        if result.is_ok() {
            self.should_refresh_git_status = true;
        }

        self.branch_publish_action_update = Some(BranchPublishActionUpdate {
            restore_view,
            result,
            session_id,
        });
    }

    /// Stores a successful focused-review preparation result.
    fn collect_review_prepared(
        &mut self,
        diff_hash: u64,
        review_text: String,
        session_id: SessionId,
    ) {
        self.review_updates.insert(
            session_id,
            ReviewUpdate {
                diff_hash,
                result: Ok(review_text),
            },
        );
    }

    /// Stores a failed focused-review preparation result.
    fn collect_review_preparation_failed(
        &mut self,
        diff_hash: u64,
        error: String,
        session_id: SessionId,
    ) {
        self.review_updates.insert(
            session_id,
            ReviewUpdate {
                diff_hash,
                result: Err(error),
            },
        );
    }

    /// Queues one published-branch sync state transition for ordered
    /// reducer application.
    fn collect_published_branch_sync_updated(
        &mut self,
        session_id: SessionId,
        sync_operation_id: String,
        sync_status: PublishedBranchSyncStatus,
    ) {
        if matches!(
            sync_status,
            PublishedBranchSyncStatus::Idle | PublishedBranchSyncStatus::Succeeded
        ) {
            self.should_refresh_git_status = true;
        }

        self.published_branch_sync_updates.push((
            session_id,
            PublishedBranchSyncUpdate {
                sync_operation_id,
                sync_status,
            },
        ));
    }

    /// Queues one review-request status refresh result for reducer
    /// application.
    fn collect_review_request_status_updated(
        &mut self,
        result: Result<SyncReviewRequestTaskResult, String>,
        session_id: SessionId,
    ) {
        self.review_request_status_updates
            .push(ReviewRequestStatusUpdate { result, session_id });
    }

    /// Merges one completed-turn projection into the per-session batch.
    ///
    /// Agent responses also mark the session as touched so the reducer still
    /// synchronizes handle-backed status and evaluates auto-review startup
    /// even when the matching `SessionUpdated` event lands in a later tick.
    /// Latest reducer-facing fields replace the older projection, while token
    /// deltas accumulate to preserve usage across multiple queued completions
    /// for the same session.
    fn collect_agent_response_received(
        &mut self,
        session_id: SessionId,
        turn_applied_state: TurnAppliedState,
    ) {
        self.session_ids.insert(session_id.clone());

        match self.applied_turns.entry(session_id) {
            Entry::Occupied(mut occupied_entry) => {
                occupied_entry.get_mut().merge_newer(turn_applied_state);
            }
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(turn_applied_state);
            }
        }
    }
}

impl App {
    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh and
    /// git-status updates, then applies session-handle sync for touched
    /// sessions.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = AppEventReducer::drain(&mut self.event_rx, first_event);
        let mut event_batch = AppEventBatch::default();
        for event in drained_events {
            event_batch.collect_event(event);
        }

        self.apply_app_event_batch(event_batch).await;
    }

    /// Processes currently queued app events without waiting.
    ///
    /// The foreground runtime calls this before draw so queued
    /// `SessionUpdated` events can synchronize only the touched sessions into
    /// render snapshots without polling every live handle each frame.
    pub(crate) async fn process_pending_app_events(&mut self) {
        let Ok(first_event) = self.event_rx.try_recv() else {
            return;
        };

        self.apply_app_events(first_event).await;
    }

    /// Waits for the next internal app event.
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Applies one reduced app-event batch to in-memory app state.
    ///
    /// Session updates are synchronized from runtime handles first. Any touched
    /// session that reached terminal status (`Done`, `Canceled`) then drops its
    /// worker queue so background workers can shut down provider runtimes.
    async fn apply_app_event_batch(&mut self, mut event_batch: AppEventBatch) {
        let previous_session_states = self.previous_session_states(&event_batch.session_ids);

        if event_batch.should_force_reload {
            self.refresh_sessions_now().await;
            self.reload_projects().await;
            self.refresh_active_project_roadmap_and_tabs().await;
        }

        if event_batch.should_refresh_git_status {
            self.restart_git_status_task();
        }

        if let Some(git_status_update) = &event_batch.git_status_update {
            self.projects.set_git_status(git_status_update.status);
            self.sessions
                .replace_session_git_statuses(event_batch.session_git_status_updates.clone());
        }

        if let Some(latest_available_version_update) = &event_batch.latest_available_version_update
        {
            self.latest_available_version
                .clone_from(&latest_available_version_update.latest_available_version);
        }

        if let Some(update_status) = event_batch.update_status {
            self.update_status = Some(update_status);
        }

        for (session_id, session_model) in event_batch.session_model_updates {
            self.sessions
                .apply_session_model_updated(&session_id, session_model);
        }

        for (session_id, reasoning_level_override) in event_batch.session_reasoning_level_updates {
            self.sessions
                .apply_session_reasoning_level_updated(&session_id, reasoning_level_override);
        }

        for (session_id, (added_lines, deleted_lines, session_size)) in
            event_batch.session_size_updates
        {
            self.sessions.apply_session_size_updated(
                &session_id,
                added_lines,
                deleted_lines,
                session_size,
            );
        }

        for (session_id, generation) in event_batch.session_title_generation_finished {
            self.sessions
                .clear_title_generation_task_if_matches(&session_id, generation);
        }

        for (session_id, entries) in event_batch.at_mention_entries_updates {
            self.apply_prompt_at_mention_entries(&session_id, entries);
        }

        apply_review_updates(
            &mut self.review_cache,
            &mut self.mode,
            &mut self.sessions,
            event_batch.review_updates,
        );

        if let Some(branch_publish_action_update) = event_batch.branch_publish_action_update {
            self.apply_branch_publish_action_update(branch_publish_action_update);
        }

        for review_request_status_update in event_batch.review_request_status_updates {
            self.apply_review_request_status_update(review_request_status_update)
                .await;
        }

        self.apply_session_progress_updates(std::mem::take(
            &mut event_batch.session_progress_updates,
        ));

        for (session_id, turn_applied_state) in event_batch.applied_turns {
            self.apply_agent_response_received(&session_id, &turn_applied_state);
        }
        for (session_id, sync_update) in event_batch.published_branch_sync_updates {
            self.apply_published_branch_sync_update(&session_id, sync_update);
        }

        for session_id in &event_batch.session_ids {
            self.sessions.sync_session_from_handle(session_id);
        }
        self.sessions
            .clear_terminal_session_workers(&event_batch.session_ids);

        auto_start_reviews(
            &mut self.review_cache,
            &event_batch.session_ids,
            &mut self.sessions,
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_model,
        )
        .await;

        if let Some(sync_main_result) = event_batch.sync_main_result {
            let should_refresh_active_project_roadmap = sync_main_result.is_ok();
            let sync_popup_context = self.sync_popup_context();

            self.mode = Self::sync_main_popup_mode(sync_main_result, &sync_popup_context);
            if should_refresh_active_project_roadmap {
                self.refresh_active_project_roadmap_and_tabs().await;
            }
        }

        self.handle_merge_queue_progress(&event_batch.session_ids, &previous_session_states)
            .await;
        self.retain_valid_session_progress_messages();
        self.sessions.retain_active_prompt_outputs();
    }

    /// Returns status snapshots for sessions touched before applying a
    /// reducer batch.
    fn previous_session_states(
        &self,
        session_ids: &HashSet<SessionId>,
    ) -> HashMap<SessionId, Status> {
        session_ids
            .iter()
            .filter_map(|session_id| {
                self.sessions
                    .sessions
                    .iter()
                    .find(|session| session.id == *session_id)
                    .map(|session| (session_id.clone(), session.status))
            })
            .collect()
    }

    /// Applies active progress message updates from one reducer batch.
    fn apply_session_progress_updates(
        &mut self,
        session_progress_updates: HashMap<SessionId, Option<String>>,
    ) {
        for (session_id, progress_message) in session_progress_updates {
            if let Some(progress_message) = progress_message {
                self.session_progress_messages
                    .insert(session_id, progress_message);
            } else {
                self.session_progress_messages.remove(&session_id);
            }
        }
    }

    /// Routes one persisted turn projection to the currently focused session
    /// UI.
    ///
    /// The session worker persists the canonical summary, clarification
    /// questions, summary, and token-usage delta before sending this
    /// event, so the reducer can apply the exact same projection in memory
    /// without waiting for a forced reload.
    fn apply_agent_response_received(
        &mut self,
        session_id: &str,
        turn_applied_state: &TurnAppliedState,
    ) {
        if !self
            .sessions
            .sessions
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.sessions
            .apply_turn_applied_state(session_id, turn_applied_state);
        let questions = turn_applied_state.questions.clone();
        if questions.is_empty() {
            return;
        }

        if self.is_viewing_session(session_id) {
            let (review_status_message, review_text) = self.question_mode_review_state(session_id);
            self.mode = AppMode::Question {
                at_mention_state: None,
                selected_option_index: question::default_option_index(&questions, 0),
                session_id: session_id.into(),
                questions,
                review_status_message,
                review_text,
                responses: Vec::new(),
                current_index: 0,
                focus: QuestionFocus::Answer,
                input: InputState::default(),
                scroll_offset: None,
            };
        }
    }

    /// Returns whether the active UI mode currently shows the provided
    /// session.
    fn is_viewing_session(&self, session_id: &str) -> bool {
        match &self.mode {
            AppMode::View {
                session_id: view_id,
                ..
            }
            | AppMode::Prompt {
                session_id: view_id,
                ..
            }
            | AppMode::Diff {
                session_id: view_id,
                ..
            }
            | AppMode::Question {
                session_id: view_id,
                ..
            }
            | AppMode::OpenCommandSelector {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            }
            | AppMode::PublishBranchInput {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            }
            | AppMode::ViewInfoPopup {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            } => view_id == session_id,
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Help { .. } => false,
        }
    }

    /// Returns the focused-review state that should remain visible when the
    /// UI enters clarification-question mode for the provided session.
    fn question_mode_review_state(&self, session_id: &str) -> (Option<String>, Option<String>) {
        match &self.mode {
            AppMode::View {
                review_status_message,
                review_text,
                ..
            }
            | AppMode::Prompt {
                review_status_message,
                review_text,
                ..
            }
            | AppMode::Question {
                review_status_message,
                review_text,
                ..
            } => (review_status_message.clone(), review_text.clone()),
            AppMode::OpenCommandSelector { restore_view, .. }
            | AppMode::PublishBranchInput { restore_view, .. }
            | AppMode::ViewInfoPopup { restore_view, .. } => (
                restore_view.review_status_message.clone(),
                restore_view.review_text.clone(),
            ),
            AppMode::Diff {
                session_id: diff_session_id,
                ..
            } if diff_session_id == session_id => self.review_view_state(session_id),
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. } => (None, None),
        }
    }

    /// Routes one published-branch auto-push update to the matching in-memory
    /// session snapshot.
    fn apply_published_branch_sync_update(
        &mut self,
        session_id: &str,
        sync_update: PublishedBranchSyncUpdate,
    ) {
        let PublishedBranchSyncUpdate {
            sync_operation_id,
            sync_status,
        } = sync_update;

        match sync_status {
            PublishedBranchSyncStatus::InProgress => {
                self.sessions
                    .start_published_branch_sync(session_id, sync_operation_id);
            }
            PublishedBranchSyncStatus::Idle
            | PublishedBranchSyncStatus::Succeeded
            | PublishedBranchSyncStatus::Failed => {
                self.sessions.finish_published_branch_sync(
                    session_id,
                    &sync_operation_id,
                    sync_status,
                );
            }
        }
    }

    /// Applies loaded at-mention entries to the currently focused prompt or
    /// question session, if the mention query is still active.
    fn apply_prompt_at_mention_entries(&mut self, session_id: &str, entries: Vec<FileEntry>) {
        let (at_mention_state, has_query) = match &mut self.mode {
            AppMode::Prompt {
                at_mention_state,
                input,
                session_id: mode_session_id,
                ..
            } if mode_session_id == session_id => {
                (at_mention_state, input.at_mention_query().is_some())
            }
            AppMode::Question {
                at_mention_state,
                input,
                session_id: mode_session_id,
                ..
            } if mode_session_id == session_id => {
                (at_mention_state, input.at_mention_query().is_some())
            }
            _ => return,
        };

        if !has_query {
            return;
        }

        if let Some(state) = at_mention_state.as_mut() {
            state.all_entries = entries;
            state.selected_index = 0;

            return;
        }

        *at_mention_state = Some(PromptAtMentionState::new(entries));
    }

    /// Applies one review assist update to cache and focused render state.
    #[cfg(test)]
    pub(super) fn apply_review_update(
        &mut self,
        session_id: &str,
        review_update: app::review::ReviewUpdate,
    ) {
        let mut review_updates = HashMap::new();
        review_updates.insert(SessionId::from(session_id), review_update);
        apply_review_updates(
            &mut self.review_cache,
            &mut self.mode,
            &mut self.sessions,
            review_updates,
        );
    }

    /// Starts focused review generation for sessions that just entered review.
    #[cfg(test)]
    pub(super) async fn auto_start_reviews(&mut self, session_ids: &HashSet<SessionId>) {
        auto_start_reviews(
            &mut self.review_cache,
            session_ids,
            &mut self.sessions,
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_model,
        )
        .await;
    }

    /// Applies one completed branch-publish action and updates the popup.
    pub(super) fn apply_branch_publish_action_update(
        &mut self,
        branch_publish_action_update: BranchPublishActionUpdate,
    ) {
        let BranchPublishActionUpdate {
            restore_view,
            result,
            session_id,
        } = branch_publish_action_update;

        let popup_mode = match result {
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name,
                review_request_creation,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);

                Self::view_info_popup_mode(
                    Self::branch_publish_success_title(PublishBranchAction::Push),
                    Self::branch_publish_success_message(
                        &branch_name,
                        review_request_creation.as_ref(),
                    ),
                    false,
                    String::new(),
                    restore_view,
                )
            }
            Ok(BranchPublishTaskSuccess::PullRequestPublished {
                branch_name,
                review_request,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);
                self.sessions
                    .apply_review_request(&session_id, review_request.clone());

                Self::view_info_popup_mode(
                    Self::review_request_publish_success_title(&review_request),
                    Self::pull_request_publish_success_message(&branch_name, &review_request),
                    false,
                    String::new(),
                    restore_view,
                )
            }
            Err(failure) => Self::view_info_popup_mode(
                failure.title,
                failure.message,
                false,
                String::new(),
                restore_view,
            ),
        };
        self.mode = popup_mode;
    }

    /// Applies one background review-request status refresh.
    pub(super) async fn apply_review_request_status_update(
        &mut self,
        review_request_status_update: ReviewRequestStatusUpdate,
    ) {
        let ReviewRequestStatusUpdate { result, session_id } = review_request_status_update;

        let Ok(task_result) = result else {
            return;
        };

        if let Some(summary) = task_result.summary {
            let _ = self
                .sessions
                .store_review_request_summary(&self.services, &session_id, summary)
                .await;
        }

        match task_result.outcome {
            crate::app::session::SyncReviewRequestOutcome::Merged { .. } => {
                if let Some(warning) = self.complete_externally_merged_session(&session_id).await {
                    self.append_output_for_session(
                        &session_id,
                        &format!("\n[Review Request Sync Warning] {warning}\n"),
                    )
                    .await;
                }
            }
            crate::app::session::SyncReviewRequestOutcome::Closed { .. } => {
                self.cancel_externally_closed_session(&session_id).await;
            }
            crate::app::session::SyncReviewRequestOutcome::Open { .. }
            | crate::app::session::SyncReviewRequestOutcome::NoReviewRequest => {}
        }
    }

    /// Transitions one externally merged session to `Done` with best-effort
    /// worktree and branch cleanup.
    ///
    /// Returns an optional warning message when worktree cleanup fails. The
    /// session is still moved to `Done` because the merge already happened
    /// upstream, but the caller should surface the warning to the user.
    async fn complete_externally_merged_session(&self, session_id: &str) -> Option<String> {
        let Ok(session) = self.sessions.session_or_err(session_id) else {
            return None;
        };
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return None;
        };

        let folder = session.folder.clone();
        let source_branch = crate::app::session::session_branch(session_id);

        let cleanup_warning = crate::app::session::SessionManager::cleanup_merged_session_worktree(
            folder,
            self.services.fs_client(),
            self.services.git_client(),
            source_branch,
            None,
        )
        .await
        .err()
        .map(|error| format!("Worktree cleanup failed: {error}"));

        let app_event_tx = self.services.event_sender();

        SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.clock().as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Done,
        )
        .await;

        cleanup_warning
    }

    /// Transitions one externally closed review session to `Canceled`.
    async fn cancel_externally_closed_session(&self, session_id: &str) {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = self.services.event_sender();

        let _ = SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.clock().as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Canceled,
        )
        .await;
    }

    /// Builds a session-view info popup mode with explicit loading metadata.
    pub(super) fn view_info_popup_mode(
        title: String,
        message: String,
        is_loading: bool,
        loading_label: String,
        restore_view: ConfirmationViewMode,
    ) -> AppMode {
        AppMode::ViewInfoPopup {
            is_loading,
            loading_label,
            message,
            restore_view,
            title,
        }
    }

    /// Returns the loading popup title for one branch-publish action.
    pub(super) fn branch_publish_loading_title(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_loading_title_text(publish_branch_action)
    }

    /// Returns the loading popup body for one branch-publish action.
    pub(super) fn branch_publish_loading_message(
        publish_branch_action: PublishBranchAction,
        remote_branch_name: Option<&str>,
    ) -> String {
        branch_publish_loading_message_text(publish_branch_action, remote_branch_name)
    }

    /// Returns the loading spinner label for one branch-publish action.
    pub(super) fn branch_publish_loading_label(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_loading_label_text(publish_branch_action)
    }

    /// Returns the success popup title for a completed branch-publish action.
    pub(super) fn branch_publish_success_title(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_success_title_text(publish_branch_action)
    }

    /// Returns the success popup body for one completed branch push.
    pub(super) fn branch_publish_success_message(
        branch_name: &str,
        review_request_creation: Option<&crate::app::branch_publish::ReviewRequestCreationInfo>,
    ) -> String {
        crate::app::branch_publish::branch_push_success_message(
            branch_name,
            review_request_creation,
        )
    }

    /// Returns the success popup title for one completed review-request
    /// publish.
    pub(super) fn review_request_publish_success_title(
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        crate::app::branch_publish::review_request_publish_success_title(review_request)
    }

    /// Returns the success popup body for one completed review-request
    /// publish.
    pub(super) fn pull_request_publish_success_message(
        branch_name: &str,
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        pull_request_publish_success_message_text(branch_name, review_request)
    }

    /// Builds final sync popup mode from background sync completion result.
    ///
    /// Authentication-related push failures are normalized to actionable
    /// authorization guidance so users can recover quickly.
    pub(super) fn sync_main_popup_mode(
        sync_main_result: Result<SyncMainOutcome, SyncSessionStartError>,
        sync_popup_context: &SyncPopupContext,
    ) -> AppMode {
        match sync_main_result {
            Ok(sync_main_outcome) => AppMode::SyncBlockedPopup {
                project_name: Some(sync_popup_context.project_name.clone()),
                default_branch: Some(sync_popup_context.default_branch.clone()),
                is_loading: false,
                message: Self::sync_success_message(&sync_main_outcome),
                title: "Sync complete".to_string(),
            },
            Err(sync_error @ SyncSessionStartError::MainHasUncommittedChanges { .. }) => {
                AppMode::SyncBlockedPopup {
                    project_name: Some(sync_popup_context.project_name.clone()),
                    default_branch: Some(sync_popup_context.default_branch.clone()),
                    is_loading: false,
                    message: sync_error.detail_message(),
                    title: "Sync blocked".to_string(),
                }
            }
            Err(sync_error @ SyncSessionStartError::Other(_)) => AppMode::SyncBlockedPopup {
                project_name: Some(sync_popup_context.project_name.clone()),
                default_branch: Some(sync_popup_context.default_branch.clone()),
                is_loading: false,
                message: Self::sync_failure_message(&sync_error),
                title: "Sync failed".to_string(),
            },
        }
    }

    /// Builds success copy for sync completion with pull/push/conflict metrics
    /// rendered as markdown sections with empty lines separating pull, push,
    /// and conflict blocks.
    fn sync_success_message(sync_main_outcome: &SyncMainOutcome) -> String {
        let pulled_summary = Self::sync_commit_summary("pulled", sync_main_outcome.pulled_commits);
        let pulled_titles =
            Self::sync_pulled_commit_titles_summary(&sync_main_outcome.pulled_commit_titles);
        let pushed_titles =
            Self::sync_pushed_commit_titles_summary(&sync_main_outcome.pushed_commit_titles);
        let pushed_summary = Self::sync_commit_summary("pushed", sync_main_outcome.pushed_commits);
        let conflict_summary =
            Self::sync_conflict_summary(&sync_main_outcome.resolved_conflict_files);

        sync_blocked::format_sync_success_message(
            &pulled_summary,
            &pulled_titles,
            &pushed_summary,
            &pushed_titles,
            &conflict_summary,
        )
    }

    /// Returns pulled commit titles formatted as an indented list.
    fn sync_pulled_commit_titles_summary(pulled_commit_titles: &[String]) -> String {
        if pulled_commit_titles.is_empty() {
            return String::new();
        }

        pulled_commit_titles
            .iter()
            .map(|title| format!("  - {title}"))
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Returns pushed commit titles formatted as an indented list.
    fn sync_pushed_commit_titles_summary(pushed_commit_titles: &[String]) -> String {
        if pushed_commit_titles.is_empty() {
            return String::new();
        }

        pushed_commit_titles
            .iter()
            .map(|title| format!("  - {title}"))
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Returns sync failure copy with actionable guidance for auth failures.
    ///
    /// Authentication failures show a dismiss-only message so users can fix
    /// credentials first, then restart sync from the list. When the failing
    /// remote host is recognizable, the guidance names the matching forge CLI.
    fn sync_failure_message(sync_error: &SyncSessionStartError) -> String {
        let detail_message = sync_error.detail_message();
        if !is_git_push_authentication_error(&detail_message) {
            return detail_message;
        }

        git_push_authentication_message(
            detected_forge_kind_from_git_push_error(&detail_message),
            "run sync again",
        )
    }

    /// Returns one brief pull/push sentence fragment for sync completion.
    fn sync_commit_summary(direction: &str, commit_count: Option<u32>) -> String {
        match commit_count {
            Some(1) => format!("1 commit {direction}"),
            Some(commit_count) => format!("{commit_count} commits {direction}"),
            None => format!("commits {direction}: unknown"),
        }
    }

    /// Returns one brief conflict-resolution sentence fragment for sync
    /// completion.
    fn sync_conflict_summary(resolved_conflict_files: &[String]) -> String {
        if resolved_conflict_files.is_empty() {
            return "no conflicts fixed".to_string();
        }

        format!("conflicts fixed: {}", resolved_conflict_files.join(", "))
    }
}
