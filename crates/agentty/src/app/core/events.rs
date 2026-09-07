//! Event types and reducer helpers for the app core module.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::future::poll_fn;
use std::path::PathBuf;
use std::sync::Arc;
use std::task::Poll;

use app::branch_publish::{
    BranchPublishActionUpdate, BranchPublishTaskResult, BranchPublishTaskSuccess,
    branch_publish_loading_label as branch_publish_loading_label_text,
    branch_publish_success_title as branch_publish_success_title_text,
    detected_forge_kind_from_git_push_error, git_push_authentication_message,
    is_git_push_authentication_error,
    review_request_created_notice as review_request_created_notice_text,
};
use app::reducer::AppEventReducer;
use app::review::{
    FocusedReviewPersistence, FocusedReviewPersistenceRetry, ReviewUpdate, apply_review_updates,
};
use tracing::warn;

use super::state::{App, SyncReviewRequestTaskResult, UpdateStatus};
use crate::app::session::{
    SessionTaskService, StatusTransition, SyncSessionStartError, TurnAppliedState,
};
use crate::app::session_state::SessionGitStatus;
use crate::app::sync::{
    ProjectSyncContext, ProjectSyncPhase, ProjectSyncStatus, SyncMainCompletion,
    SyncMainReviewUpdate,
};
use crate::app::{self, SessionRuntimeCommand};
use crate::domain::agent::AgentCliInfo;
use crate::domain::file_entry::{FileEntry, at_mention_lookup_root};
use crate::domain::input::InputState;
use crate::domain::question::default_option_index;
use crate::domain::session::{
    PublishBranchAction, PublishedBranchSyncStatus, Session, SessionDiffStats, SessionHandles,
    SessionId, Status,
};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::TransientMessageBody;
use crate::infra::db::DbError;
use crate::presentation::app_mode::{
    AppMode, ChatFocus, ConfirmationViewMode, DiffPreview, DiffPreviewUnavailableReason,
    DiffReviewComments, HelpContext,
};
#[cfg(test)]
use crate::presentation::app_mode::{DiffSidebarFocus, ReviewCommentSelection};
use crate::presentation::prompt::PromptAtMentionState;
use crate::presentation::review_comment as review_comment_selection;

/// Next foreground-owned runtime event accepted by the app.
pub(crate) enum AppRuntimeEvent {
    /// One event emitted by a background workflow.
    App(Box<AppEvent>),
    /// One API command accepted by the bounded session actor mailbox.
    Session(SessionRuntimeCommand),
}

/// Internal app events emitted by background workers and workflows.
///
/// Producers should emit events only; state mutation is centralized in
/// [`App::apply_app_events`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    /// Result of external session creation, acknowledged after snapshot
    /// refresh.
    SessionCreationCompleted {
        request_id: String,
        result: Result<String, String>,
    },
    /// Indicates background-loaded prompt at-mention entries for one session.
    AtMentionEntriesLoaded {
        entries: Vec<FileEntry>,
        session_id: SessionId,
    },
    /// Indicates completion of one bounded diff-preview worktree read.
    DiffPreviewLoaded {
        /// Selected repository-relative markdown path.
        path: String,
        /// Request generation used to reject stale completions.
        request_id: u64,
        /// Bounded worktree-file result.
        result: Result<ag_git::WorktreeFileContent, String>,
        /// Session whose diff preview requested the file.
        session_id: SessionId,
    },
    /// Indicates completion of one background full session-diff load.
    SessionDiffLoaded {
        /// Request generation used to reject stale completions.
        request_id: u64,
        /// Full diff text or a user-facing load failure.
        result: Result<String, String>,
        /// Session whose worktree or archive was loaded.
        session_id: SessionId,
    },
    /// Indicates the latest project-branch and session-branch ahead/behind
    /// information from the git status worker.
    GitStatusUpdated {
        /// Sync-context generation used to reject stale completions.
        generation: u64,
        session_statuses: HashMap<SessionId, SessionGitStatus>,
        status: Option<(u32, u32)>,
    },
    /// Indicates whether a newer stable `agentty` release is available.
    VersionAvailabilityUpdated {
        latest_available_version: Option<String>,
    },
    /// Indicates locally available agent CLI versions finished loading.
    AgentCliVersionsUpdated { agent_clis: Vec<AgentCliInfo> },
    /// Indicates progress of the background auto-update.
    UpdateStatusChanged { update_status: UpdateStatus },
    /// Indicates a session agent/model selection has been persisted.
    SessionModelUpdated {
        session_id: SessionId,
        session_agent: crate::domain::agent::AgentSelection,
    },
    /// Indicates a session personality selection has been persisted.
    SessionPersonalityUpdated {
        personality_id: Option<String>,
        session_id: SessionId,
    },
    /// Indicates a session provider permission mode has been persisted.
    SessionPermissionModeUpdated {
        permission_mode: crate::domain::permission::PermissionMode,
        session_id: SessionId,
    },
    /// Indicates a session reasoning-level selection has been persisted.
    SessionReasoningLevelUpdated {
        reasoning_level: crate::domain::agent::ReasoningLevel,
        session_id: SessionId,
    },
    /// Indicates a session response-style selection has been persisted.
    SessionResponseStyleUpdated {
        response_style: crate::domain::agent::ResponseStyle,
        session_id: SessionId,
    },
    /// Indicates a session response-speed selection has been persisted.
    SessionSpeedModeUpdated {
        session_id: SessionId,
        speed_mode: crate::domain::agent::SpeedMode,
    },
    /// Requests a DB-backed session list refresh.
    RefreshSessions,
    /// Requests a DB-backed project list refresh, including aggregate session
    /// counts shown on the projects tab.
    RefreshProjects,
    /// Requests an immediate git-status refresh outside the periodic poll
    /// cadence.
    RefreshGitStatus,
    /// Indicates completion of a linked session review-request comment load.
    SessionReviewCommentSnapshotLoaded {
        /// Request generation used to reject stale completions.
        request_id: u64,
        /// Comment snapshot result from the background forge task.
        result: Result<ag_forge::ReviewCommentSnapshot, String>,
        /// Session whose comments were requested.
        session_id: SessionId,
    },
    /// Indicates compact live thinking text for an in-progress session.
    SessionProgressUpdated {
        progress_message: Option<String>,
        session_id: SessionId,
    },
    /// Indicates completion of a list-mode sync workflow.
    SyncMainCompleted { completion: SyncMainCompletion },
    /// Indicates list-mode sync is resolving rebase conflicts.
    SyncMainConflictResolutionStarted {
        conflicted_files: Vec<String>,
        operation: ProjectSyncContext,
    },
    /// Indicates recomputed diff-derived metadata for one session.
    SessionDiffStatsUpdated {
        diff_stats: SessionDiffStats,
        session_id: SessionId,
    },
    /// Indicates one tracked draft-title generation task reached a terminal
    /// outcome and can be pruned from in-memory task tracking.
    SessionTitleGenerationFinished {
        generation: u64,
        session_id: SessionId,
    },
    /// Indicates completion of a session-view branch-publish action.
    BranchPublishActionCompleted {
        result: Box<BranchPublishTaskResult>,
        session_id: SessionId,
    },
    /// Indicates a queued review-request action resolved without starting.
    BranchPublishActionResolved { session_id: SessionId },
    /// Indicates a queued review-request action has begun executing on its
    /// session worker.
    BranchPublishActionStarted { session_id: SessionId },
    /// Indicates a queued session sync has either started or failed visibly.
    SessionQueuedSyncResolved { session_id: SessionId },
    /// Indicates a session start or resume command began its turn.
    SessionTurnStarted { session_id: SessionId },
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
    /// Retries one automatic focused-review deferral write after a transient
    /// persistence failure.
    DeferredAutoReviewPersistenceRetry {
        retry: crate::app::session_diff::DeferredAutoReviewPersistenceRetry,
    },
    /// Retries one focused-review persistence write that failed while its
    /// cache generation remains current.
    FocusedReviewPersistenceRetry {
        retry: FocusedReviewPersistenceRetry,
    },
    /// Indicates that a session handle snapshot changed in-memory and carries
    /// the latest observable handle version for redraw deduplication.
    SessionUpdated { session_id: SessionId, version: u64 },
    /// Indicates that an agent turn completed and persisted one reducer-ready
    /// projection.
    AgentResponseReceived {
        session_id: SessionId,
        turn_applied_state: TurnAppliedState,
    },
    /// Indicates one review-ready parent turn finished and any materialized
    /// stacked child branches should sync onto the refreshed parent branch.
    StackedParentTurnCompleted { session_id: SessionId },
    /// Indicates one review-ready parent sync finished and any materialized
    /// stacked child branches should sync onto the refreshed parent branch.
    StackedParentSyncCompleted { session_id: SessionId },
    /// Indicates a parent session merged and its materialized children should
    /// run deterministic restack rebases against the parent's former base.
    StackedParentMergeCompleted { child_session_ids: Vec<SessionId> },
    /// Indicates a transient workflow notice changed for one session.
    SessionWorkflowNoticeUpdated {
        notice: String,
        session_id: SessionId,
    },
    /// Indicates that the replaceable child-status loader changed for an
    /// orchestrator.
    SessionOrchestrationProgressUpdated {
        progress: Option<String>,
        session_id: SessionId,
    },
    /// Indicates that one published session branch started or finished a
    /// background auto-push after a completed turn.
    PublishedBranchSyncUpdated {
        /// Durable transcript notice promoted into place for a terminal
        /// operation, or `None` for progress-only updates.
        persistent_notice: Option<String>,
        session_id: SessionId,
        sync_operation_id: String,
        sync_status: PublishedBranchSyncStatus,
    },
    /// Indicates completion of one background review-request status refresh.
    ReviewRequestStatusUpdated {
        /// Sync-context generation used to reject stale completions.
        generation: u64,
        result: Result<SyncReviewRequestTaskResult, String>,
        session_id: SessionId,
    },
}

/// Reduced representation of all app events currently queued for one tick.
#[derive(Default)]
pub(super) struct AppEventBatch {
    pub(super) session_creations: Vec<(String, Result<String, String>)>,
    pub(super) applied_turns: HashMap<SessionId, TurnAppliedState>,
    pub(super) agent_cli_updates: Option<Vec<AgentCliInfo>>,
    pub(super) at_mention_entries_updates: HashMap<SessionId, Vec<FileEntry>>,
    pub(super) branch_publish_action_updates: Vec<BranchPublishActionUpdate>,
    pub(super) branch_publish_resolved_session_ids: HashSet<SessionId>,
    pub(super) branch_publish_started_session_ids: HashSet<SessionId>,
    pub(super) deferred_auto_review_persistence_retries:
        Vec<crate::app::session_diff::DeferredAutoReviewPersistenceRetry>,
    pub(super) diff_preview_updates: Vec<DiffPreviewUpdate>,
    pub(super) focused_review_persistence_retries: Vec<FocusedReviewPersistenceRetry>,
    pub(super) git_status_update: Option<GitStatusBatchUpdate>,
    pub(super) latest_available_version_update: Option<LatestAvailableVersionUpdate>,
    pub(super) published_branch_sync_updates: Vec<(SessionId, PublishedBranchSyncUpdate)>,
    pub(super) review_updates: HashMap<SessionId, ReviewUpdate>,
    pub(super) session_git_status_updates: HashMap<SessionId, SessionGitStatus>,
    pub(super) session_ids: HashSet<SessionId>,
    pub(super) session_update_versions: HashMap<SessionId, u64>,
    pub(super) session_model_updates: HashMap<SessionId, crate::domain::agent::AgentSelection>,
    pub(super) session_orchestration_progress_updates: HashMap<SessionId, Option<String>>,
    pub(super) session_queued_sync_resolved_ids: HashSet<SessionId>,
    pub(super) session_turn_started_ids: HashSet<SessionId>,
    pub(super) session_personality_updates: HashMap<SessionId, Option<String>>,
    pub(super) session_permission_mode_updates:
        HashMap<SessionId, crate::domain::permission::PermissionMode>,
    pub(super) session_reasoning_level_updates:
        HashMap<SessionId, crate::domain::agent::ReasoningLevel>,
    pub(super) session_response_style_updates:
        HashMap<SessionId, crate::domain::agent::ResponseStyle>,
    pub(super) session_speed_mode_updates: HashMap<SessionId, crate::domain::agent::SpeedMode>,
    pub(super) session_progress_updates: HashMap<SessionId, Option<String>>,
    pub(super) session_review_comment_snapshots: Vec<SessionReviewCommentSnapshotUpdate>,
    pub(super) session_diff_stats_updates: HashMap<SessionId, SessionDiffStats>,
    pub(super) session_diff_updates: Vec<crate::app::SessionDiffUpdate>,
    pub(super) stacked_parent_merge_child_rebases: HashSet<SessionId>,
    pub(super) stacked_parent_syncs_completed: HashSet<SessionId>,
    pub(super) stacked_parent_turns_completed: HashSet<SessionId>,
    pub(super) session_title_generation_finished: HashMap<SessionId, u64>,
    pub(super) session_workflow_notice_updates: HashMap<SessionId, Vec<String>>,
    pub(super) should_refresh_git_status: bool,
    /// Whether this batch should reload project list snapshots from
    /// persistence.
    pub(super) should_reload_projects: bool,
    /// Whether this batch should reload session list snapshots from
    /// persistence.
    pub(super) should_reload_sessions: bool,
    pub(super) review_request_status_updates: Vec<ReviewRequestStatusUpdate>,
    pub(super) sync_main_completion: Option<SyncMainCompletion>,
    pub(super) sync_main_conflict: Option<(ProjectSyncContext, Vec<String>)>,
    pub(super) update_status: Option<UpdateStatus>,
}

/// Ordered external effects planned before an app-event batch mutates state.
#[derive(Debug, Eq, PartialEq)]
enum AppEventEffect {
    ReloadSessions,
    ReloadProjects,
    RefreshGitStatus,
    ApplyReviewUpdates(HashMap<SessionId, ReviewUpdate>),
    PersistDeferredAutoReviewTriggers(
        Vec<crate::app::session_diff::DeferredAutoReviewPersistenceRetry>,
    ),
    PersistFocusedReviewUpdates(Vec<FocusedReviewPersistenceRetry>),
}

/// Deterministic state/effect plan derived from one coalesced event batch.
#[derive(Debug, Eq, PartialEq)]
struct AppEventReductionPlan {
    after_snapshot_effects: Vec<AppEventEffect>,
    before_snapshot_effects: Vec<AppEventEffect>,
    changes_observable_state: bool,
}

/// Completed diff-preview file read ready for stale-safe reducer application.
pub(super) struct DiffPreviewUpdate {
    pub(super) path: String,
    pub(super) request_id: u64,
    pub(super) result: Result<ag_git::WorktreeFileContent, String>,
    pub(super) session_id: SessionId,
}

/// Completed review-comment load ready for stale-safe reducer application.
pub(super) struct SessionReviewCommentSnapshotUpdate {
    pub(super) request_id: u64,
    pub(super) result: Result<ag_forge::ReviewCommentSnapshot, String>,
    pub(super) session_id: SessionId,
}

/// Optional aggregate git status payload from the latest status event in one
/// reducer batch.
pub(super) struct GitStatusBatchUpdate {
    /// Sync-context generation that produced this status snapshot.
    generation: u64,
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
    /// Durable notice promoted while retracting the matching loading slot.
    persistent_notice: Option<String>,
    /// Operation identifier used to ignore stale terminal auto-push updates.
    sync_operation_id: String,
    /// Auto-push state carried by this update.
    sync_status: PublishedBranchSyncStatus,
}

/// Completed review-request status refresh payload ready for reducer
/// application.
pub(super) struct ReviewRequestStatusUpdate {
    pub(super) generation: u64,
    pub(super) result: Result<SyncReviewRequestTaskResult, String>,
    pub(super) session_id: SessionId,
}

impl AppEventBatch {
    /// Drains payload-bearing updates into an ordered effect plan without
    /// mutating application state or performing I/O.
    ///
    /// The batch remains available for later reducer phases, but updates
    /// moved into the returned plan must not be read from the batch again.
    fn drain_reduction_plan(&mut self) -> AppEventReductionPlan {
        let mut before_snapshot_effects = Vec::new();
        if self.should_reload_sessions {
            before_snapshot_effects.push(AppEventEffect::ReloadSessions);
        }
        if self.should_reload_projects {
            before_snapshot_effects.push(AppEventEffect::ReloadProjects);
        }
        if self.should_refresh_git_status {
            before_snapshot_effects.push(AppEventEffect::RefreshGitStatus);
        }
        let changes_observable_state = self.should_reload_sessions
            || self.should_reload_projects
            || self.agent_cli_updates.is_some()
            || self.git_status_update.is_some()
            || self.latest_available_version_update.is_some()
            || self.update_status.is_some()
            || !self.applied_turns.is_empty()
            || !self.at_mention_entries_updates.is_empty()
            || !self.branch_publish_action_updates.is_empty()
            || !self.branch_publish_resolved_session_ids.is_empty()
            || !self.branch_publish_started_session_ids.is_empty()
            || !self.diff_preview_updates.is_empty()
            || !self.published_branch_sync_updates.is_empty()
            || !self.review_request_status_updates.is_empty()
            || !self.review_updates.is_empty()
            || !self.session_model_updates.is_empty()
            || !self.session_orchestration_progress_updates.is_empty()
            || !self.session_personality_updates.is_empty()
            || !self.session_permission_mode_updates.is_empty()
            || !self.session_progress_updates.is_empty()
            || !self.session_queued_sync_resolved_ids.is_empty()
            || !self.session_turn_started_ids.is_empty()
            || !self.session_review_comment_snapshots.is_empty()
            || !self.session_reasoning_level_updates.is_empty()
            || !self.session_response_style_updates.is_empty()
            || !self.session_speed_mode_updates.is_empty()
            || !self.session_diff_stats_updates.is_empty()
            || !self.session_diff_updates.is_empty()
            || !self.session_title_generation_finished.is_empty()
            || !self.session_workflow_notice_updates.is_empty()
            || !self.stacked_parent_merge_child_rebases.is_empty()
            || !self.stacked_parent_syncs_completed.is_empty()
            || !self.stacked_parent_turns_completed.is_empty()
            || self.sync_main_conflict.is_some()
            || self.sync_main_completion.is_some();
        let mut after_snapshot_effects = (!self.review_updates.is_empty())
            .then(|| AppEventEffect::ApplyReviewUpdates(std::mem::take(&mut self.review_updates)))
            .into_iter()
            .collect::<Vec<_>>();
        if !self.deferred_auto_review_persistence_retries.is_empty() {
            after_snapshot_effects.push(AppEventEffect::PersistDeferredAutoReviewTriggers(
                std::mem::take(&mut self.deferred_auto_review_persistence_retries),
            ));
        }
        if !self.focused_review_persistence_retries.is_empty() {
            after_snapshot_effects.push(AppEventEffect::PersistFocusedReviewUpdates(
                std::mem::take(&mut self.focused_review_persistence_retries),
            ));
        }

        AppEventReductionPlan {
            after_snapshot_effects,
            before_snapshot_effects,
            changes_observable_state,
        }
    }

    /// Collects one app event into the coalesced batch state.
    ///
    /// Most per-session projections use latest-wins semantics, but queued
    /// `AgentResponseReceived` events merge token-usage deltas so one reducer
    /// tick preserves cumulative usage from multiple completed turns.
    pub(super) fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::SessionOrchestrationProgressUpdated {
                progress,
                session_id,
            } => {
                self.session_orchestration_progress_updates
                    .insert(session_id, progress);
            }
            AppEvent::SessionCreationCompleted { request_id, result } => {
                self.session_creations.push((request_id, result));
            }
            AppEvent::GitStatusUpdated {
                generation,
                session_statuses,
                status,
            } => self.collect_git_status_updated(generation, session_statuses, status),
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            } => self.collect_version_availability_updated(latest_available_version),
            AppEvent::AgentCliVersionsUpdated { agent_clis } => {
                self.agent_cli_updates = Some(agent_clis);
            }
            AppEvent::UpdateStatusChanged { update_status } => {
                self.update_status = Some(update_status);
            }
            AppEvent::SessionPersonalityUpdated {
                personality_id,
                session_id,
            } => {
                self.session_personality_updates
                    .insert(session_id, personality_id);
            }
            AppEvent::SessionPermissionModeUpdated {
                permission_mode,
                session_id,
            } => {
                self.session_permission_mode_updates
                    .insert(session_id, permission_mode);
            }
            AppEvent::SessionReasoningLevelUpdated {
                reasoning_level,
                session_id,
            } => {
                self.session_reasoning_level_updates
                    .insert(session_id, reasoning_level);
            }
            AppEvent::SessionResponseStyleUpdated {
                response_style,
                session_id,
            } => {
                self.session_response_style_updates
                    .insert(session_id, response_style);
            }
            AppEvent::SessionSpeedModeUpdated {
                session_id,
                speed_mode,
            } => {
                self.session_speed_mode_updates
                    .insert(session_id, speed_mode);
            }
            AppEvent::RefreshSessions => self.should_reload_sessions = true,
            AppEvent::RefreshProjects => self.should_reload_projects = true,
            AppEvent::RefreshGitStatus => self.should_refresh_git_status = true,
            event => self.collect_runtime_event(event),
        }
    }

    /// Collects session, workflow, and runtime events after top-level app
    /// refresh events have been handled.
    fn collect_runtime_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => {
                self.at_mention_entries_updates.insert(session_id, entries);
            }
            AppEvent::SessionModelUpdated {
                session_id,
                session_agent,
            } => {
                self.session_model_updates.insert(session_id, session_agent);
            }
            AppEvent::DiffPreviewLoaded {
                path,
                request_id,
                result,
                session_id,
            } => self.diff_preview_updates.push(DiffPreviewUpdate {
                path,
                request_id,
                result,
                session_id,
            }),
            AppEvent::SessionDiffLoaded {
                request_id,
                result,
                session_id,
            } => self.collect_session_diff_loaded(request_id, result, session_id),
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => {
                self.session_progress_updates
                    .insert(session_id, progress_message);
            }
            AppEvent::SessionReviewCommentSnapshotLoaded {
                request_id,
                result,
                session_id,
            } => self.collect_session_review_comment_snapshot(request_id, result, session_id),
            AppEvent::SyncMainCompleted { completion } => {
                self.collect_sync_main_completed(completion);
            }
            AppEvent::SyncMainConflictResolutionStarted {
                conflicted_files,
                operation,
            } => self.collect_sync_main_conflict(operation, conflicted_files),
            AppEvent::SessionDiffStatsUpdated {
                diff_stats,
                session_id,
            } => {
                self.session_diff_stats_updates
                    .insert(session_id, diff_stats);
            }
            AppEvent::SessionTitleGenerationFinished {
                generation,
                session_id,
            } => {
                self.session_title_generation_finished
                    .insert(session_id, generation);
            }
            event @ (AppEvent::BranchPublishActionCompleted { .. }
            | AppEvent::BranchPublishActionResolved { .. }
            | AppEvent::BranchPublishActionStarted { .. }
            | AppEvent::SessionQueuedSyncResolved { .. }
            | AppEvent::SessionTurnStarted { .. }
            | AppEvent::ReviewPrepared { .. }
            | AppEvent::ReviewPreparationFailed { .. }
            | AppEvent::DeferredAutoReviewPersistenceRetry { .. }
            | AppEvent::FocusedReviewPersistenceRetry { .. }
            | AppEvent::SessionUpdated { .. }
            | AppEvent::AgentResponseReceived { .. }
            | AppEvent::StackedParentTurnCompleted { .. }
            | AppEvent::StackedParentSyncCompleted { .. }
            | AppEvent::StackedParentMergeCompleted { .. }
            | AppEvent::SessionWorkflowNoticeUpdated { .. }
            | AppEvent::PublishedBranchSyncUpdated { .. }
            | AppEvent::ReviewRequestStatusUpdated { .. }) => self.collect_workflow_event(event),
            AppEvent::SessionOrchestrationProgressUpdated { .. }
            | AppEvent::SessionCreationCompleted { .. }
            | AppEvent::GitStatusUpdated { .. }
            | AppEvent::VersionAvailabilityUpdated { .. }
            | AppEvent::AgentCliVersionsUpdated { .. }
            | AppEvent::UpdateStatusChanged { .. }
            | AppEvent::SessionPersonalityUpdated { .. }
            | AppEvent::SessionPermissionModeUpdated { .. }
            | AppEvent::SessionReasoningLevelUpdated { .. }
            | AppEvent::SessionResponseStyleUpdated { .. }
            | AppEvent::SessionSpeedModeUpdated { .. }
            | AppEvent::RefreshSessions
            | AppEvent::RefreshProjects
            | AppEvent::RefreshGitStatus => {
                unreachable!("top-level app event should be collected before runtime events")
            }
        }
    }

    /// Collects one background full-diff completion for foreground reduction.
    fn collect_session_diff_loaded(
        &mut self,
        request_id: u64,
        result: Result<String, String>,
        session_id: SessionId,
    ) {
        self.session_diff_updates
            .push(crate::app::SessionDiffUpdate {
                request_id,
                result,
                session_id,
            });
    }

    /// Collects workflow completion and follow-up events into the batch.
    fn collect_workflow_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::BranchPublishActionCompleted { result, session_id } => {
                self.collect_branch_publish_action_completed(*result, session_id);
            }
            AppEvent::BranchPublishActionResolved { session_id } => {
                self.branch_publish_resolved_session_ids.insert(session_id);
            }
            AppEvent::BranchPublishActionStarted { session_id } => {
                self.branch_publish_started_session_ids.insert(session_id);
            }
            AppEvent::SessionQueuedSyncResolved { session_id } => {
                self.session_queued_sync_resolved_ids.insert(session_id);
            }
            AppEvent::SessionTurnStarted { session_id } => {
                self.session_turn_started_ids.insert(session_id);
            }
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
            AppEvent::DeferredAutoReviewPersistenceRetry { retry } => {
                self.deferred_auto_review_persistence_retries.push(retry);
            }
            AppEvent::FocusedReviewPersistenceRetry { retry } => {
                self.focused_review_persistence_retries.push(retry);
            }
            AppEvent::SessionUpdated {
                session_id,
                version,
            } => self.collect_session_updated(session_id, version),
            AppEvent::AgentResponseReceived {
                session_id,
                turn_applied_state,
            } => self.collect_agent_response_received(session_id, turn_applied_state),
            AppEvent::StackedParentTurnCompleted { session_id } => {
                self.stacked_parent_turns_completed.insert(session_id);
            }
            AppEvent::StackedParentSyncCompleted { session_id } => {
                self.stacked_parent_syncs_completed.insert(session_id);
            }
            AppEvent::StackedParentMergeCompleted { child_session_ids } => self
                .stacked_parent_merge_child_rebases
                .extend(child_session_ids),
            AppEvent::SessionWorkflowNoticeUpdated { notice, session_id } => {
                self.collect_session_workflow_notice_updated(session_id, notice);
            }
            AppEvent::PublishedBranchSyncUpdated {
                persistent_notice,
                session_id,
                sync_operation_id,
                sync_status,
            } => self.collect_published_branch_sync_updated(
                session_id,
                sync_operation_id,
                sync_status,
                persistent_notice,
            ),
            AppEvent::ReviewRequestStatusUpdated {
                generation,
                result,
                session_id,
            } => self.collect_review_request_status_updated(generation, result, session_id),
            AppEvent::SessionOrchestrationProgressUpdated { .. }
            | AppEvent::SessionCreationCompleted { .. }
            | AppEvent::AtMentionEntriesLoaded { .. }
            | AppEvent::DiffPreviewLoaded { .. }
            | AppEvent::SessionDiffLoaded { .. }
            | AppEvent::GitStatusUpdated { .. }
            | AppEvent::VersionAvailabilityUpdated { .. }
            | AppEvent::AgentCliVersionsUpdated { .. }
            | AppEvent::UpdateStatusChanged { .. }
            | AppEvent::SessionModelUpdated { .. }
            | AppEvent::SessionPersonalityUpdated { .. }
            | AppEvent::SessionPermissionModeUpdated { .. }
            | AppEvent::SessionReasoningLevelUpdated { .. }
            | AppEvent::SessionResponseStyleUpdated { .. }
            | AppEvent::SessionSpeedModeUpdated { .. }
            | AppEvent::RefreshSessions
            | AppEvent::RefreshProjects
            | AppEvent::RefreshGitStatus
            | AppEvent::SessionReviewCommentSnapshotLoaded { .. }
            | AppEvent::SessionProgressUpdated { .. }
            | AppEvent::SyncMainCompleted { .. }
            | AppEvent::SyncMainConflictResolutionStarted { .. }
            | AppEvent::SessionDiffStatsUpdated { .. }
            | AppEvent::SessionTitleGenerationFinished { .. } => {
                unreachable!("top-level app event should be collected before runtime events")
            }
        }
    }

    /// Stores a workflow notice update and marks its session as touched.
    fn collect_session_workflow_notice_updated(&mut self, session_id: SessionId, notice: String) {
        self.session_ids.insert(session_id.clone());
        self.session_workflow_notice_updates
            .entry(session_id)
            .or_default()
            .push(notice);
    }

    /// Stores the latest git status event for this reducer batch.
    fn collect_git_status_updated(
        &mut self,
        generation: u64,
        session_statuses: HashMap<SessionId, SessionGitStatus>,
        status: Option<(u32, u32)>,
    ) {
        if self
            .git_status_update
            .as_ref()
            .is_none_or(|batched_update| generation >= batched_update.generation)
        {
            self.git_status_update = Some(GitStatusBatchUpdate { generation, status });
            self.session_git_status_updates = session_statuses;
        }
    }

    /// Stores the latest version availability event for this reducer batch.
    fn collect_version_availability_updated(&mut self, latest_available_version: Option<String>) {
        self.latest_available_version_update = Some(LatestAvailableVersionUpdate {
            latest_available_version,
        });
    }

    /// Stores one completed linked review-comment snapshot load.
    fn collect_session_review_comment_snapshot(
        &mut self,
        request_id: u64,
        result: Result<ag_forge::ReviewCommentSnapshot, String>,
        session_id: SessionId,
    ) {
        self.session_review_comment_snapshots
            .push(SessionReviewCommentSnapshotUpdate {
                request_id,
                result,
                session_id,
            });
    }

    /// Stores the latest default-branch sync result for this reducer batch.
    fn collect_sync_main_completed(&mut self, completion: SyncMainCompletion) {
        if completion.result.is_ok() {
            self.should_refresh_git_status = true;
        }

        self.sync_main_completion = Some(completion);
    }

    /// Stores the latest explicit sync conflict phase for this reducer batch.
    fn collect_sync_main_conflict(
        &mut self,
        operation: ProjectSyncContext,
        conflicted_files: Vec<String>,
    ) {
        self.sync_main_conflict = Some((operation, conflicted_files));
    }

    /// Stores one branch-publish action result for this reducer batch.
    fn collect_branch_publish_action_completed(
        &mut self,
        result: BranchPublishTaskResult,
        session_id: SessionId,
    ) {
        if result.is_ok() {
            self.should_refresh_git_status = true;
        }

        self.branch_publish_action_updates
            .push(BranchPublishActionUpdate { result, session_id });
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
        persistent_notice: Option<String>,
    ) {
        if matches!(
            sync_status,
            PublishedBranchSyncStatus::Idle | PublishedBranchSyncStatus::Succeeded
        ) {
            self.should_refresh_git_status = true;
        }

        self.session_ids.insert(session_id.clone());
        self.published_branch_sync_updates.push((
            session_id,
            PublishedBranchSyncUpdate {
                persistent_notice,
                sync_operation_id,
                sync_status,
            },
        ));
    }

    /// Queues one review-request status refresh result for reducer
    /// application.
    fn collect_review_request_status_updated(
        &mut self,
        generation: u64,
        result: Result<SyncReviewRequestTaskResult, String>,
        session_id: SessionId,
    ) {
        self.review_request_status_updates
            .push(ReviewRequestStatusUpdate {
                generation,
                result,
                session_id,
            });
    }

    /// Stores the latest reduced handle version for one touched session.
    fn collect_session_updated(&mut self, session_id: SessionId, version: u64) {
        self.session_ids.insert(session_id.clone());
        self.session_update_versions.insert(session_id, version);
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
    /// This method drains one bounded batch of currently queued app events,
    /// coalesces refresh and git-status updates within that batch, then applies
    /// session-handle sync for touched sessions. Events beyond the per-cycle
    /// budget remain queued so foreground redraws are not starved.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = AppEventReducer::drain(&mut self.event_rx, first_event);
        let mut event_batch = AppEventBatch::default();
        for event in drained_events {
            event_batch.collect_event(event);
        }

        self.apply_app_event_batch(event_batch).await;
    }

    /// Processes one bounded batch of currently queued app events without
    /// waiting.
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
    #[cfg(test)]
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Waits for either a background app event or a session actor command.
    pub(crate) async fn next_runtime_event(&mut self) -> AppRuntimeEvent {
        let event_rx = &mut self.event_rx;
        let sessions = &mut self.sessions;

        tokio::select! {
            event = poll_fn(|context| match event_rx.poll_recv(context) {
                Poll::Ready(Some(event)) => Poll::Ready(event),
                Poll::Ready(None) | Poll::Pending => Poll::Pending,
            }) => AppRuntimeEvent::App(Box::new(event)),
            command = sessions.next_command() => AppRuntimeEvent::Session(command),
        }
    }

    /// Applies one reduced app-event batch to in-memory app state.
    ///
    /// The reducer first records whether the batch changes any render-visible
    /// state, applies global runtime updates, and then synchronizes touched
    /// session snapshots from their live handles. Any touched session that
    /// reached terminal status (`Done`, `Canceled`) then drops its worker queue
    /// so background workers can shut down provider runtimes.
    async fn apply_app_event_batch(&mut self, mut event_batch: AppEventBatch) {
        let sync_generation_for_review_updates = self.sync_handle.current_generation();
        let AppEventReductionPlan {
            after_snapshot_effects,
            before_snapshot_effects,
            changes_observable_state,
        } = event_batch.drain_reduction_plan();
        let mut should_mark_dirty = changes_observable_state;
        let previous_session_states = self.previous_session_states(&event_batch.session_ids);

        should_mark_dirty |=
            self.update_session_redraw_versions(&event_batch.session_update_versions);

        self.apply_app_event_effects(before_snapshot_effects).await;
        self.apply_batch_runtime_updates(&mut event_batch);

        self.apply_batch_session_snapshot_updates(&mut event_batch);
        self.apply_app_event_effects(after_snapshot_effects).await;
        self.complete_session_creations(std::mem::take(&mut event_batch.session_creations))
            .await;

        self.apply_worker_action_transitions(&mut event_batch);

        for branch_publish_action_update in
            std::mem::take(&mut event_batch.branch_publish_action_updates)
        {
            self.apply_branch_publish_action_update(branch_publish_action_update)
                .await;
        }

        self.apply_review_request_status_updates_and_synced_merges(
            &mut event_batch,
            sync_generation_for_review_updates,
        )
        .await;

        self.apply_session_progress_updates(std::mem::take(
            &mut event_batch.session_progress_updates,
        ));
        self.apply_session_review_comment_snapshot_updates(std::mem::take(
            &mut event_batch.session_review_comment_snapshots,
        ));
        let completed_turn_session_ids = event_batch.applied_turns.keys().cloned().collect();
        let completed_review_session_ids =
            Self::completed_review_session_ids(&event_batch.applied_turns);
        self.supersede_review_diff_loads(&completed_turn_session_ids);
        for session_diff_update in std::mem::take(&mut event_batch.session_diff_updates) {
            self.apply_session_diff_update(session_diff_update).await;
        }

        for (session_id, turn_applied_state) in event_batch.applied_turns {
            self.apply_agent_response_received(&session_id, &turn_applied_state);
        }
        for (session_id, sync_update) in event_batch.published_branch_sync_updates {
            self.apply_published_branch_sync_update(&session_id, sync_update)
                .await;
        }

        if let Some((operation, conflicted_files)) = event_batch.sync_main_conflict.as_ref() {
            self.apply_sync_main_conflict_resolution_started(operation, conflicted_files);
        }

        self.sync_touched_sessions(&event_batch.session_ids);
        for (session_id, progress) in
            std::mem::take(&mut event_batch.session_orchestration_progress_updates)
        {
            self.sessions
                .update_orchestration_progress(&session_id, progress);
        }
        for (session_id, notices) in
            std::mem::take(&mut event_batch.session_workflow_notice_updates)
        {
            for notice in notices {
                self.sessions.append_workflow_notice(&session_id, notice);
            }
        }
        self.start_stacked_child_rebases_after_parent_merge(std::mem::take(
            &mut event_batch.stacked_parent_merge_child_rebases,
        ))
        .await;
        let mut turned_parent_session_ids =
            std::mem::take(&mut event_batch.stacked_parent_turns_completed);
        turned_parent_session_ids.extend(std::mem::take(
            &mut event_batch.stacked_parent_syncs_completed,
        ));
        self.start_stacked_child_rebases_after_parent_turns(turned_parent_session_ids)
            .await;
        let mut auto_review_session_ids =
            self.sessions_entering_review(&event_batch.session_ids, &previous_session_states);
        auto_review_session_ids.extend(completed_review_session_ids);
        auto_review_session_ids.extend(
            event_batch
                .session_ids
                .intersection(&self.deferred_auto_review_session_ids)
                .cloned(),
        );
        self.start_or_defer_auto_reviews(&auto_review_session_ids)
            .await;
        app::review::hydrate_review_transients(&self.review_cache, self.sessions.state_mut());

        self.handle_merge_queue_progress(&event_batch.session_ids, &previous_session_states)
            .await;
        self.retain_valid_session_progress_messages();
        self.sessions.retain_active_prompt_outputs();

        if should_mark_dirty {
            self.mark_dirty();
        }
    }

    /// Returns completed turns eligible to trigger an automatic focused
    /// review rather than clarification-question input.
    fn completed_review_session_ids(
        applied_turns: &HashMap<SessionId, TurnAppliedState>,
    ) -> HashSet<SessionId> {
        applied_turns
            .iter()
            .filter(|(_, turn_applied_state)| turn_applied_state.questions.is_empty())
            .map(|(session_id, _)| session_id.clone())
            .collect()
    }

    /// Applies queued worker actions that started or otherwise resolved.
    fn apply_worker_action_transitions(&mut self, event_batch: &mut AppEventBatch) {
        self.apply_branch_publish_starts(std::mem::take(
            &mut event_batch.branch_publish_started_session_ids,
        ));
        self.apply_branch_publish_resolutions(std::mem::take(
            &mut event_batch.branch_publish_resolved_session_ids,
        ));
        self.apply_session_queued_sync_resolutions(std::mem::take(
            &mut event_batch.session_queued_sync_resolved_ids,
        ));
        self.apply_session_turn_starts(std::mem::take(&mut event_batch.session_turn_started_ids));
    }

    /// Replaces queued review-request labels when their worker actions start.
    fn apply_branch_publish_starts(&mut self, session_ids: HashSet<SessionId>) {
        for session_id in session_ids {
            self.sessions.start_branch_publish(
                &session_id,
                Self::branch_publish_loading_label(PublishBranchAction::PublishPullRequest),
            );
        }
    }

    /// Removes queued review-request labels that resolved without starting.
    fn apply_branch_publish_resolutions(&mut self, session_ids: HashSet<SessionId>) {
        for session_id in session_ids {
            self.sessions.resolve_queued_branch_publish(&session_id);
        }
    }

    /// Removes queued-sync labels once work starts or a visible failure lands.
    fn apply_session_queued_sync_resolutions(&mut self, session_ids: HashSet<SessionId>) {
        for session_id in session_ids {
            self.sessions.resolve_queued_session_sync(&session_id);
        }
    }

    /// Clears diff comment drafts when their session turn starts.
    fn apply_session_turn_starts(&mut self, session_ids: HashSet<SessionId>) {
        for session_id in session_ids {
            self.clear_diff_comment_progress(&session_id);
        }
    }

    async fn apply_review_request_status_updates_and_synced_merges(
        &mut self,
        event_batch: &mut AppEventBatch,
        sync_generation: u64,
    ) {
        let review_request_status_updates =
            std::mem::take(&mut event_batch.review_request_status_updates);
        let applied_review_request_status_update = review_request_status_updates
            .iter()
            .any(|update| update.generation == sync_generation);
        for review_request_status_update in review_request_status_updates {
            if review_request_status_update.generation != sync_generation {
                continue;
            }
            self.apply_review_request_status_update(review_request_status_update)
                .await;
        }
        if applied_review_request_status_update {
            self.publish_sync_context();
        }

        if let Some(completion) = event_batch.sync_main_completion.take() {
            self.apply_sync_main_completion(completion).await;
        }
    }

    /// Applies one terminal sync result now or defers project-scoped
    /// reconciliation until its project becomes active again.
    async fn apply_sync_main_completion(&mut self, mut completion: SyncMainCompletion) {
        if !self.is_latest_project_sync_operation(&completion.operation) {
            return;
        }

        if self.projects.active_project_id() == completion.operation.project_id {
            self.reconcile_project_sync_completion(&mut completion)
                .await;
            self.set_project_sync_terminal_status(&completion);
        } else {
            self.set_project_sync_terminal_status(&completion);
            self.pending_project_sync_completions
                .insert(completion.operation.project_id, completion);
        }

        self.resume_base_checkout_work().await;
    }

    /// Reconciles a completed sync that was deferred across a project switch.
    pub(super) async fn apply_pending_project_sync_completion(&mut self) {
        let project_id = self.projects.active_project_id();
        let Some(mut completion) = self.pending_project_sync_completions.remove(&project_id) else {
            return;
        };
        if !self.is_latest_project_sync_operation(&completion.operation) {
            return;
        }

        self.reconcile_project_sync_completion(&mut completion)
            .await;
        self.set_project_sync_terminal_status(&completion);
        self.refresh_sessions_now().await;
    }

    /// Returns whether an operation is still the newest request for its
    /// project. Missing entries support startup state and injected test
    /// completions that predate an in-memory request.
    fn is_latest_project_sync_operation(&self, operation: &ProjectSyncContext) -> bool {
        self.latest_project_sync_operation_ids
            .get(&operation.project_id)
            .is_none_or(|operation_id| *operation_id == operation.operation_id)
    }

    /// Applies review results and merged-session finalization to the owning
    /// project's loaded session snapshot.
    async fn reconcile_project_sync_completion(&mut self, completion: &mut SyncMainCompletion) {
        let Ok(sync_main_outcome) = &mut completion.result else {
            return;
        };

        let review_request_updates = std::mem::take(&mut completion.review_request_updates);
        let did_apply_review_updates = !review_request_updates.is_empty();
        for SyncMainReviewUpdate { result, session_id } in review_request_updates {
            self.apply_review_request_status_update(ReviewRequestStatusUpdate {
                generation: self.sync_handle.current_generation(),
                result,
                session_id,
            })
            .await;
        }
        if did_apply_review_updates {
            self.publish_sync_context();
        }

        let default_branch = sync_main_outcome.default_branch.clone();
        sync_main_outcome.deferred_merged_session_ids = self
            .finalize_merged_sessions_after_main_sync(&default_branch)
            .await;
    }

    /// Replaces the matching running status with a compact terminal phase.
    fn set_project_sync_terminal_status(&mut self, completion: &SyncMainCompletion) {
        let status = self
            .project_sync_status
            .get_or_insert_with(|| ProjectSyncStatus {
                context: completion.operation.clone(),
                phase: ProjectSyncPhase::Running,
            });
        if status.context.operation_id != completion.operation.operation_id
            || status.context.project_id != completion.operation.project_id
        {
            return;
        }

        status.phase = match &completion.result {
            Ok(outcome) => ProjectSyncPhase::Complete {
                deferred_session_count: outcome.deferred_merged_session_ids.len(),
                pulled_commits: outcome.pulled_commits,
                pushed_commits: outcome.pushed_commits,
                resolved_conflict_count: outcome.resolved_conflict_files.len(),
            },
            Err(error @ SyncSessionStartError::MainHasUncommittedChanges { .. }) => {
                ProjectSyncPhase::Blocked {
                    message: error.detail_message(),
                }
            }
            Err(error @ SyncSessionStartError::Other(_)) => ProjectSyncPhase::Failed {
                message: Self::sync_failure_message(error),
            },
        };
        self.schedule_project_sync_status_expiry();
    }

    /// Applies completed linked-session comment loads only while the matching
    /// diff workspace remains visible.
    fn apply_session_review_comment_snapshot_updates(
        &mut self,
        updates: Vec<SessionReviewCommentSnapshotUpdate>,
    ) {
        for SessionReviewCommentSnapshotUpdate {
            request_id,
            result,
            session_id,
        } in updates
        {
            let Some(review_comments) = self.diff_review_comments_for_session(&session_id) else {
                continue;
            };
            if review_comments.request_id != request_id {
                continue;
            }

            review_comments.is_loading_comments = false;
            match result {
                Ok(snapshot) => {
                    review_comment_selection::retain_actionable_selections(
                        &mut review_comments.selected_comments,
                        &snapshot,
                    );
                    review_comments.selected_comment_index =
                        review_comment_selection::retarget_selected_index(
                            review_comments.comment_snapshot.as_ref(),
                            review_comments.selected_comment_index,
                            &snapshot,
                        );
                    review_comments.comment_error = None;
                    review_comments.comment_snapshot = Some(snapshot);
                }
                Err(error) => {
                    review_comments.comment_error =
                        Some(format!("Failed to load review comments: {error}"));
                    review_comments.comment_snapshot = None;
                }
            }
        }
    }

    /// Returns mutable review-comment state for the active diff or its help
    /// overlay when it belongs to `session_id`.
    fn diff_review_comments_for_session(
        &mut self,
        session_id: &SessionId,
    ) -> Option<&mut DiffReviewComments> {
        match &mut self.mode {
            AppMode::Diff {
                review_comments: Some(review_comments),
                session_id: diff_session_id,
                ..
            } if diff_session_id == session_id => Some(review_comments),
            AppMode::Help {
                context:
                    HelpContext::Diff {
                        review_comments: Some(review_comments),
                        session_id: diff_session_id,
                        ..
                    },
                ..
            } if diff_session_id == session_id => Some(review_comments),
            _ => None,
        }
    }

    /// Starts automatic sync rebases for stacked children after their parent
    /// has returned to a review-ready state.
    async fn start_stacked_child_rebases_after_parent_turns(
        &mut self,
        parent_session_ids: HashSet<SessionId>,
    ) {
        for parent_session_id in parent_session_ids {
            let failures = self
                .sessions
                .rebase_stacked_children_after_parent_turn(
                    &self.services,
                    parent_session_id.as_str(),
                )
                .await;
            self.sessions
                .append_stacked_rebase_failure_notices(failures, "Stacked child auto-sync failed");
        }
    }

    /// Starts deterministic sync rebases for children retargeted by a parent
    /// merge.
    async fn start_stacked_child_rebases_after_parent_merge(
        &mut self,
        child_session_ids: HashSet<SessionId>,
    ) {
        if child_session_ids.is_empty() {
            return;
        }

        let mut child_session_ids = child_session_ids.into_iter().collect::<Vec<_>>();
        child_session_ids.sort();

        let failures = self
            .sessions
            .rebase_sessions_after_parent_merge(&self.services, child_session_ids)
            .await;
        self.sessions.append_stacked_rebase_failure_notices(
            failures,
            "Stacked child post-merge sync failed",
        );
    }

    /// Executes the ordered external effects from a pure reduction plan.
    async fn apply_app_event_effects(&mut self, effects: Vec<AppEventEffect>) {
        for effect in effects {
            match effect {
                AppEventEffect::ReloadSessions => self.refresh_sessions_now().await,
                AppEventEffect::ReloadProjects => self.reload_projects().await,
                AppEventEffect::RefreshGitStatus => self.restart_git_status_task(),
                AppEventEffect::ApplyReviewUpdates(review_updates) => {
                    let focused_review_persistence = apply_review_updates(
                        &mut self.review_cache,
                        self.sessions.state_mut(),
                        review_updates,
                    );
                    let ready_session_ids = focused_review_persistence
                        .iter()
                        .filter(|update| {
                            update.status == crate::domain::review::FocusedReviewStatus::Ready
                        })
                        .map(|update| update.session_id.clone())
                        .collect();
                    self.persist_focused_review_updates(focused_review_persistence)
                        .await;
                    self.auto_address_focused_reviews(ready_session_ids);
                }
                AppEventEffect::PersistDeferredAutoReviewTriggers(retries) => {
                    self.persist_deferred_auto_review_retries(retries).await;
                }
                AppEventEffect::PersistFocusedReviewUpdates(persistence_updates) => {
                    self.persist_focused_review_retries(persistence_updates)
                        .await;
                }
            }
        }
    }

    /// Applies reducer-batch payloads that mutate global app runtime state.
    fn apply_batch_runtime_updates(&mut self, event_batch: &mut AppEventBatch) {
        if let Some(agent_clis) = event_batch.agent_cli_updates.take() {
            self.services.replace_available_agent_clis(agent_clis);
        }

        if let Some(git_status_update) = &event_batch.git_status_update
            && git_status_update.generation == self.sync_handle.current_generation()
        {
            self.projects.set_git_status(git_status_update.status);
            self.sessions
                .replace_session_git_statuses(event_batch.session_git_status_updates.clone());
        }

        for diff_preview_update in std::mem::take(&mut event_batch.diff_preview_updates) {
            self.apply_diff_preview_update(&diff_preview_update);
        }

        self.apply_status_bar_updates(
            event_batch.latest_available_version_update.as_ref(),
            event_batch.update_status.take(),
        );
    }

    /// Applies a worktree read only to its still-loading diff selection.
    fn apply_diff_preview_update(&mut self, update: &DiffPreviewUpdate) {
        match &mut self.mode {
            AppMode::Diff {
                preview,
                scroll_cache,
                session_id,
                ..
            } if *session_id == update.session_id => {
                if Self::resolve_diff_preview(preview, update) {
                    *scroll_cache = None;
                }
            }
            AppMode::Help {
                context:
                    HelpContext::Diff {
                        preview,
                        session_id,
                        ..
                    },
                ..
            } if *session_id == update.session_id => {
                Self::resolve_diff_preview(preview, update);
            }
            _ => {}
        }
    }

    /// Resolves one matching loading state into ready or unavailable content.
    fn resolve_diff_preview(preview: &mut DiffPreview, update: &DiffPreviewUpdate) -> bool {
        if !matches!(
            preview,
            DiffPreview::Loading { path, request_id }
                if path == &update.path && *request_id == update.request_id
        ) {
            return false;
        }

        let unavailable = |reason| DiffPreview::Unavailable {
            path: update.path.clone(),
            reason,
            request_id: update.request_id,
        };
        *preview = match &update.result {
            Ok(ag_git::WorktreeFileContent::Text(content)) => DiffPreview::Ready {
                content: content.clone(),
                path: update.path.clone(),
                request_id: update.request_id,
            },
            Ok(ag_git::WorktreeFileContent::Missing) => {
                unavailable(DiffPreviewUnavailableReason::Deleted)
            }
            Ok(ag_git::WorktreeFileContent::Binary) => {
                unavailable(DiffPreviewUnavailableReason::Binary)
            }
            Ok(ag_git::WorktreeFileContent::TooLarge) => {
                unavailable(DiffPreviewUnavailableReason::TooLarge)
            }
            Err(error) => unavailable(DiffPreviewUnavailableReason::LoadFailed(error.clone())),
        };

        true
    }

    /// Synchronizes touched sessions from their runtime handles and drops
    /// worker queues for sessions that reached a terminal status.
    fn sync_touched_sessions(&mut self, session_ids: &HashSet<SessionId>) {
        for session_id in session_ids {
            self.sessions.sync_session_from_handle(session_id);
        }

        self.sessions.clear_terminal_session_workers(session_ids);
    }

    /// Applies status-bar state updates carried by one reducer batch.
    fn apply_status_bar_updates(
        &mut self,
        latest_available_version_update: Option<&LatestAvailableVersionUpdate>,
        update_status: Option<UpdateStatus>,
    ) {
        if let Some(latest_available_version_update) = latest_available_version_update {
            self.latest_available_version
                .clone_from(&latest_available_version_update.latest_available_version);
        }

        if let Some(update_status) = update_status {
            self.update_status = Some(update_status);
        }
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
                    .sessions()
                    .iter()
                    .find(|session| session.id == *session_id)
                    .map(|session| (session_id.clone(), session.status))
            })
            .collect()
    }

    /// Returns touched sessions whose synchronized status newly entered the
    /// review-ready lifecycle.
    fn sessions_entering_review(
        &self,
        session_ids: &HashSet<SessionId>,
        previous_session_states: &HashMap<SessionId, Status>,
    ) -> HashSet<SessionId> {
        session_ids
            .iter()
            .filter(|session_id| {
                previous_session_states
                    .get(*session_id)
                    .is_some_and(|status| !status.allows_review_actions())
                    && self
                        .sessions
                        .session_for_id(session_id)
                        .is_some_and(|session| session.status == Status::Review)
            })
            .cloned()
            .collect()
    }

    /// Updates non-modal sync progress for the matching live operation.
    fn apply_sync_main_conflict_resolution_started(
        &mut self,
        operation: &ProjectSyncContext,
        conflicted_files: &[String],
    ) {
        let Some(status) = self.project_sync_status.as_mut() else {
            return;
        };
        if status.context.operation_id != operation.operation_id
            || status.context.project_id != operation.project_id
            || !status.is_running()
        {
            return;
        }

        status.phase = ProjectSyncPhase::ResolvingConflicts {
            conflicted_file_count: conflicted_files.len(),
        };
    }

    /// Updates the last-seen session-handle versions and returns whether any
    /// carried version is newer than the reduced value already applied.
    fn update_session_redraw_versions(
        &mut self,
        session_update_versions: &HashMap<SessionId, u64>,
    ) -> bool {
        let mut did_change = false;

        for (session_id, version) in session_update_versions {
            let previous_version = self
                .last_seen_session_update_versions
                .insert(session_id.clone(), *version);

            if previous_version != Some(*version) {
                did_change = true;
            }
        }

        did_change
    }

    /// Applies reducer batch updates that mutate cached session snapshots or
    /// auxiliary session-view lookup state.
    fn apply_batch_session_snapshot_updates(&mut self, event_batch: &mut AppEventBatch) {
        for (session_id, session_agent) in std::mem::take(&mut event_batch.session_model_updates) {
            self.sessions
                .apply_session_model_updated(&session_id, session_agent);
        }

        for (session_id, personality_id) in
            std::mem::take(&mut event_batch.session_personality_updates)
        {
            self.sessions
                .apply_session_personality_updated(&session_id, personality_id);
        }

        for (session_id, permission_mode) in
            std::mem::take(&mut event_batch.session_permission_mode_updates)
        {
            self.sessions
                .apply_session_permission_mode_updated(&session_id, permission_mode);
        }

        for (session_id, reasoning_level) in
            std::mem::take(&mut event_batch.session_reasoning_level_updates)
        {
            self.sessions
                .apply_session_reasoning_level_updated(&session_id, reasoning_level);
        }

        for (session_id, response_style) in
            std::mem::take(&mut event_batch.session_response_style_updates)
        {
            self.sessions
                .apply_session_response_style_updated(&session_id, response_style);
        }

        for (session_id, speed_mode) in std::mem::take(&mut event_batch.session_speed_mode_updates)
        {
            self.sessions
                .apply_session_speed_mode_updated(&session_id, speed_mode);
        }

        for (session_id, diff_stats) in std::mem::take(&mut event_batch.session_diff_stats_updates)
        {
            self.sessions
                .apply_session_diff_stats_updated(&session_id, diff_stats);
        }

        for (session_id, generation) in
            std::mem::take(&mut event_batch.session_title_generation_finished)
        {
            self.sessions
                .clear_title_generation_task_if_matches(&session_id, generation);
        }

        for (session_id, entries) in std::mem::take(&mut event_batch.at_mention_entries_updates) {
            let lookup_root = self.at_mention_lookup_root(&session_id);
            self.sessions
                .set_at_mention_index_for_root(lookup_root, entries.clone());

            self.apply_prompt_at_mention_entries(&session_id, entries);
        }
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
    /// The session worker persists clarification questions and the token-usage
    /// delta before sending this
    /// event, so the reducer can apply the exact same projection in memory
    /// without waiting for a forced reload.
    fn apply_agent_response_received(
        &mut self,
        session_id: &str,
        turn_applied_state: &TurnAppliedState,
    ) {
        if !self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.sessions
            .apply_turn_applied_state(session_id, turn_applied_state);
        self.question_progress.remove(session_id);
        let questions = turn_applied_state.questions.clone();
        if questions.is_empty() {
            return;
        }

        let accepts_user_turns = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(Session::accepts_user_turns);
        if accepts_user_turns && self.is_viewing_session(session_id) {
            self.mode = AppMode::Question {
                at_mention_state: None,
                selected_option_index: default_option_index(&questions, 0),
                session_id: session_id.into(),
                questions,
                responses: Vec::new(),
                current_index: 0,
                focus: ChatFocus::Input,
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
            | AppMode::DiffLoading {
                session_id: view_id,
                ..
            }
            | AppMode::Question {
                session_id: view_id,
                ..
            }
            | AppMode::LaunchConfigurationSelector {
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
            | AppMode::SessionCreation { .. }
            | AppMode::StackAppendParentSelection { .. }
            | AppMode::PreCommitHookWarning { .. }
            | AppMode::ProjectSwitcher { .. }
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Help { .. } => false,
        }
    }

    /// Routes one published-branch auto-push update to the matching in-memory
    /// session snapshot.
    async fn apply_published_branch_sync_update(
        &mut self,
        session_id: &str,
        sync_update: PublishedBranchSyncUpdate,
    ) {
        let PublishedBranchSyncUpdate {
            persistent_notice,
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
                let was_applied = self.sessions.finish_published_branch_sync(
                    session_id,
                    &sync_operation_id,
                    persistent_notice.as_deref(),
                );
                if was_applied && let Some(persistent_notice) = persistent_notice {
                    SessionTaskService::persist_workflow_notice(
                        self.services.db(),
                        session_id,
                        &persistent_notice,
                    )
                    .await;
                }
            }
        }
    }

    /// Returns the lookup root for one session's at-mention entries.
    ///
    /// Materialized sessions use their own worktree. An unmaterialized
    /// stacked draft walks its parent chain to the nearest materialized
    /// ancestor so files introduced there remain available before the
    /// intermediate child worktrees are created.
    pub(crate) fn at_mention_lookup_root(&self, session_id: &str) -> PathBuf {
        let project_working_dir = self.working_dir().to_path_buf();
        let mut candidate_session_id = Some(SessionId::from(session_id));
        let mut visited_session_ids = HashSet::new();
        let mut nearest_materialized_folder = None;

        while let Some(ref session_id) = candidate_session_id {
            if !visited_session_ids.insert(session_id.clone()) {
                break;
            }
            let Some(session) = self.sessions.session_for_id(session_id) else {
                break;
            };
            if self.services.fs_client().is_dir(session.folder.clone()) {
                nearest_materialized_folder = Some(session.folder.clone());

                break;
            }
            candidate_session_id.clone_from(&session.parent_session_id);
        }

        let has_materialized_folder = nearest_materialized_folder.is_some();

        at_mention_lookup_root(
            project_working_dir,
            nearest_materialized_folder,
            has_materialized_folder,
        )
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
            self.sessions.state_mut(),
            review_updates,
        );
    }

    /// Persists current focused-review generations and requeues transient
    /// failures through the foreground reducer.
    pub(crate) async fn persist_focused_review_updates(
        &mut self,
        focused_review_persistence: Vec<FocusedReviewPersistence>,
    ) {
        let retries = focused_review_persistence
            .into_iter()
            .map(|persistence_update| {
                self.pending_focused_review_persistence.insert(
                    persistence_update.session_id.clone(),
                    persistence_update.clone(),
                );

                FocusedReviewPersistenceRetry::initial(persistence_update)
            })
            .collect();

        self.persist_focused_review_retries(retries).await;
    }

    /// Applies bounded focused-review persistence attempts while discarding
    /// retries superseded by newer cache state.
    async fn persist_focused_review_retries(
        &mut self,
        focused_review_retries: Vec<FocusedReviewPersistenceRetry>,
    ) {
        for retry in focused_review_retries {
            let persistence_update = retry.persistence_update.clone();
            let is_current_persistence = self
                .pending_focused_review_persistence
                .get(&persistence_update.session_id)
                == Some(&persistence_update);
            let is_current_cache = self
                .review_cache
                .get(&persistence_update.session_id)
                .is_some_and(|cache_entry| cache_entry.matches_persistence(&persistence_update));
            if !is_current_persistence || !is_current_cache {
                self.remove_current_focused_review_persistence(&persistence_update);

                continue;
            }

            let diff_hash = persistence_update
                .diff_hash
                .map(|diff_hash| diff_hash.to_string());

            let result = self
                .services
                .db()
                .sessions()
                .update_session_focused_review(
                    persistence_update.session_id.as_str(),
                    Some(persistence_update.status),
                    diff_hash,
                    persistence_update.text.clone(),
                )
                .await;
            let retry_scheduled = Self::handle_focused_review_persistence_result(
                self.services.event_sender(),
                retry,
                result,
            );
            if !retry_scheduled {
                self.remove_current_focused_review_persistence(&persistence_update);
            }
        }

        app::review::prune_review_cache(
            &mut self.review_cache,
            &self.pending_focused_review_persistence,
            self.sessions.state(),
        );
    }

    /// Clears one settled or stale pending write without removing a newer
    /// focused-review generation for the same session.
    fn remove_current_focused_review_persistence(
        &mut self,
        persistence_update: &FocusedReviewPersistence,
    ) {
        if self
            .pending_focused_review_persistence
            .get(&persistence_update.session_id)
            == Some(persistence_update)
        {
            self.pending_focused_review_persistence
                .remove(&persistence_update.session_id);
        }
    }

    /// Schedules a reducer-owned retry after one focused-review persistence
    /// failure while leaving successful writes settled.
    fn handle_focused_review_persistence_result(
        app_event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
        retry: FocusedReviewPersistenceRetry,
        result: Result<(), DbError>,
    ) -> bool {
        let Err(error) = result else {
            return false;
        };
        let session_id = retry.persistence_update.session_id.clone();
        let Some(retry) = retry.next() else {
            warn!(
                session_id = %session_id,
                error = %error,
                "focused-review persistence retries exhausted; durable orchestration state \
                 will recover the review on restart"
            );

            return false;
        };
        warn!(
            session_id = %session_id,
            retry_attempt = retry.attempt,
            error = %error,
            "failed to persist focused review; scheduling retry"
        );
        app::task::TaskService::spawn_focused_review_persistence_retry(app_event_tx, retry);

        true
    }

    /// Starts focused review generation for sessions that just entered review.
    pub(super) fn auto_start_reviews(&mut self, session_ids: &HashSet<SessionId>) {
        self.start_auto_review_diff_loads(session_ids);
    }

    /// Starts automatic reviews for loaded and inactive-project sessions,
    /// retaining durable triggers when preparation cannot start yet.
    async fn start_or_defer_auto_reviews(&mut self, session_ids: &HashSet<SessionId>) {
        let mut loaded_session_ids = HashSet::new();

        for session_id in session_ids {
            let loaded_status = self
                .sessions
                .session_for_id(session_id)
                .map(|session| session.status);
            if loaded_status == Some(Status::InProgress) {
                self.defer_auto_review_session(session_id).await;

                continue;
            }
            if loaded_status.is_some() {
                self.deferred_auto_review_session_ids.remove(session_id);
                loaded_session_ids.insert(session_id.clone());

                continue;
            }

            if !self.start_inactive_auto_review_diff_load(session_id).await {
                self.defer_auto_review_session(session_id).await;
            }
        }

        self.start_auto_review_diff_loads(&loaded_session_ids);
    }

    /// Consumes deferred automatic-review triggers whose sessions were
    /// restored by the latest project switch.
    pub(super) fn resume_deferred_auto_reviews(&mut self, persisted_session_ids: Vec<String>) {
        self.deferred_auto_review_session_ids
            .extend(persisted_session_ids.into_iter().map(SessionId::from));
        let loaded_session_ids = self
            .deferred_auto_review_session_ids
            .iter()
            .filter(|session_id| self.sessions.session_for_id(session_id).is_some())
            .cloned()
            .collect::<HashSet<_>>();

        for session_id in &loaded_session_ids {
            self.deferred_auto_review_session_ids.remove(session_id);
        }

        self.start_auto_review_diff_loads(&loaded_session_ids);
    }

    /// Applies one completed branch-publish action to the session chat.
    pub(super) async fn apply_branch_publish_action_update(
        &mut self,
        branch_publish_action_update: BranchPublishActionUpdate,
    ) {
        let BranchPublishActionUpdate { result, session_id } = branch_publish_action_update;

        match result {
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name,
                review_request_creation,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);

                let result_message = TransientMessageBody::Markdown(format!(
                    "**{}**\n\n{}",
                    Self::branch_publish_success_title(PublishBranchAction::Push),
                    Self::branch_publish_success_message(
                        &branch_name,
                        review_request_creation.as_ref(),
                    )
                ));
                if let Some(persistent_notice) = self
                    .sessions
                    .finish_branch_publish(&session_id, result_message)
                {
                    SessionTaskService::persist_workflow_notice(
                        self.services.db(),
                        &session_id,
                        &persistent_notice,
                    )
                    .await;
                }
            }
            Ok(BranchPublishTaskSuccess::PullRequestPublished {
                review_request,
                upstream_reference,
                ..
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);
                self.sessions
                    .apply_review_request(&session_id, review_request.clone());

                let persistent_notice = Self::review_request_created_notice(&review_request);
                if self
                    .sessions
                    .finish_review_request_publish(&session_id, &persistent_notice)
                {
                    SessionTaskService::persist_workflow_notice(
                        self.services.db(),
                        &session_id,
                        &persistent_notice,
                    )
                    .await;
                }
            }
            Err(failure) => {
                let result_message = TransientMessageBody::Markdown(format!(
                    "**{}**\n\n{}",
                    failure.title, failure.message
                ));
                if let Some(persistent_notice) = self
                    .sessions
                    .finish_branch_publish(&session_id, result_message)
                {
                    SessionTaskService::persist_workflow_notice(
                        self.services.db(),
                        &session_id,
                        &persistent_notice,
                    )
                    .await;
                }
            }
        }
    }

    /// Applies one background review-request status refresh.
    pub(super) async fn apply_review_request_status_update(
        &mut self,
        review_request_status_update: ReviewRequestStatusUpdate,
    ) {
        let ReviewRequestStatusUpdate {
            generation: _,
            result,
            session_id,
        } = review_request_status_update;

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
            crate::app::session::SyncReviewRequestOutcome::Merged {
                session_head_hash, ..
            } => {
                if let Some(warning) = self
                    .record_externally_merged_session(&session_id, session_head_hash)
                    .await
                {
                    self.append_output_for_session(
                        &session_id,
                        &TranscriptNotice::ReviewRequestSyncWarning.format(warning),
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

    /// Records one externally merged session as read-only `Merged` without
    /// starting local cleanup or stacked-child restacking.
    pub(super) async fn record_externally_merged_session(
        &self,
        session_id: &str,
        session_head_hash: Option<String>,
    ) -> Option<String> {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return None;
        };
        let mut warnings = Vec::new();

        if let Some(session_head_hash) = session_head_hash
            && let Err(error) = self
                .services
                .db()
                .sessions()
                .update_session_merged_commit_hash(session_id, Some(session_head_hash))
                .await
        {
            warnings.push(format!("Merged commit hash persistence failed: {error}"));
        }

        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        if !status_transition.apply(Status::Merged).await {
            warnings.push("Could not mark the merged session read-only".to_string());
        }

        (!warnings.is_empty()).then(|| warnings.join("\n"))
    }

    /// Finalizes read-only merged sessions that reached the branch updated by
    /// a successful user-triggered main sync.
    ///
    /// A merged stacked child also reached that branch when its merged parent
    /// targeted the branch directly, even if the child's persisted review
    /// target still names the parent review branch.
    async fn finalize_merged_sessions_after_main_sync(
        &mut self,
        default_branch: &str,
    ) -> Vec<SessionId> {
        let mut deferred_session_ids = Vec::new();
        let merged_session_ids = self
            .sessions
            .sessions()
            .iter()
            .filter(|session| {
                self.sessions
                    .session_handles_or_err(&session.id)
                    .ok()
                    .and_then(|handles| handles.status.lock().ok().map(|status| *status))
                    == Some(Status::Merged)
            })
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        let mut session_ids = Vec::new();
        for session_id in merged_session_ids {
            match self
                .merged_session_reached_synced_branch(&session_id, default_branch)
                .await
            {
                Ok(true) => session_ids.push(session_id),
                Ok(false) => {}
                Err(error) => {
                    self.append_output_for_session(
                        &session_id,
                        &TranscriptNotice::ReviewRequestSyncWarning
                            .format(format!("Durable restack marker load failed: {error}")),
                    )
                    .await;
                    deferred_session_ids.push(session_id);
                }
            }
        }

        for session_id in session_ids {
            let session_head_hash = match self
                .services
                .db()
                .sessions()
                .load_session_merged_commit_hash(&session_id)
                .await
            {
                Ok(session_head_hash) => session_head_hash,
                Err(error) => {
                    self.append_output_for_session(
                        &session_id,
                        &TranscriptNotice::ReviewRequestSyncWarning
                            .format(format!("Merged commit hash load failed: {error}")),
                    )
                    .await;
                    deferred_session_ids.push(session_id);

                    continue;
                }
            };

            if let Some(warning) = self
                .complete_externally_merged_session(&session_id, session_head_hash)
                .await
            {
                self.append_output_for_session(
                    &session_id,
                    &TranscriptNotice::ReviewRequestSyncWarning.format(warning),
                )
                .await;
            }
            if self
                .sessions
                .session_handles_or_err(&session_id)
                .ok()
                .and_then(|handles| handles.status.lock().ok().map(|status| *status))
                == Some(Status::Merged)
            {
                deferred_session_ids.push(session_id);
            }
        }

        deferred_session_ids
    }

    /// Returns whether one merged review is now present on the manually
    /// synchronized branch, directly, through its merged stack parent, or by
    /// the durable restack marker left after that parent was archived.
    ///
    /// Returns an error when the durable marker cannot be loaded.
    async fn merged_session_reached_synced_branch(
        &self,
        session_id: &SessionId,
        default_branch: &str,
    ) -> Result<bool, DbError> {
        let Ok(session) = self.sessions.session_or_err(session_id) else {
            return Ok(false);
        };
        let Some(review_request) = session.review_request.as_ref() else {
            return Ok(false);
        };
        if review_request.summary.target_branch == default_branch {
            return Ok(true);
        }
        if let Some(parent_session_id) = session.parent_session_id.as_ref() {
            let Some(parent_session) = self
                .sessions
                .sessions()
                .iter()
                .find(|candidate| candidate.id == *parent_session_id)
            else {
                return Ok(false);
            };
            let Some(parent_review_request) = parent_session.review_request.as_ref() else {
                return Ok(false);
            };

            return Ok(review_request.summary.target_branch
                == parent_review_request.summary.source_branch
                && parent_review_request.summary.target_branch == default_branch
                && self
                    .sessions
                    .session_handles_or_err(parent_session_id)
                    .ok()
                    .and_then(|handles| handles.status.lock().ok().map(|status| *status))
                    == Some(Status::Merged));
        }
        if session.base_branch != default_branch {
            return Ok(false);
        }

        let stack_base_commit_hash = self
            .services
            .db()
            .sessions()
            .get_session_stack_base_commit_hash(session_id)
            .await?;

        Ok(stack_base_commit_hash.is_some())
    }

    /// Marks one externally merged session `Done` after manual target sync,
    /// persists child restack intent, and returns any finalization warning.
    ///
    /// The session is still moved to `Done` when cleanup fails because the
    /// merge already happened upstream, but the caller should surface the
    /// warning to the user.
    pub(super) async fn complete_externally_merged_session(
        &self,
        session_id: &str,
        session_head_hash: Option<String>,
    ) -> Option<String> {
        let Ok(session) = self.sessions.session_or_err(session_id) else {
            return None;
        };
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return None;
        };
        let mut warnings = Vec::new();

        let folder = session.folder.clone();
        let base_branch = session.base_branch.clone();
        let source_branch = crate::app::session::session_branch(session_id);
        let app_event_tx = self.services.event_sender();

        match crate::app::session::SessionManager::restack_child_sessions_after_parent_merge(
            self.services.db(),
            session_id,
            &base_branch,
            session_head_hash,
        )
        .await
        {
            Ok(child_session_ids) => {
                crate::app::session::SessionManager::emit_stacked_parent_merge_completed(
                    &app_event_tx,
                    child_session_ids,
                );
            }
            Err(error) => {
                return Some(format!("Stacked child restack intent failed: {error}"));
            }
        }

        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let status_applied = status_transition.apply(Status::Done).await;
        if !status_applied {
            warnings.push("Could not archive the merged session".to_string());

            return Some(warnings.join("\n"));
        }
        self.spawn_externally_merged_session_cleanup(session_id, folder, source_branch, handles);

        (!warnings.is_empty()).then(|| warnings.join("\n"))
    }

    /// Removes an externally merged session worktree without delaying terminal
    /// input or redraws, persisting any cleanup warning after the task
    /// finishes.
    fn spawn_externally_merged_session_cleanup(
        &self,
        session_id: &str,
        folder: PathBuf,
        source_branch: String,
        handles: &SessionHandles,
    ) {
        let app_event_tx = self.services.event_sender();
        let db = self.services.db().clone();
        let fs_client = self.services.fs_client();
        let git_client = self.services.git_client();
        let session_id = SessionId::from(session_id);
        let session_update_versions = self.services.session_update_versions();
        let transcript = Arc::clone(&handles.transcript);
        let cleanup_task = tokio::spawn(async move {
            if let Err(error) =
                crate::app::session::SessionManager::cleanup_merged_session_worktree(
                    folder,
                    fs_client,
                    git_client,
                    source_branch,
                    None,
                )
                .await
            {
                let warning = TranscriptNotice::ReviewRequestSyncWarning
                    .format(format!("Worktree cleanup failed: {error}"));
                SessionTaskService::append_workflow_notice(
                    &transcript,
                    &db,
                    &app_event_tx,
                    &session_update_versions,
                    session_id.as_str(),
                    &warning,
                )
                .await;
            }
        });
        self.services.track_cleanup_task(cleanup_task);
    }

    /// Transitions one externally closed review session to `Canceled`.
    async fn cancel_externally_closed_session(&self, session_id: &str) {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };

        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let _ = status_transition.apply(Status::Canceled).await;
        let _ = self
            .sessions
            .cancel_stacked_child_sessions(&self.services, session_id)
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

    /// Returns the inline loading label for one branch-publish action.
    pub(super) fn branch_publish_loading_label(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_loading_label_text(publish_branch_action)
    }

    /// Returns the inline success title for a completed branch-publish action.
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

    /// Returns the durable transcript notice for one completed review-request
    /// publish.
    pub(super) fn review_request_created_notice(
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        review_request_created_notice_text(review_request)
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
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;

    use ag_forge::{
        ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
    };

    use super::*;
    use crate::domain::session::{
        ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    };
    use crate::presentation::app_mode::{DiffFocus, DiffLineComments};

    #[tokio::test]
    async fn merged_branch_eligibility_rejects_incomplete_session_context() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let review_request = ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#23".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/child".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: None,
                target_branch: "wt/parent".to_string(),
                title: "Merged child".to_string(),
                web_url: "https://example.test/pull/23".to_string(),
            },
        };
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id("session-without-review")
                .build(),
        );
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id("child-with-missing-parent")
                .parent_session_id(Some("missing-parent".into()))
                .review_request(Some(review_request.clone()))
                .build(),
        );
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id("parent-without-review")
                .build(),
        );
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id("child-with-unlinked-parent")
                .parent_session_id(Some("parent-without-review".into()))
                .review_request(Some(review_request))
                .build(),
        );

        // Act
        let missing_session = app
            .merged_session_reached_synced_branch(&"missing-session".into(), "main")
            .await
            .expect("missing session eligibility should not fail");
        let session_without_review = app
            .merged_session_reached_synced_branch(&"session-without-review".into(), "main")
            .await
            .expect("unlinked session eligibility should not fail");
        let child_with_missing_parent = app
            .merged_session_reached_synced_branch(&"child-with-missing-parent".into(), "main")
            .await
            .expect("missing parent eligibility should not fail");
        let child_with_unlinked_parent = app
            .merged_session_reached_synced_branch(&"child-with-unlinked-parent".into(), "main")
            .await
            .expect("unlinked parent eligibility should not fail");

        // Assert
        assert!(!missing_session);
        assert!(!session_without_review);
        assert!(!child_with_missing_parent);
        assert!(!child_with_unlinked_parent);
    }

    #[tokio::test]
    async fn test_session_review_comment_result_updates_matching_open_page() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = review_comment_diff_mode("session-id", DiffReviewComments::loading(1));

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Ok(ag_forge::ReviewCommentSnapshot::default()),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    comment_error: None,
                    comment_snapshot: Some(_),
                    is_loading_comments: false,
                    ..
                }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_session_review_comment_refresh_retargets_selected_thread() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let previous_snapshot = review_comment_snapshot([
            review_comment_thread("selected", false),
            review_comment_thread("other", false),
        ]);
        let updated_snapshot = review_comment_snapshot([
            review_comment_thread("selected", true),
            review_comment_thread("other", false),
        ]);
        app.mode = review_comment_diff_mode(
            "session-id",
            DiffReviewComments {
                selected_comments: vec![
                    ReviewCommentSelection {
                        thread_id: "selected".to_string(),
                    },
                    ReviewCommentSelection {
                        thread_id: "other".to_string(),
                    },
                ],
                comment_error: None,
                comment_snapshot: Some(previous_snapshot),
                is_loading_comments: true,
                request_id: 1,
                selected_comment_index: 0,
                sidebar_focus: DiffSidebarFocus::Comments,
            },
        );

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Ok(updated_snapshot),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    ref selected_comments,
                    comment_snapshot: Some(ref snapshot),
                    selected_comment_index: 1,
                    ..
                }),
                ..
            } if review_comment_selection::selected_thread_id(snapshot, 1) == Some("selected")
                && selected_comments == &[ReviewCommentSelection {
                    thread_id: "other".to_string(),
                }]
        ));
    }

    #[tokio::test]
    async fn test_session_review_comment_result_ignores_stale_request() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = review_comment_diff_mode("session-id", DiffReviewComments::loading(2));

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Err("stale failure".to_string()),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    comment_error: None,
                    comment_snapshot: None,
                    is_loading_comments: true,
                    request_id: 2,
                    ..
                }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_session_review_comment_result_ignores_stale_pages_and_surfaces_errors() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Ok(ag_forge::ReviewCommentSnapshot::default()),
            session_id: "closed-session".into(),
        })
        .await;
        app.mode = review_comment_diff_mode("open-session", DiffReviewComments::loading(1));
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Ok(ag_forge::ReviewCommentSnapshot::default()),
            session_id: "stale-session".into(),
        })
        .await;
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Err("authentication failed".to_string()),
            session_id: "open-session".into(),
        })
        .await;

        // Assert
        assert!(app.is_viewing_session("open-session"));
        assert!(!app.is_viewing_session("stale-session"));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    comment_error: Some(ref error),
                    comment_snapshot: None,
                    is_loading_comments: false,
                    ..
                }),
                ..
            } if error == "Failed to load review comments: authentication failed"
        ));
    }

    #[tokio::test]
    async fn diff_loading_mode_is_viewing_its_session() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::DiffLoading {
            fallback_view_scroll_offset: None,
            request_id: 1,
            restore: None,
            session_id: "loading-session".into(),
            sidebar_focus: DiffSidebarFocus::Files,
        };

        // Act
        let is_loading_session_visible = app.is_viewing_session("loading-session");
        let is_other_session_visible = app.is_viewing_session("other-session");

        // Assert
        assert!(is_loading_session_visible);
        assert!(!is_other_session_visible);
    }

    #[tokio::test]
    async fn test_session_review_comment_result_updates_open_diff_help_overlay() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                can_comment: true,
                diff: String::new(),
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                line_comments: DiffLineComments::default(),
                selected_diff_line_index: 0,
                preview: DiffPreview::default(),
                review_comments: Some(Box::new(DiffReviewComments::loading(1))),
                restore: None,
                scroll_offset: 0,
                session_id: "session-id".into(),
            },
            scroll_offset: 0,
        };

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Ok(ReviewCommentSnapshot::default()),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    review_comments: Some(ref review_comments),
                    ..
                },
                ..
            } if matches!(
                **review_comments,
                DiffReviewComments {
                    comment_snapshot: Some(_),
                    is_loading_comments: false,
                    ..
                }
            )
        ));
    }

    /// Builds a comment snapshot from inline thread fixtures.
    fn review_comment_snapshot<const THREAD_COUNT: usize>(
        threads: [ReviewCommentThread; THREAD_COUNT],
    ) -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: Vec::from(threads),
        }
    }

    /// Builds a diff workspace focused on one linked review-comment state.
    fn review_comment_diff_mode(
        session_id: &str,
        mut review_comments: DiffReviewComments,
    ) -> AppMode {
        review_comments.sidebar_focus = DiffSidebarFocus::Comments;

        AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(review_comments),
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            session_id: session_id.into(),
        }
    }

    /// Builds one current or resolved review-comment thread.
    fn review_comment_thread(id: &str, is_resolved: bool) -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "reviewer".to_string(),
                authored_by_current_user: false,
                body: "Review comment".to_string(),
            }],
            id: id.to_string(),
            is_outdated: Some(false),
            is_resolved,
            line: Some(1),
            path: "src/main.rs".to_string(),
            start_line: None,
        }
    }

    #[test]
    fn test_refresh_sessions_batch_sets_only_session_reload_scope() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::RefreshSessions);

        // Assert
        assert!(event_batch.should_reload_sessions);
        assert!(!event_batch.should_reload_projects);
    }

    #[test]
    fn test_refresh_projects_batch_sets_only_project_reload_scope() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::RefreshProjects);

        // Assert
        assert!(event_batch.should_reload_projects);
        assert!(!event_batch.should_reload_sessions);
    }

    #[test]
    fn collect_runtime_event_rejects_top_level_event() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        let panic_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            event_batch.collect_runtime_event(AppEvent::RefreshSessions);
        }));

        // Assert
        assert!(panic_result.is_err());
    }

    #[test]
    fn collect_workflow_event_rejects_runtime_event() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        let panic_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            event_batch.collect_workflow_event(AppEvent::SyncMainConflictResolutionStarted {
                conflicted_files: Vec::new(),
                operation: ProjectSyncContext {
                    default_branch: "main".to_string(),
                    operation_id: 1,
                    project_id: 1,
                    project_name: "agentty".to_string(),
                },
            });
        }));

        // Assert
        assert!(panic_result.is_err());
    }

    #[test]
    fn reduction_plan_orders_external_effects_without_running_them() {
        // Arrange
        let mut event_batch = AppEventBatch {
            should_refresh_git_status: true,
            should_reload_projects: true,
            should_reload_sessions: true,
            ..AppEventBatch::default()
        };

        // Act
        let reduction_plan = event_batch.drain_reduction_plan();

        // Assert
        assert_eq!(
            reduction_plan,
            AppEventReductionPlan {
                after_snapshot_effects: Vec::new(),
                before_snapshot_effects: vec![
                    AppEventEffect::ReloadSessions,
                    AppEventEffect::ReloadProjects,
                    AppEventEffect::RefreshGitStatus,
                ],
                changes_observable_state: true,
            }
        );
    }

    #[test]
    fn reduction_plan_keeps_an_empty_batch_pure_and_invisible() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        let reduction_plan = event_batch.drain_reduction_plan();

        // Assert
        assert_eq!(
            reduction_plan,
            AppEventReductionPlan {
                after_snapshot_effects: Vec::new(),
                before_snapshot_effects: Vec::new(),
                changes_observable_state: false,
            }
        );
    }

    #[test]
    fn reduction_plan_orders_review_persistence_after_snapshot_updates() {
        // Arrange
        let mut event_batch = AppEventBatch::default();
        event_batch.collect_event(AppEvent::ReviewPrepared {
            diff_hash: 42,
            review_text: "review output".to_string(),
            session_id: "session-1".into(),
        });

        // Act
        let reduction_plan = event_batch.drain_reduction_plan();

        // Assert
        assert_eq!(
            reduction_plan.after_snapshot_effects,
            vec![AppEventEffect::ApplyReviewUpdates(HashMap::from([(
                "session-1".into(),
                ReviewUpdate {
                    diff_hash: 42,
                    result: Ok("review output".to_string()),
                },
            )]))]
        );
        assert_eq!(
            reduction_plan.before_snapshot_effects,
            [] as [crate::app::core::events::AppEventEffect; 0]
        );
        assert!(reduction_plan.changes_observable_state);
    }

    /// Applies queued events until the expected session-diff request settles.
    async fn apply_session_diff_request(app: &mut App, expected_request_id: u64) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while app
                .pending_session_diff_requests
                .contains_key(&expected_request_id)
            {
                let event = app
                    .next_app_event()
                    .await
                    .expect("session diff should emit an event");
                app.apply_app_events(event).await;
            }
        })
        .await
        .expect("timed out waiting for session diff");
    }

    /// Queues an event that may share a reducer batch with a session-diff
    /// result.
    fn queue_unrelated_session_progress(app: &App) {
        app.services
            .event_sender()
            .send(AppEvent::SessionProgressUpdated {
                progress_message: Some("Unrelated progress".to_string()),
                session_id: "unrelated-session".into(),
            })
            .expect("unrelated event should queue before the diff result");
    }

    #[tokio::test]
    async fn completed_turn_starts_auto_review_when_project_is_inactive() {
        // Arrange
        let diff_text = "diff --git a/file.rs b/file.rs\n+inactive change";
        let expected_hash = crate::app::diff_content_hash(diff_text);
        let mut git_client = ag_git::MockGitClient::new();
        git_client
            .expect_diff()
            .once()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        let clients = crate::test_support::test_app_clients().with_git_client(Arc::new(git_client));
        let (mut app, base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
        let session_id = SessionId::from("inactive-completed-session");
        let inactive_project_path = base_dir.path().join("inactive-project");
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project(&inactive_project_path.to_string_lossy(), None)
            .await
            .expect("failed to insert inactive project");
        app.services
            .db()
            .sessions()
            .insert_session(
                session_id.as_str(),
                "gpt-5.6-sol",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("failed to insert inactive session");
        app.services
            .db()
            .settings()
            .upsert_project_settings(
                inactive_project_id,
                vec![
                    (
                        crate::domain::setting::SettingName::DefaultReviewAgent,
                        "claude".to_string(),
                    ),
                    (
                        crate::domain::setting::SettingName::DefaultReviewModel,
                        "claude-sonnet-5".to_string(),
                    ),
                    (
                        crate::domain::setting::SettingName::DefaultReviewReasoningLevel,
                        "low".to_string(),
                    ),
                    (
                        crate::domain::setting::SettingName::DefaultReviewSpeedMode,
                        "fast".to_string(),
                    ),
                ],
            )
            .await
            .expect("failed to persist inactive-project review settings");
        let turn_applied_state = TurnAppliedState {
            follow_up_tasks: Vec::new(),
            questions: Vec::new(),
            token_usage_delta: crate::domain::session::SessionStats::default(),
        };

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: session_id.clone(),
            turn_applied_state,
        })
        .await;
        let expected_request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("inactive-project diff should be pending");
        queue_unrelated_session_progress(&app);
        apply_session_diff_request(&mut app, expected_request_id).await;

        // Assert
        assert!(app.deferred_auto_review_session_ids.is_empty());
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(app::review::ReviewCacheEntry::Loading {
                diff_hash,
                review_agent,
            }) if *diff_hash == expected_hash
                && *review_agent == (
                    crate::domain::agent::AgentSelection::new(
                        crate::domain::agent::AgentKind::Claude,
                        crate::domain::agent::AgentModel::ClaudeOpus5,
                    ),
                    crate::domain::agent::ReasoningLevel::Low,
                    crate::domain::agent::SpeedMode::Fast,
                )
        ));
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_pending_focused_review_session_ids(inactive_project_id)
                .await
                .expect("failed to load deferred review"),
            [session_id.as_str()]
        );
    }

    #[tokio::test]
    async fn late_completed_turn_for_deleted_session_is_not_deferred() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_test_app().await;
        let session_id = SessionId::from("deleted-completed-session");
        let inactive_project_path = base_dir.path().join("inactive-project");
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project(&inactive_project_path.to_string_lossy(), None)
            .await
            .expect("failed to insert inactive project");
        app.services
            .db()
            .sessions()
            .insert_session(
                session_id.as_str(),
                "gpt-5.6-sol",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("failed to insert inactive session");
        app.services
            .db()
            .sessions()
            .delete_session(session_id.as_str())
            .await
            .expect("failed to delete inactive session");
        let turn_applied_state = TurnAppliedState {
            follow_up_tasks: Vec::new(),
            questions: Vec::new(),
            token_usage_delta: crate::domain::session::SessionStats::default(),
        };

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id,
            turn_applied_state,
        })
        .await;

        // Assert
        assert!(app.deferred_auto_review_session_ids.is_empty());
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_pending_focused_review_session_ids(inactive_project_id)
                .await
                .expect("failed to load deferred reviews"),
            [] as [String; 0]
        );
    }

    #[tokio::test]
    async fn completed_turn_with_questions_is_not_deferred() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = SessionId::from("inactive-question-session");
        let turn_applied_state = TurnAppliedState {
            follow_up_tasks: Vec::new(),
            questions: vec![crate::domain::question::QuestionItem::new("Which project?")],
            token_usage_delta: crate::domain::session::SessionStats::default(),
        };

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id,
            turn_applied_state,
        })
        .await;

        // Assert
        assert!(app.deferred_auto_review_session_ids.is_empty());
    }

    #[tokio::test]
    async fn completed_focused_review_persists_for_inactive_project() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_test_app().await;
        let inactive_project_path = base_dir.path().join("inactive-project");
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project(&inactive_project_path.to_string_lossy(), None)
            .await
            .expect("failed to insert inactive project");
        let session_id = SessionId::from("inactive-review");
        let review_text = "## Review\nInactive project finding.";
        app.services
            .db()
            .sessions()
            .insert_session(
                session_id.as_str(),
                "gpt-5.6-sol",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("failed to insert inactive review session");
        let review_agent = app.review_agent();
        app.review_cache.insert(
            session_id.clone(),
            app::review::ReviewCacheEntry::Loading {
                diff_hash: 42,
                review_agent,
            },
        );

        // Act
        app.apply_app_events(AppEvent::ReviewPrepared {
            diff_hash: 42,
            review_text: review_text.to_string(),
            session_id: session_id.clone(),
        })
        .await;
        let persisted_reviews = app
            .services
            .db()
            .sessions()
            .load_session_focused_reviews_for_project(inactive_project_id)
            .await
            .expect("failed to load inactive project review");

        // Assert
        assert_eq!(persisted_reviews.len(), 1);
        assert_eq!(persisted_reviews[0].session_id, session_id.as_str());
        assert_eq!(persisted_reviews[0].diff_hash, "42");
        assert_eq!(persisted_reviews[0].text, review_text);
        assert!(!app.review_cache.contains_key(&session_id));
        assert!(app.pending_focused_review_persistence.is_empty());
    }

    #[tokio::test]
    async fn failed_focused_review_persistence_retries_without_replaying_stale_state() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let project_id = app.projects.active_project_id();
        app.services
            .db()
            .sessions()
            .insert_session("session-1", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert review session");
        app.review_cache.insert(
            "session-1".into(),
            app::review::ReviewCacheEntry::Ready {
                diff_hash: 42,
                text: "review output".to_string(),
            },
        );
        let persistence_update = FocusedReviewPersistence {
            diff_hash: Some(42),
            session_id: "session-1".into(),
            status: crate::domain::review::FocusedReviewStatus::Ready,
            text: Some("review output".to_string()),
        };
        app.pending_focused_review_persistence.insert(
            persistence_update.session_id.clone(),
            persistence_update.clone(),
        );
        let (retry_tx, mut retry_rx) = tokio::sync::mpsc::unbounded_channel();
        let retry_scheduled = App::handle_focused_review_persistence_result(
            retry_tx,
            FocusedReviewPersistenceRetry::initial(persistence_update.clone()),
            Err(DbError::Query(sqlx::Error::PoolClosed)),
        );

        // Act
        let retry_event = tokio::time::timeout(std::time::Duration::from_secs(1), retry_rx.recv())
            .await
            .expect("timed out waiting for focused-review persistence retry")
            .expect("focused-review persistence failure should requeue an event");
        app.apply_app_events(retry_event).await;
        let stale_pending = FocusedReviewPersistence {
            status: crate::domain::review::FocusedReviewStatus::Pending,
            text: None,
            ..persistence_update.clone()
        };
        app.apply_app_events(AppEvent::FocusedReviewPersistenceRetry {
            retry: FocusedReviewPersistenceRetry {
                attempt: 1,
                persistence_update: stale_pending,
            },
        })
        .await;
        let persisted = app
            .services
            .db()
            .sessions()
            .load_session_focused_reviews_for_project(project_id)
            .await
            .expect("failed to load retried focused review");
        let (exhausted_tx, mut exhausted_rx) = tokio::sync::mpsc::unbounded_channel();
        let exhausted_retry_scheduled = App::handle_focused_review_persistence_result(
            exhausted_tx,
            FocusedReviewPersistenceRetry {
                attempt: app::review::MAX_FOCUSED_REVIEW_PERSISTENCE_RETRIES,
                persistence_update,
            },
            Err(DbError::Query(sqlx::Error::PoolClosed)),
        );
        let exhausted_event = exhausted_rx.recv().await;

        // Assert
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].session_id, "session-1");
        assert_eq!(persisted[0].diff_hash, "42");
        assert_eq!(persisted[0].text, "review output");
        assert!(retry_scheduled);
        assert!(!exhausted_retry_scheduled);
        assert_eq!(exhausted_event, None);
    }

    #[tokio::test]
    async fn test_diff_preview_events_map_all_worktree_results() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let outcomes = [
            Ok(ag_git::WorktreeFileContent::Text("# Preview".to_string())),
            Ok(ag_git::WorktreeFileContent::Missing),
            Ok(ag_git::WorktreeFileContent::Binary),
            Ok(ag_git::WorktreeFileContent::TooLarge),
            Err("read failed".to_string()),
        ];
        let resolve_diff_state = |mode: &AppMode| match mode {
            AppMode::Diff {
                preview,
                scroll_cache,
                ..
            } => Some((preview.clone(), scroll_cache.is_none())),
            _ => None,
        };

        // Act
        let mut resolved_previews = Vec::new();
        for (request_id, result) in (1_u64..).zip(outcomes) {
            app.mode = AppMode::Diff {
                diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                line_comments: DiffLineComments::default(),
                selected_diff_line_index: 0,
                preview: DiffPreview::Loading {
                    path: "README.md".to_string(),
                    request_id,
                },
                review_comments: None,
                restore: None,
                scroll_cache: Some(crate::presentation::app_mode::DiffScrollCache {
                    content_area: crate::presentation::app_mode::ViewportRect {
                        height: 24,
                        width: 80,
                        x: 0,
                        y: 0,
                    },
                    file_explorer_selected_index: 0,
                    max_scroll_offset: 4,
                }),
                scroll_offset: 2,
                session_id: "session-id".into(),
            };
            app.apply_app_events(AppEvent::DiffPreviewLoaded {
                path: "README.md".to_string(),
                request_id,
                result,
                session_id: "session-id".into(),
            })
            .await;
            let (preview, scroll_cache_cleared) = resolve_diff_state(&app.mode)
                .expect("diff preview result should preserve diff mode");
            assert!(scroll_cache_cleared);
            resolved_previews.push(preview);
        }

        // Assert
        assert!(resolve_diff_state(&AppMode::List).is_none());
        assert_eq!(resolved_previews.len(), 5);
        assert!(matches!(
            &resolved_previews[0],
            DiffPreview::Ready { content, .. } if content == "# Preview"
        ));
        assert!(matches!(
            &resolved_previews[1],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::Deleted,
                ..
            }
        ));
        assert!(matches!(
            &resolved_previews[2],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::Binary,
                ..
            }
        ));
        assert!(matches!(
            &resolved_previews[3],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::TooLarge,
                ..
            }
        ));
        assert!(matches!(
            &resolved_previews[4],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::LoadFailed(error),
                ..
            } if error == "read failed"
        ));
    }

    #[tokio::test]
    async fn test_diff_preview_event_ignores_stale_mode_session_path_and_request() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let loading = || DiffPreview::Loading {
            path: "README.md".to_string(),
            request_id: 4,
        };
        let event = |path: &str, request_id: u64, session_id: &str| AppEvent::DiffPreviewLoaded {
            path: path.to_string(),
            request_id,
            result: Ok(ag_git::WorktreeFileContent::Text("stale".to_string())),
            session_id: session_id.into(),
        };
        let diff_mode = |preview| AppMode::Diff {
            diff: "diff".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            preview,
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            selected_diff_line_index: 0,
            session_id: "session-id".into(),
        };

        // Act
        app.mode = diff_mode(loading());
        app.apply_app_events(event("OTHER.md", 4, "session-id"))
            .await;
        let stale_path_ignored = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Loading { .. },
                ..
            }
        );
        app.mode = diff_mode(loading());
        app.apply_app_events(event("README.md", 5, "session-id"))
            .await;
        let stale_request_ignored = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Loading { .. },
                ..
            }
        );
        app.mode = diff_mode(loading());
        app.apply_app_events(event("README.md", 4, "other-session"))
            .await;
        let stale_session_ignored = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Loading { .. },
                ..
            }
        );
        app.mode = AppMode::List;
        app.apply_app_events(event("README.md", 4, "session-id"))
            .await;

        // Assert
        assert!(stale_path_ignored);
        assert!(stale_request_ignored);
        assert!(stale_session_ignored);
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_diff_preview_event_resolves_while_help_is_open() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                can_comment: true,
                diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                line_comments: DiffLineComments::default(),
                selected_diff_line_index: 0,
                preview: DiffPreview::Loading {
                    path: "README.md".to_string(),
                    request_id: 8,
                },
                review_comments: None,
                restore: None,
                scroll_offset: 0,
                session_id: "session-id".into(),
            },
            scroll_offset: 0,
        };

        // Act
        app.apply_app_events(AppEvent::DiffPreviewLoaded {
            path: "README.md".to_string(),
            request_id: 8,
            result: Ok(ag_git::WorktreeFileContent::Text("# Ready".to_string())),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    preview: DiffPreview::Ready { ref content, .. },
                    ..
                },
                ..
            } if content == "# Ready"
        ));
    }
}
