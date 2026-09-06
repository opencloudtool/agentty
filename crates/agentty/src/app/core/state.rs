//! App state definitions and workflow glue for the app core module.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use ag_agent::{AgentAvailabilityProbe, AppServerClient, RealAgentAvailabilityProbe};
#[cfg(test)]
use ag_forge as forge;
use ag_forge::{RealReviewRequestClient, ReviewRequestClient};
use ag_git::{GitClient, GitError, RealGitClient};
#[cfg(test)]
use app::branch_publish::detected_forge_kind_from_git_push_error;
use app::branch_publish::{
    BranchPublishTaskContext, BranchPublishTaskSession, review_request_queued_label,
    run_branch_publish_action,
};
#[cfg(test)]
use app::branch_publish::{BranchPublishTaskFailure, branch_push_failure, push_session_branch};
use app::merge_queue::{MergeQueue, MergeQueueProgress};
use app::project::ProjectManager;
use app::review::{
    FocusedReviewPersistence, ReviewCacheEntry, mark_session_agent_review, review_failure_message,
    review_loading_message, review_view_text, start_review_assist as spawn_review_assist,
};
use app::service::AppServices;
use app::session::SessionManager;
use app::session_runtime::SessionRuntime;
use app::setting::SettingsManager;
use app::sync::{
    PROJECT_SYNC_STATUS_VISIBLE_DURATION, ProjectSyncContext, ProjectSyncPhase, ProjectSyncStatus,
    SyncMainCompletion, SyncMainRequest, SyncMainRunner,
};
use app::tab::TabManager;
use app::{sync, task};
use session::StatusTransition;
#[cfg(test)]
use session::{SyncMainOutcome, SyncSessionStartError, TurnAppliedState};
use tokio::sync::mpsc;
use tracing::warn;

use super::events::AppEvent;
#[cfg(test)]
use super::events::{AppEventBatch, ReviewRequestStatusUpdate};
use crate::app;
use crate::app::session_diff::PendingSessionDiffRequest;
use crate::app::{AppError, session};
#[cfg(test)]
use crate::domain::agent::AgentCliInfo;
#[cfg(test)]
use crate::domain::agent::AgentSelection;
use crate::domain::agent::{AgentKind, ReasoningLevel};
use crate::domain::input::InputState;
use crate::domain::question::{QuestionItem, QuestionProgress, default_option_index};
use crate::domain::session::{
    FollowUpTaskAction, PublishBranchAction, Session, SessionDiffStats, SessionId, Status,
};
use crate::domain::session_message::SessionTranscript;
use crate::domain::setting::SettingName;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPrompt;
#[cfg(test)]
use crate::infra::db;
use crate::infra::fs::{FsClient, RealFsClient};
use crate::infra::personality::{PersonalityCatalogClient, RealPersonalityCatalogClient};
use crate::infra::project_discovery::{ProjectDiscoveryClient, RealProjectDiscoveryClient};
use crate::infra::tmux::{self, RealTmuxClient, TmuxClient};
use crate::presentation::app_mode::{
    AppMode, ChatFocus, ConfirmationViewMode, DiffLineComments, DiffReviewComments,
    PromptModeSnapshot,
};
use crate::presentation::settings::SettingsPresentationState;

/// Relative directory name used for session git worktrees within the
/// `agentty` home directory.
pub const AGENTTY_WT_DIR: &str = "wt";

/// Background auto-update progress state for the status bar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateStatus {
    /// Background `npm i -g agentty@latest` is running.
    InProgress {
        /// Version currently being installed.
        version: String,
    },
    /// Update installed successfully; restart to use the new version.
    Complete {
        /// Version that was installed.
        version: String,
    },
    /// Update failed; fall back to manual update hint.
    Failed {
        /// Version whose installation failed.
        version: String,
    },
}

/// Source-session context needed to create a seeded continuation draft.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TerminalContinuationDraft {
    /// Base branch copied from the terminal source session.
    base_branch: String,
    /// Persisted project identifier copied from the terminal source session.
    project_id: i64,
    /// Initial draft message that gives the new session prior context.
    prompt_seed: String,
}

/// Background sync task result carrying the normalized summary for
/// persistence alongside the UI outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncReviewRequestTaskResult {
    pub(crate) outcome: session::SyncReviewRequestOutcome,
    /// Normalized summary to persist when a review request was found or
    /// refreshed.
    pub(crate) summary: Option<crate::domain::session::ReviewRequestSummary>,
}

/// External clients used to compose [`App`] startup dependencies.
pub(crate) struct AppClients {
    pub(super) agent_availability_probe: Arc<dyn AgentAvailabilityProbe>,
    /// Whether startup should spawn background CLI version detection.
    pub(super) agent_cli_version_task_enabled: bool,
    pub(super) app_server_client_override: Option<Arc<dyn AppServerClient>>,
    pub(super) fs_client: Arc<dyn FsClient>,
    pub(super) git_client: Arc<dyn GitClient>,
    pub(super) is_tmux_session: bool,
    pub(super) personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    pub(super) project_discovery_client: Arc<dyn ProjectDiscoveryClient>,
    pub(super) review_request_client: Arc<dyn ReviewRequestClient>,
    pub(super) sync_main_runner: Option<Arc<dyn SyncMainRunner>>,
    pub(super) tmux_client: Arc<dyn TmuxClient>,
}

impl AppClients {
    /// Builds one client bundle with real implementations for each external
    /// boundary.
    pub(crate) fn new() -> Self {
        Self {
            agent_availability_probe: Arc::new(RealAgentAvailabilityProbe),
            agent_cli_version_task_enabled: !cfg!(test),
            app_server_client_override: None,
            fs_client: Arc::new(RealFsClient),
            git_client: Arc::new(RealGitClient),
            is_tmux_session: tmux::is_tmux_session(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            project_discovery_client: Arc::new(RealProjectDiscoveryClient),
            review_request_client: Arc::new(RealReviewRequestClient::default()),
            sync_main_runner: None,
            tmux_client: Arc::new(RealTmuxClient),
        }
    }

    /// Replaces the startup agent-availability boundary while preserving the
    /// remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_agent_availability_probe(
        mut self,
        agent_availability_probe: Arc<dyn AgentAvailabilityProbe>,
    ) -> Self {
        self.agent_availability_probe = agent_availability_probe;

        self
    }

    /// Replaces the default provider-owned app-server clients with one shared
    /// override.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_app_server_client_override(
        mut self,
        app_server_client_override: Arc<dyn AppServerClient>,
    ) -> Self {
        self.app_server_client_override = Some(app_server_client_override);

        self
    }

    /// Replaces the git boundary for deterministic app tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_git_client(mut self, git_client: Arc<dyn GitClient>) -> Self {
        self.git_client = git_client;

        self
    }

    /// Replaces the personality catalog boundary for deterministic app tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_personality_catalog_client(
        mut self,
        personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    ) -> Self {
        self.personality_catalog_client = personality_catalog_client;

        self
    }

    /// Replaces the startup project-discovery boundary while preserving the
    /// remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_project_discovery_client(
        mut self,
        project_discovery_client: Arc<dyn ProjectDiscoveryClient>,
    ) -> Self {
        self.project_discovery_client = project_discovery_client;

        self
    }

    /// Replaces the tmux boundary while preserving the remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_tmux_client(mut self, tmux_client: Arc<dyn TmuxClient>) -> Self {
        self.is_tmux_session = true;
        self.tmux_client = tmux_client;

        self
    }

    /// Overrides whether the test app is treated as running inside `tmux`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_tmux_session(mut self, is_tmux_session: bool) -> Self {
        self.is_tmux_session = is_tmux_session;

        self
    }
}

// SessionState definition moved to session_state.rs

/// Stores application state and coordinates session/project workflows.
pub struct App {
    /// Tracks the currently active UI mode and its transient state.
    pub mode: AppMode,
    /// Tracks whether the foreground runtime should render a fresh frame.
    pub(crate) needs_redraw: bool,
    /// Stores persisted and in-memory application settings for the active
    /// project.
    pub settings: SettingsManager,
    /// Owns frontend-neutral selection and editor state for the settings tab.
    pub(crate) settings_presentation: SettingsPresentationState,
    /// Manages the selected top-level list tab.
    pub tabs: TabManager,
    /// Saves prompt composers per session so leaving chat focus with `q` and
    /// reopening the session restores the complete typed draft. Entries are
    /// consumed on restore and removed when their session is deleted.
    pub(crate) prompt_progress: HashMap<SessionId, PromptModeSnapshot>,
    /// Saves completed file and inline comments per session while Diff mode is
    /// closed. Entries are consumed when the diff reopens and cleared when a
    /// turn starts or the session is deleted.
    pub(crate) diff_comment_progress: HashMap<SessionId, DiffLineComments>,
    /// Saves partially answered clarification progress per session so
    /// already-submitted answers survive leaving question mode with `q` and
    /// reopening the session. Entries are consumed on restore and cleared
    /// when a new turn result replaces the session's question list.
    pub(crate) question_progress: HashMap<SessionId, QuestionProgress>,
    /// Records the session for which `reconcile_open_session_question_mode`
    /// already reloaded detail and still found no persisted questions, so the
    /// per-frame reconciliation does not reissue that database load every
    /// render cycle. Cleared once the open view leaves `Status::Question` or
    /// `AppMode::View`, or once the panel opens, so a later legitimate
    /// transition reloads again.
    pub(crate) question_reconcile_reload_attempted: Option<SessionId>,
    /// Caches generated focused review text per session so it survives mode
    /// switches, is hydrated after restart, and is ready when the user presses
    /// `f`.
    pub(crate) review_cache: HashMap<SessionId, ReviewCacheEntry>,
    /// Counts automatic focused-review remediation turns in the current
    /// user-initiated cycle for each session.
    pub(crate) auto_address_review_iterations: HashMap<SessionId, u8>,
    /// Version label projected to the status bar by frontend snapshots.
    pub(crate) current_version_display_text: String,
    /// Retains automatic focused-review triggers for completed sessions whose
    /// owning project is not currently loaded.
    pub(crate) deferred_auto_review_session_ids: HashSet<SessionId>,
    /// Retains focused-review cache generations until their durable writes
    /// settle so project-scoped refreshes cannot discard off-project output.
    pub(crate) pending_focused_review_persistence: HashMap<SessionId, FocusedReviewPersistence>,
    /// Tracks background session-diff loads by request generation so stale
    /// completions cannot change the active mode or review generation.
    pub(crate) pending_session_diff_requests: HashMap<u64, PendingSessionDiffRequest>,
    /// Records the newest explicit sync operation requested for each project
    /// so delayed completions cannot apply superseded reconciliation work.
    pub(crate) latest_project_sync_operation_ids: HashMap<i64, u64>,
    /// Retains completed sync reconciliation until its owning project is
    /// active.
    pub(crate) pending_project_sync_completions: HashMap<i64, SyncMainCompletion>,
    /// Retains explicit sync requests for other projects while one sync or a
    /// base-checkout merge owns the foreground mutation slot.
    pub(crate) pending_project_sync_requests: VecDeque<SyncMainRequest>,
    /// Owns project selection state, project metadata, and git status
    /// snapshots.
    pub(crate) projects: ProjectManager,
    /// Shares application-wide services and external clients across workflows.
    pub(crate) services: AppServices,
    /// Owns session state, worker coordination, and the bounded control
    /// mailbox used by frontend-neutral callers.
    pub(crate) sessions: SessionRuntime,
    /// Runs sync-to-main workflows behind an injectable boundary.
    pub(crate) sync_main_runner: Arc<dyn SyncMainRunner>,
    /// Latest non-modal explicit project-sync lifecycle state.
    pub(crate) project_sync_status: Option<ProjectSyncStatus>,
    /// Deadline after which the terminal project-sync result is removed.
    pub(crate) project_sync_status_expires_at: Option<Instant>,
    /// Owns the active-project sync orchestrator command and context
    /// channels.
    pub(crate) sync_handle: sync::SyncHandle,
    /// Receives app events emitted by background tasks and workflows.
    pub(super) event_rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Whether Agentty was launched from inside a `tmux` session.
    pub(super) is_tmux_session: bool,
    /// Stores the latest available stable `agentty` version when one is
    /// detected.
    pub(crate) latest_available_version: Option<String>,
    /// Serializes local merge requests so only one merge workflow runs at a
    /// time.
    pub(super) merge_queue: MergeQueue,
    /// Tracks per-session thinking text rendered while background work is
    /// active.
    pub(crate) session_progress_messages: HashMap<SessionId, String>,
    /// Interacts with tmux panes for session-specific terminal workflows.
    pub(super) tmux_client: Arc<dyn TmuxClient>,
    /// Tracks the last reduced observable-handle version for each session so
    /// stale `SessionUpdated` events do not trigger redundant redraws.
    pub(crate) last_seen_session_update_versions: HashMap<SessionId, u64>,
    /// Stores the current auto-update progress state when an update is running.
    pub(crate) update_status: Option<UpdateStatus>,
    /// Monotonic identifier assigned to the next explicit project sync.
    pub(crate) next_sync_operation_id: u64,
}

impl App {
    /// Returns an advisory message when the active project declares
    /// pre-commit validation without an executable Git hook.
    pub(crate) async fn pre_commit_hook_warning(&self) -> Option<String> {
        let git_client = self.services.git_client();
        let working_dir = self.projects.working_dir().to_path_buf();
        let repo_root = git_client.find_git_repo_root(working_dir).await?;

        match git_client.check_pre_commit_hook_ready(repo_root).await {
            Ok(()) => None,
            Err(error @ GitError::PreCommitHookMissing { .. }) => Some(error.to_string()),
            Err(error) => {
                warn!(
                    error = %error,
                    "failed to inspect pre-commit hook readiness before session creation"
                );

                None
            }
        }
    }

    /// Marks the app as needing one fresh terminal frame.
    pub(crate) fn mark_dirty(&mut self) {
        self.needs_redraw = true;
    }

    /// Returns whether the runtime should render a fresh frame immediately.
    pub(crate) fn needs_redraw(&self) -> bool {
        self.needs_redraw
    }

    /// Clears the pending redraw request after one frame is rendered.
    pub(crate) fn clear_redraw(&mut self) {
        self.needs_redraw = false;
    }

    /// Returns whether tmux-only session actions are available.
    pub(crate) fn is_tmux_session(&self) -> bool {
        self.is_tmux_session
    }

    /// Cycles the active list tab forward.
    pub fn next_tab(&mut self) {
        self.tabs.next();
    }

    /// Cycles the active list tab backward.
    pub fn previous_tab(&mut self) {
        self.tabs.previous();
    }

    /// Persists the active list tab for startup restoration.
    pub(crate) async fn persist_current_tab(&self) {
        let _ = self
            .services
            .db()
            .settings()
            .upsert_setting(SettingName::ActiveTab, self.tabs.current().as_str())
            .await;
    }

    /// Starts loading linked forge review comments for the unified diff
    /// workspace.
    pub(crate) fn start_session_review_comment_load(
        &mut self,
        session_id: &SessionId,
    ) -> Option<DiffReviewComments> {
        let session = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == *session_id)?;
        let review_request = session.review_request.as_ref()?;
        let display_id = review_request.summary.display_id.clone();
        let fallback_repo_url = SessionManager::review_request_repo_url(review_request);
        let working_dir = session.folder.clone();

        let request_id = task::TaskService::spawn_session_review_comment_snapshot_task(
            task::SessionReviewCommentSnapshotTask {
                display_id,
                fallback_repo_url,
                session_id: session_id.clone(),
                working_dir,
            },
            self.services.event_sender(),
            self.services.git_client(),
            self.services.review_request_client(),
        );

        Some(DiffReviewComments::loading(request_id))
    }

    /// Moves selection to the next session in the list.
    pub fn next(&mut self) {
        self.sessions.next();
    }

    /// Moves selection to the previous session in the list.
    pub fn previous(&mut self) {
        self.sessions.previous();
    }

    /// Moves selection to the next project in the projects list.
    pub fn next_project(&mut self) {
        self.projects.next_project();
    }

    /// Moves selection to the previous project in the projects list.
    pub fn previous_project(&mut self) {
        self.projects.previous_project();
    }

    /// Selects the currently selected project in the projects list.
    ///
    /// # Errors
    /// Returns an error if there is no selected project or project switching
    /// fails.
    pub async fn switch_selected_project(&mut self) -> Result<(), AppError> {
        let selected_project_id = self
            .projects
            .selected_project_id()
            .ok_or_else(|| AppError::Workflow("No project selected".to_string()))?;

        self.switch_project(selected_project_id).await
    }

    /// Switches app context to one persisted project id.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or session refresh fails.
    pub async fn switch_project(&mut self, project_id: i64) -> Result<(), AppError> {
        let project = self
            .services
            .db()
            .projects()
            .get_project(project_id)
            .await?
            .map(Self::project_from_row)
            .ok_or_else(|| {
                AppError::Workflow(format!("Project with id `{project_id}` was not found"))
            })?;
        let recoverable_focused_review_session_ids =
            Self::load_recoverable_focused_review_session_ids(self.services.db(), project.id)
                .await?;
        let git_branch = self
            .services
            .git_client()
            .detect_git_info(project.path.clone())
            .await;
        let git_upstream_ref = Self::load_git_upstream_ref(
            self.services.git_client().as_ref(),
            project.path.as_path(),
            git_branch.as_deref(),
        )
        .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .projects()
            .upsert_project(&project.path.to_string_lossy(), git_branch.clone())
            .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .settings()
            .set_active_project_id(project.id)
            .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .projects()
            .touch_project_last_opened(project.id)
            .await;

        self.projects.update_active_project_context(
            project.id,
            project.display_label(),
            git_branch,
            git_upstream_ref,
            project.path,
        );
        self.settings = SettingsManager::from_repositories(
            self.services.db().clone(),
            self.services.available_agent_kinds(),
            project.id,
        )
        .await;
        self.settings_presentation = SettingsPresentationState::default();
        let default_session_model = SessionManager::load_default_session_model(
            &self.services,
            Some(project.id),
            AgentKind::Antigravity.default_model(),
        )
        .await;
        self.sessions
            .set_default_session_model(default_session_model);
        for (session_id, persisted_review) in
            Self::load_focused_review_cache(self.services.db(), project.id).await
        {
            self.review_cache
                .entry(session_id)
                .or_insert(persisted_review);
        }
        self.reload_projects().await;
        self.refresh_sessions_now().await;
        self.apply_pending_project_sync_completion().await;
        self.resume_deferred_auto_reviews(recoverable_focused_review_session_ids);

        Ok(())
    }

    /// Creates a blank session and schedules list refresh through events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_session(&mut self) -> Result<String, AppError> {
        self.ensure_project_checkout_available(self.projects.active_project_id())?;
        let session_id = self
            .sessions
            .create_session(&self.projects, &self.services)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Creates a blank draft session and schedules list refresh through
    /// events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_draft_session(&mut self) -> Result<String, AppError> {
        self.ensure_project_checkout_available(self.projects.active_project_id())?;
        let base_branch = self
            .projects
            .git_branch()
            .ok_or_else(|| {
                AppError::Workflow("Git branch is required to create a session".to_string())
            })?
            .to_string();

        self.create_finalized_draft_session_for_project(
            self.projects.active_project_id(),
            &base_branch,
        )
        .await
    }

    /// Creates a draft session stacked on the selected parent session.
    ///
    /// # Errors
    /// Returns an error if the parent is not eligible for stacking or the
    /// stacked draft row cannot be persisted.
    pub async fn create_stacked_draft_session(
        &mut self,
        parent_session_id: &str,
    ) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .create_stacked_draft_session(&self.services, parent_session_id)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Moves one review-ready root session beneath an eligible parent and
    /// starts the required branch sync.
    ///
    /// # Errors
    /// Returns an error when stack policy, persistence, or sync startup fails.
    pub async fn append_session_to_stack(
        &mut self,
        session_id: &str,
        parent_session_id: &str,
    ) -> Result<(), AppError> {
        self.sessions
            .append_session_to_stack(&self.services, session_id, parent_session_id)
            .await?;

        Ok(())
    }

    /// Forks one root review-ready session and opens the forked session
    /// view.
    ///
    /// The new session starts on a fresh worktree branch that points at the
    /// source session branch tip, with persisted transcript history copied at
    /// fork time and provider-native conversation identifiers reset.
    ///
    /// # Errors
    /// Returns an error when the source session is missing, not root
    /// review-ready, lacks project metadata, or the forked session cannot be
    /// created.
    pub async fn fork_session(&mut self, source_session_id: &str) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .fork_session(&self.services, source_session_id)
            .await?;
        self.finish_session_creation(&session_id).await;
        self.sessions
            .load_session_detail_into_state(self.services.db(), &session_id)
            .await;
        self.open_session(&session_id);

        Ok(session_id)
    }

    /// Creates one fresh draft session, stages the continuation context as
    /// its first draft message, and opens an empty composer for follow-up
    /// notes.
    ///
    /// # Errors
    /// Returns an error if the source session is missing, is not terminal, has
    /// neither a merged commit hash nor persisted continuation context, or if
    /// the new draft session cannot be created.
    pub async fn continue_terminal_session(
        &mut self,
        source_session_id: &str,
    ) -> Result<String, AppError> {
        let continuation_draft = self
            .terminal_session_continuation_draft(source_session_id)
            .await?;
        let session_id = self
            .create_finalized_draft_session_for_project(
                continuation_draft.project_id,
                &continuation_draft.base_branch,
            )
            .await?;
        self.stage_draft_message(&session_id, continuation_draft.prompt_seed)
            .await?;

        self.mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: crate::presentation::prompt::PromptAttachmentState::default(),
            focus: ChatFocus::Input,
            history_state: crate::presentation::prompt::PromptHistoryState::new(Vec::new()),
            slash_state: self.prompt_slash_state(),
            session_id: SessionId::from(session_id.as_str()),
            input: InputState::default(),
            scroll_offset: None,
        };

        Ok(session_id)
    }

    /// Creates one draft session for a project and runs the shared app-level
    /// post-create refresh/selection flow before returning.
    ///
    /// # Errors
    /// Returns an error if draft persistence or app refresh fails.
    async fn create_finalized_draft_session_for_project(
        &mut self,
        project_id: i64,
        base_branch: &str,
    ) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .create_draft_session_for_project(&self.services, project_id, base_branch)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Applies the shared post-create refresh and selection flow for a new
    /// session.
    async fn finish_session_creation(&mut self, session_id: &str) {
        self.process_pending_app_events().await;
        self.reload_projects().await;

        let index = self
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == session_id)
            .unwrap_or(0);
        self.sessions.select_session_index(Some(index));
    }

    /// Returns the persisted continuation draft context for one terminal
    /// session.
    ///
    /// # Errors
    /// Returns an error if the source session is missing, not terminal, or has
    /// neither a stored merged commit hash nor usable persisted continuation
    /// context.
    async fn terminal_session_continuation_draft(
        &self,
        source_session_id: &str,
    ) -> Result<TerminalContinuationDraft, AppError> {
        let source_session = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == source_session_id)
            .ok_or_else(|| AppError::Workflow("Session not found".to_string()))?;
        if !source_session.allows_terminal_continuation() {
            return Err(AppError::Workflow(
                "Only `Done` or `Canceled` sessions can be continued".to_string(),
            ));
        }

        let project_id = self
            .services
            .db()
            .sessions()
            .load_session_project_id(source_session_id)
            .await?
            .ok_or_else(|| {
                AppError::Workflow(
                    "Source session has no project association. Restart Agentty from this project \
                     to backfill legacy sessions, then continue the session again."
                        .to_string(),
                )
            })?;
        let merged_commit_hash = self
            .services
            .db()
            .sessions()
            .load_session_merged_commit_hash(source_session_id)
            .await?;
        let prompt_seed = if source_session.status == Status::Done
            && let Some(merged_commit_hash) = merged_commit_hash
        {
            Self::merged_commit_continuation_prompt(source_session, &merged_commit_hash)
        } else {
            source_session.continuation_prompt_seed().ok_or_else(|| {
                AppError::Workflow(
                    "Terminal continuation requires a merged commit hash or persisted context"
                        .to_string(),
                )
            })?
        };

        Ok(TerminalContinuationDraft {
            base_branch: source_session.base_branch.clone(),
            project_id,
            prompt_seed,
        })
    }

    /// Builds the initial continuation draft message that asks the agent to use
    /// one merged session commit as context.
    fn merged_commit_continuation_prompt(
        _source_session: &Session,
        merged_commit_hash: &str,
    ) -> String {
        format!("Use {merged_commit_hash} commit as an initial context for this session")
    }

    /// Submits the initial prompt for a newly created session.
    ///
    /// Starting a new turn clears cached and persisted focused-review output
    /// for that session so review text does not bleed into the next prompt
    /// cycle.
    ///
    /// # Errors
    /// Returns an error if the session is missing or task enqueue fails.
    pub async fn start_session(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), AppError> {
        if self
            .sessions
            .session_for_id(session_id)
            .is_some_and(Session::is_draft_session)
        {
            self.ensure_project_checkout_available(self.projects.active_project_id())?;
        }
        self.clear_review_output(session_id);
        self.services
            .db()
            .sessions()
            .update_session_focused_review(session_id, None, None, None)
            .await?;

        self.sessions
            .start_session(&self.services, session_id, prompt)
            .await?;
        self.clear_diff_comment_progress(session_id);

        Ok(())
    }

    /// Persists one staged draft message for a `Draft` session without
    /// launching the agent.
    ///
    /// # Errors
    /// Returns an error if the session cannot accept more staged drafts.
    pub async fn stage_draft_message(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), AppError> {
        Ok(self
            .sessions
            .stage_draft_message(&self.services, session_id, prompt)
            .await?)
    }

    /// Starts a `Draft` session from its persisted staged draft bundle.
    ///
    /// Stacked drafts only launch when their parent branch is review-ready and
    /// the stack has no other active branch work.
    ///
    /// # Errors
    /// Returns an error if the project checkout is unavailable, the session is
    /// missing, has no staged drafts, or stack consistency or launch enqueueing
    /// fails.
    pub async fn start_staged_session(&mut self, session_id: &str) -> Result<(), AppError> {
        self.ensure_project_checkout_available(self.projects.active_project_id())?;
        self.clear_review_output(session_id);
        self.services
            .db()
            .sessions()
            .update_session_focused_review(session_id, None, None, None)
            .await?;

        self.sessions
            .start_staged_session(&self.services, session_id)
            .await?;
        self.clear_diff_comment_progress(session_id);

        Ok(())
    }

    /// Submits a follow-up prompt for an existing session.
    ///
    /// Starting a new turn clears cached and persisted focused-review output
    /// for that session so review text does not persist past prompt
    /// submission. Returns `true` when the reply command was enqueued on the
    /// session worker.
    pub async fn reply(&mut self, session_id: &str, prompt: impl Into<TurnPrompt>) -> bool {
        if self
            .sessions
            .session_or_err(session_id)
            .is_ok_and(|session| session.status.is_read_only())
        {
            return false;
        }

        self.clear_review_output(session_id);
        let _ = self
            .services
            .db()
            .sessions()
            .update_session_focused_review(session_id, None, None, None)
            .await;

        let enqueued = self
            .sessions
            .reply(&self.services, session_id, prompt)
            .await;
        if enqueued {
            self.clear_diff_comment_progress(session_id);
        }

        enqueued
    }

    /// Queues one chat prompt for an existing `InProgress` or `Rebasing`
    /// session so the session worker dispatches it as the next turn once the
    /// active operation finishes.
    ///
    /// # Errors
    /// Returns the underlying [`crate::app::session::SessionError`] when
    /// the session does not exist or the payload is empty.
    pub fn enqueue_message(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), crate::app::session::SessionError> {
        self.sessions
            .enqueue_message(&self.services, session_id, prompt)?;

        Ok(())
    }

    /// Returns the current wall-clock time used for render-time timers.
    pub(crate) fn wall_clock_unix_seconds(&self) -> i64 {
        session::unix_timestamp_from_system_time(self.sessions.state().now_system_time())
    }

    /// Returns the active agent profile used for focused review generation.
    pub(crate) fn review_agent(&self) -> app::review::ReviewAgent {
        (
            self.settings.default_review_selection,
            self.settings.default_review_reasoning_level,
            self.settings.default_review_speed_mode,
        )
    }

    /// Returns the focused-review output state that should be shown when one
    /// session view is reopened.
    pub(crate) fn review_view_state(&self, session_id: &str) -> (Option<String>, Option<&str>) {
        let status_message = match self.review_cache.get(session_id) {
            Some(ReviewCacheEntry::Loading { review_agent, .. }) => {
                Some(review_loading_message(*review_agent))
            }
            Some(ReviewCacheEntry::Failed { error, .. }) => Some(review_failure_message(error)),
            Some(ReviewCacheEntry::Ready { .. } | ReviewCacheEntry::Suppressed) | None => None,
        };

        (
            status_message,
            review_view_text(&self.review_cache, session_id),
        )
    }

    /// Returns whether focused-review generation is already running for one
    /// session without inspecting user-visible status copy.
    pub(crate) fn review_is_loading(&self, session_id: &str) -> bool {
        matches!(
            self.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { .. })
        )
    }

    /// Restores one session's cache-backed focused review into its visible
    /// output slot when the session is reopened.
    pub(crate) fn restore_review_output(&mut self, session_id: &str) {
        app::review::hydrate_review_transient(
            &self.review_cache,
            self.sessions.state_mut(),
            session_id,
        );
    }

    /// Clears cached focused-review state, invalidates pending `/apply`
    /// continuations, and retracts its display slot.
    pub(crate) fn clear_review_output(&mut self, session_id: &str) {
        self.discard_pending_apply_review_diff_loads(&SessionId::from(session_id));
        self.review_cache.remove(session_id);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session
                .transient_messages
                .retract(TransientMessageSlot::Review);
        }
    }

    /// Stores completed focused-review text and posts it to the stable review
    /// display slot.
    pub(crate) fn set_review_ready_output(
        &mut self,
        session_id: &str,
        diff_hash: u64,
        text: String,
    ) {
        self.review_cache.insert(
            SessionId::from(session_id),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: text.clone(),
            },
        );
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            let anchor = app::review::focused_review_result_anchor(session);
            session.transient_messages.upsert(TransientMessage {
                anchor,
                body: TransientMessageBody::Markdown(text),
                lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                slot: TransientMessageSlot::Review,
                turn_position: session.latest_user_prompt_position(),
            });
        }
    }

    /// Suppresses automatic focused review and removes any previous review
    /// display slot for the stopped turn.
    pub(crate) fn suppress_review_output(&mut self, session_id: &str) {
        self.review_cache
            .insert(SessionId::from(session_id), ReviewCacheEntry::Suppressed);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session
                .transient_messages
                .retract(TransientMessageSlot::Review);
        }
    }

    /// Persists and applies an agent/model selection for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_model(
        &mut self,
        session_id: &str,
        session_agent: crate::domain::agent::AgentSelection,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_model(&self.services, session_id, session_agent)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Persists and applies a personality selection for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_personality(
        &mut self,
        session_id: &str,
        personality_id: Option<String>,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_personality(&self.services, session_id, personality_id)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Persists and applies a reasoning level for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_reasoning_level(
        &mut self,
        session_id: &str,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_reasoning_level(&self.services, session_id, reasoning_level)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Persists and applies a response style for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_response_style(
        &mut self,
        session_id: &str,
        response_style: crate::domain::agent::ResponseStyle,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_response_style(&self.services, session_id, response_style)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Persists and applies a provider permission mode for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_permission_mode(
        &mut self,
        session_id: &str,
        permission_mode: crate::domain::permission::PermissionMode,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_permission_mode(&self.services, session_id, permission_mode)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Persists and applies a response-speed preference for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_speed_mode(
        &mut self,
        session_id: &str,
        speed_mode: crate::domain::agent::SpeedMode,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_speed_mode(&self.services, session_id, speed_mode)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.selected_session()
    }

    /// Returns the session snapshot for one list index, if it still exists.
    pub fn session_at(&self, session_index: usize) -> Option<&Session> {
        self.sessions.session_at(session_index)
    }

    /// Returns session id by list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<SessionId> {
        self.sessions.session_id_for_index(session_index)
    }

    /// Resolves a session id to current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.sessions.session_index_for_id(session_id)
    }

    /// Returns compact live thinking text for a session, if available.
    pub fn session_progress_message(&self, session_id: &str) -> Option<&str> {
        self.session_progress_messages
            .get(session_id)
            .map(std::string::String::as_str)
    }

    /// Returns the latest reduced observable update version for a session.
    pub fn session_update_version(&self, session_id: &str) -> u64 {
        self.last_seen_session_update_versions
            .get(session_id)
            .copied()
            .unwrap_or_default()
    }

    /// Returns the selected follow-up task action for one session, if that
    /// session currently exposes follow-up tasks.
    pub(crate) fn selected_follow_up_task_action(
        &self,
        session_id: &str,
    ) -> Option<FollowUpTaskAction> {
        self.sessions.selected_follow_up_task_action(session_id)
    }

    /// Returns whether one session has multiple follow-up tasks to cycle
    /// through in session view.
    pub(crate) fn has_multiple_follow_up_tasks(&self, session_id: &str) -> bool {
        self.sessions.has_multiple_follow_up_tasks(session_id)
    }

    /// Moves the selected follow-up task forward within one session.
    pub(crate) fn select_next_follow_up_task(&mut self, session_id: &str) {
        self.sessions.select_next_follow_up_task(session_id);
    }

    /// Moves the selected follow-up task backward within one session.
    pub(crate) fn select_previous_follow_up_task(&mut self, session_id: &str) {
        self.sessions.select_previous_follow_up_task(session_id);
    }

    /// Launches the selected follow-up task into a sibling session or opens
    /// the already launched sibling when one is linked.
    ///
    /// # Errors
    /// Returns an error if creating or starting the sibling session fails.
    pub(crate) async fn launch_or_open_selected_follow_up_task(
        &mut self,
        session_id: &str,
    ) -> Result<(), AppError> {
        let Some((position, task_text, launched_session_id)) =
            self.selected_follow_up_task_snapshot(session_id)
        else {
            return Ok(());
        };

        if let Some(launched_session_id) = launched_session_id {
            if self.open_session_if_present(&launched_session_id) {
                return Ok(());
            }

            self.set_follow_up_task_launched_session_id(session_id, position, None);
        }

        if self
            .sessions
            .session_or_err(session_id)?
            .status
            .is_read_only()
        {
            return Err(AppError::Workflow(
                "Merged sessions cannot launch new follow-up tasks".to_string(),
            ));
        }

        let sibling_session_id = self.create_session().await?;
        self.start_session(&sibling_session_id, TurnPrompt::from_text(task_text))
            .await?;
        self.set_follow_up_task_launched_session_id(
            session_id,
            position,
            Some(sibling_session_id.clone().into()),
        );
        self.open_session(&sibling_session_id);

        Ok(())
    }

    /// Deletes the selected session, clears transient review and `@`-mention
    /// state for that session, and schedules list refresh.
    pub async fn delete_selected_session(&mut self) {
        let session_id = self.selected_session().map(|session| session.id.clone());
        self.sessions
            .delete_selected_session(&self.projects, &self.services)
            .await;

        if let Some(session_id) = session_id {
            app::at_mention_task::clear_pending_load(&session_id);
            self.discard_prompt_progress(&session_id).await;
            self.discard_deleted_session_diff_state(&session_id);
            self.review_cache.remove(&session_id);
        }

        self.process_pending_app_events().await;
        self.reload_projects().await;
    }

    /// Deletes the selected session while deferring worktree filesystem cleanup
    /// to a background task, and clears transient review and `@`-mention
    /// state for that session.
    pub async fn delete_selected_session_deferred_cleanup(&mut self) {
        let session_id = self.selected_session().map(|session| session.id.clone());
        self.sessions
            .delete_selected_session_deferred_cleanup(&self.projects, &self.services)
            .await;

        if let Some(session_id) = session_id {
            app::at_mention_task::clear_pending_load(&session_id);
            self.discard_prompt_progress(&session_id).await;
            self.discard_deleted_session_diff_state(&session_id);
            self.review_cache.remove(&session_id);
        }

        self.process_pending_app_events().await;
        self.reload_projects().await;
    }

    /// Cancels a session that is running, in review, or an unstarted draft.
    ///
    /// # Errors
    /// Returns an error if the session is not found or not cancelable.
    pub async fn cancel_session(&self, session_id: &str) -> Result<(), AppError> {
        Ok(self
            .sessions
            .cancel_session(&self.services, session_id)
            .await?)
    }

    /// Waits for tracked background cleanup tasks before process shutdown.
    pub(crate) async fn wait_for_background_cleanup_tasks(&self) {
        self.services.wait_for_cleanup_tasks().await;
    }

    /// Opens the selected session worktree in tmux and optionally runs the
    /// first configured launch configuration. This is a no-op when Agentty
    /// was launched outside tmux.
    pub async fn open_session_worktree_in_tmux(&mut self) {
        let selected_launch_configuration =
            self.configured_launch_configurations().into_iter().next();

        self.open_session_worktree_in_tmux_with_command(selected_launch_configuration.as_deref())
            .await;
    }

    /// Opens the selected session worktree in tmux and optionally runs one
    /// provided launch configuration.
    ///
    /// Sessions without a materialized worktree and Agentty processes outside
    /// tmux are treated as a no-op. A successfully opened writable worktree
    /// invalidates cached diff presence because external edits can begin
    /// immediately.
    pub(crate) async fn open_session_worktree_in_tmux_with_command(
        &mut self,
        launch_configuration: Option<&str>,
    ) {
        if !self.is_tmux_session() {
            return;
        }

        let Some((session_folder, session_id)) = self
            .selected_session()
            .map(|session| (session.folder.clone(), session.id.clone()))
        else {
            return;
        };
        if !self.services.fs_client().is_dir(session_folder.clone()) {
            return;
        }
        if !self.invalidate_session_diff_presence(&session_id).await {
            return;
        }

        let Some(window_id) = self
            .tmux_client
            .open_window_for_folder(session_folder)
            .await
        else {
            return;
        };

        let Some(launch_configuration) = launch_configuration
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return;
        };

        self.tmux_client
            .run_command_in_window(window_id, launch_configuration.to_string())
            .await;
    }

    /// Marks one writable session worktree's cached diff presence unknown in
    /// both the durable session row and loaded snapshot.
    ///
    /// Returns `false` without changing the snapshot when durable invalidation
    /// fails, allowing callers to prevent external write access.
    async fn invalidate_session_diff_presence(&mut self, session_id: &SessionId) -> bool {
        if let Err(error) = self
            .services
            .db()
            .sessions()
            .mark_session_diff_unknown(session_id)
            .await
        {
            warn!(
                session_id = %session_id,
                error = %error,
                "failed to invalidate session diff presence before opening writable worktree"
            );

            return false;
        }

        self.sessions
            .apply_session_diff_stats_updated(session_id, SessionDiffStats::Unknown);

        true
    }

    /// Starts the session-view branch-publish action flow for one session.
    pub(crate) async fn start_publish_branch_action(
        &mut self,
        restore_view: ConfirmationViewMode,
        session_id: &str,
        publish_branch_action: PublishBranchAction,
        remote_branch_name: Option<String>,
    ) {
        let Some(branch_publish_context) = self.branch_publish_task_context(session_id) else {
            self.mode = Self::view_info_popup_mode(
                "Branch push failed".to_string(),
                "Session is no longer available.".to_string(),
                false,
                String::new(),
                restore_view,
            );

            return;
        };

        if publish_branch_action == PublishBranchAction::PublishPullRequest {
            let branch_operation_lock = Arc::clone(&branch_publish_context.branch_operation_lock);
            // Reserve an idle branch before persistence. An existing owner
            // already serializes worker execution, so the UI never waits here.
            let _branch_operation_guard = branch_operation_lock.try_lock_owned().ok();
            let enqueue_result = self
                .sessions
                .enqueue_review_request_creation(
                    &self.services,
                    branch_publish_context.session,
                    remote_branch_name,
                    None,
                )
                .await;
            match enqueue_result {
                Err(error) => {
                    let _ = self.sessions.finish_branch_publish(
                        session_id,
                        TransientMessageBody::Markdown(format!(
                            "**Review request publish failed**\n\n{error}"
                        )),
                    );
                }
                Ok(Some(queued_order)) => self.sessions.queue_branch_publish(
                    session_id,
                    queued_order,
                    review_request_queued_label(),
                ),
                Ok(None) => self.sessions.start_branch_publish(
                    session_id,
                    Self::branch_publish_loading_label(publish_branch_action),
                ),
            }
            self.mode = restore_view.into_view_mode();

            return;
        }

        let loading_label = Self::branch_publish_loading_label(publish_branch_action);
        let clock = self.services.clock();
        let db = self.services.db().clone();
        let event_sender = self.services.event_sender();
        let git_client = self.services.git_client();
        let review_request_client = self.services.review_request_client();
        let event_session_id = branch_publish_context.session.id.clone();

        self.sessions
            .start_branch_publish(session_id, loading_label);
        self.mode = restore_view.into_view_mode();

        tokio::spawn(async move {
            let result = run_branch_publish_action(
                publish_branch_action,
                branch_publish_context,
                db,
                clock,
                git_client,
                review_request_client,
                remote_branch_name,
            )
            .await;
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = event_sender.send(AppEvent::BranchPublishActionCompleted {
                result: Box::new(result),
                session_id: event_session_id,
            });
        });
    }

    /// Returns all configured launch configurations in user-defined order.
    #[must_use]
    pub(crate) fn configured_launch_configurations(&self) -> Vec<String> {
        self.settings.launch_configurations()
    }

    /// Appends output text to a session stream and persists it.
    pub(crate) async fn append_output_for_session(&self, session_id: &str, output: &str) {
        self.sessions
            .append_output_for_session(&self.services, session_id, output)
            .await;
    }

    /// Removes prompt attachment files that still belong to the active
    /// composer state.
    pub(crate) async fn cleanup_prompt_attachment_files(&self, prompt: &TurnPrompt) {
        self.sessions
            .cleanup_prompt_attachment_files(&self.services, prompt)
            .await;
    }

    /// Starts squash-merge workflow for a review-ready session.
    ///
    /// # Errors
    /// Returns an error if session is not mergeable, queueing fails, or
    /// immediate merge start fails while the queue is idle.
    pub async fn merge_session(&mut self, session_id: &str) -> Result<(), AppError> {
        self.ensure_project_checkout_available(self.projects.active_project_id())?;
        if self.merge_queue.is_queued_or_active(session_id) {
            return Ok(());
        }

        self.validate_merge_request(session_id)?;
        if self.merge_queue.has_active() {
            self.mark_session_as_queued_for_merge(session_id).await?;
            self.merge_queue.enqueue(SessionId::from(session_id));

            return Ok(());
        }

        self.merge_queue.enqueue(SessionId::from(session_id));

        self.start_next_merge_from_queue(true).await
    }

    /// Starts or queues a session branch rebase onto its base branch.
    ///
    /// If the session is currently generating focused review output, starting
    /// sync cancels the pending review cache and persisted review entries so
    /// late review-assist completions cannot overwrite the rebased view state
    /// and startup cannot hydrate stale review text.
    ///
    /// # Errors
    /// Returns an error if focused-review persistence cannot be cleared before
    /// sync starts, or if session sync cannot start.
    pub async fn rebase_session(&mut self, session_id: &str) -> Result<(), AppError> {
        self.ensure_project_checkout_available(self.projects.active_project_id())?;
        let should_clear_pending_review = matches!(
            self.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { .. })
        );
        if should_clear_pending_review {
            self.services
                .db()
                .sessions()
                .update_session_focused_review(session_id, None, None, None)
                .await?;
            self.clear_review_output(session_id);
        }

        self.sessions
            .rebase_session(&self.services, session_id)
            .await?;

        Ok(())
    }

    /// Starts selected-project branch sync without changing the active mode.
    ///
    /// Duplicate requests are coalesced while the same project operation is
    /// running. The immutable sync context prevents later navigation or a
    /// project switch from redirecting queued Git and review work.
    pub(crate) fn start_sync_main(&mut self) {
        let sync_context = self.sync_handle.context_snapshot();
        if self.project_sync_status.as_ref().is_some_and(|status| {
            status.is_running() && status.context.project_id == sync_context.project_id
        }) || self
            .pending_project_sync_requests
            .iter()
            .any(|request| request.operation.project_id == sync_context.project_id)
        {
            return;
        }

        let request = self.new_sync_main_request(sync_context);
        if self
            .project_sync_status
            .as_ref()
            .is_some_and(ProjectSyncStatus::is_running)
        {
            self.pending_project_sync_requests.push_back(request);

            return;
        }
        if self.merge_queue.has_work() {
            self.project_sync_status = Some(ProjectSyncStatus {
                context: request.operation,
                phase: ProjectSyncPhase::Blocked {
                    message: "a merge is active or queued; try again after it finishes".to_string(),
                },
            });
            self.schedule_project_sync_status_expiry();
            self.mark_dirty();

            return;
        }

        self.dispatch_sync_main_request(request);
    }

    /// Captures one immutable request for the currently selected project.
    fn new_sync_main_request(&mut self, sync_context: sync::SyncContext) -> SyncMainRequest {
        let operation_id = self.next_sync_operation_id;
        self.next_sync_operation_id = self.next_sync_operation_id.saturating_add(1);
        self.latest_project_sync_operation_ids
            .insert(sync_context.project_id, operation_id);
        self.pending_project_sync_completions
            .remove(&sync_context.project_id);
        let operation = ProjectSyncContext {
            default_branch: sync_context
                .project_branch_name
                .clone()
                .unwrap_or_else(|| "not detected".to_string()),
            operation_id,
            project_id: sync_context.project_id,
            project_name: sync_context.project_name.clone(),
        };

        SyncMainRequest {
            app_event_tx: self.services.event_sender(),
            operation,
            session_model: self.sessions.default_session_model(),
            sync_context,
        }
    }

    /// Starts one request after the foreground mutation slot is reserved.
    fn dispatch_sync_main_request(&mut self, request: SyncMainRequest) {
        self.project_sync_status_expires_at = None;
        self.project_sync_status = Some(ProjectSyncStatus {
            context: request.operation.clone(),
            phase: ProjectSyncPhase::Running,
        });
        self.mark_dirty();

        self.sync_main_runner.start_sync_main(
            request.app_event_tx,
            request.operation,
            request.session_model,
            request.sync_context,
        );
    }

    /// Keeps the current terminal sync result visible for a short period.
    pub(super) fn schedule_project_sync_status_expiry(&mut self) {
        self.project_sync_status_expires_at =
            Some(self.services.clock().now_instant() + PROJECT_SYNC_STATUS_VISIBLE_DURATION);
    }

    /// Clears a terminal sync result once its visibility deadline is reached.
    pub(crate) fn expire_project_sync_status(&mut self, now: Instant) {
        if self
            .project_sync_status_expires_at
            .is_none_or(|expires_at| now < expires_at)
        {
            return;
        }

        self.project_sync_status = None;
        self.project_sync_status_expires_at = None;
        self.mark_dirty();
    }

    /// Starts the oldest queued project sync when base-checkout mutation is
    /// idle.
    fn start_next_project_sync_from_queue(&mut self) {
        if self
            .project_sync_status
            .as_ref()
            .is_some_and(ProjectSyncStatus::is_running)
            || self.merge_queue.has_work()
        {
            return;
        }
        let Some(request) = self.pending_project_sync_requests.pop_front() else {
            return;
        };

        self.dispatch_sync_main_request(request);
    }

    /// Gives pending merges priority, then resumes queued project sync work.
    pub(super) async fn resume_base_checkout_work(&mut self) {
        if self
            .project_sync_status
            .as_ref()
            .is_some_and(ProjectSyncStatus::is_running)
        {
            return;
        }
        if self.merge_queue.has_work() {
            // Best-effort: merge queue progression failures are surfaced in
            // session output.
            let _ = self.start_next_merge_from_queue(false).await;
        }

        self.start_next_project_sync_from_queue();
    }

    /// Rejects base-checkout operations that could race the active project
    /// sync.
    pub(crate) fn ensure_project_checkout_available(
        &self,
        project_id: i64,
    ) -> Result<(), AppError> {
        let Some(sync_status) = self
            .project_sync_status
            .as_ref()
            .filter(|status| status.is_running() && status.context.project_id == project_id)
        else {
            return Ok(());
        };

        Err(AppError::Workflow(format!(
            "Project `{}` is synchronizing `{}`; try this base-branch operation again after sync",
            sync_status.context.project_name, sync_status.context.default_branch,
        )))
    }

    /// Starts review assist generation for one session using the
    /// current diff text and the configured default review model.
    ///
    /// The review assist prompt enforces inspection-only review constraints
    /// and recommends verification commands instead of running them.
    pub(crate) async fn start_review_assist(
        &mut self,
        session_id: &str,
        session_folder: &Path,
        diff_hash: u64,
        review_diff: &str,
        review_agent: app::review::ReviewAgent,
    ) {
        let review_agent = app::review::normalize_review_agent(review_agent);
        self.review_cache.insert(
            SessionId::from(session_id),
            ReviewCacheEntry::Loading {
                diff_hash,
                review_agent,
            },
        );
        let session_chat_history = self.session_chat_history(session_id).await;

        mark_session_agent_review(self.sessions.state_mut(), session_id);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Loading(review_loading_message(review_agent)),
                lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                slot: TransientMessageSlot::Review,
                turn_position: session.latest_user_prompt_position(),
            });
        }

        spawn_review_assist(
            self.services.event_sender(),
            review_agent,
            session_id,
            session_folder,
            diff_hash,
            review_diff,
            session_chat_history.as_deref(),
        );
    }

    /// Returns saved conversation context from live state or persistence.
    pub(super) async fn session_chat_history(&self, session_id: &str) -> Option<String> {
        let live_history = self
            .sessions
            .session_handles()
            .get(session_id)
            .and_then(|handles| {
                handles
                    .transcript
                    .lock()
                    .ok()
                    .as_deref()
                    .and_then(SessionTranscript::conversation_replay_text)
            })
            .or_else(|| {
                self.sessions
                    .session_for_id(session_id)
                    .and_then(|session| session.transcript.as_ref())
                    .and_then(SessionTranscript::conversation_replay_text)
            });
        if live_history.is_some() {
            return live_history;
        }

        session::load_session_transcript(self.services.db(), session_id)
            .await
            .ok()
            .and_then(|transcript| transcript.conversation_replay_text())
    }

    /// Reloads sessions when metadata cache indicates changes.
    ///
    /// Returns `true` when the fallback poll refreshed render-visible session
    /// state.
    pub async fn refresh_sessions_if_needed(&mut self) -> bool {
        let resources_refreshed = self.sessions.refresh_resources().await;
        let refreshed = self
            .sessions
            .refresh_sessions_if_needed(&mut self.mode, &self.projects, &self.services)
            .await;
        if refreshed {
            app::review::prune_review_cache(
                &mut self.review_cache,
                &self.pending_focused_review_persistence,
                self.sessions.state(),
            );
            app::review::hydrate_review_transients(&self.review_cache, self.sessions.state_mut());
        }

        refreshed || resources_refreshed
    }

    /// Forces immediate session list reload.
    pub(crate) async fn refresh_sessions_now(&mut self) {
        self.sessions
            .refresh_sessions_now(&mut self.mode, &self.projects, &self.services)
            .await;
        app::review::prune_review_cache(
            &mut self.review_cache,
            &self.pending_focused_review_persistence,
            self.sessions.state(),
        );
        app::review::hydrate_review_transients(&self.review_cache, self.sessions.state_mut());
        self.restart_git_status_task();
    }

    /// Reloads project list snapshots from persistence.
    pub(super) async fn reload_projects(&mut self) {
        let project_items =
            Self::load_project_items(self.services.db(), self.services.fs_client().as_ref()).await;
        self.projects.replace_project_items(project_items);
    }

    /// Publishes the current project/session sync context and requests an
    /// immediate orchestrator refresh when the active project has a git
    /// branch.
    pub(super) fn restart_git_status_task(&mut self) {
        self.publish_sync_context_for_refresh();
        if self.projects.has_git_branch() {
            self.sync_handle.request_refresh();
        }
    }

    /// Publishes a fresh sync context after reducer-applied session changes
    /// that may affect polling targets.
    pub(super) fn publish_sync_context(&self) {
        self.sync_handle.publish_context(Self::sync_context_for(
            &self.projects,
            &self.services,
            &self.sessions,
        ));
    }

    /// Publishes a fresh sync context and forces a new generation so in-flight
    /// status completions computed before the requested refresh are ignored.
    pub(super) fn publish_sync_context_for_refresh(&self) {
        self.sync_handle
            .publish_refresh_context(Self::sync_context_for(
                &self.projects,
                &self.services,
                &self.sessions,
            ));
    }

    /// Builds the versioned sync context for the active project and session
    /// snapshot.
    pub(crate) fn sync_context_for(
        projects: &ProjectManager,
        services: &AppServices,
        sessions: &SessionManager,
    ) -> sync::SyncContext {
        sync::SyncContext {
            generation: 0,
            git_client: services.git_client(),
            project_branch_name: projects.git_branch().map(str::to_string),
            project_id: projects.active_project_id(),
            project_name: projects.project_name().to_string(),
            review_request_client: services.review_request_client(),
            review_request_sync_targets: Self::review_request_sync_targets(sessions),
            session_git_status_targets: Self::session_git_status_targets(sessions),
            working_dir: projects.working_dir().to_path_buf(),
        }
    }

    /// Builds git-status polling targets for active session branches in the
    /// current project.
    pub(crate) fn session_git_status_targets(
        sessions: &SessionManager,
    ) -> Vec<sync::SessionGitStatusTarget> {
        sessions
            .state()
            .sessions()
            .iter()
            .filter(|session| !matches!(session.status, Status::Canceled | Status::Done))
            .filter(|session| Self::session_has_git_status_target(sessions, session))
            .map(|session| sync::SessionGitStatusTarget {
                base_branch: session.base_branch.clone(),
                branch_name: sessions
                    .session_branch_name(&session.id)
                    .map_or_else(|| session::session_branch(&session.id), str::to_string),
                session_id: session.id.clone(),
            })
            .collect()
    }

    /// Returns whether a session has a materialized branch that can be polled
    /// for git-status comparisons.
    fn session_has_git_status_target(
        sessions: &SessionManager,
        session: &crate::domain::session::Session,
    ) -> bool {
        !session.is_draft_session()
            || sessions
                .session_worktree_availability()
                .get(&session.id)
                .copied()
                .unwrap_or(false)
    }

    /// Builds review-request polling targets for active session branches in
    /// the current project.
    pub(crate) fn review_request_sync_targets(
        sessions: &SessionManager,
    ) -> Vec<sync::ReviewRequestSyncTarget> {
        sessions
            .state()
            .sessions()
            .iter()
            .filter(|session| session.can_sync_review_request())
            .map(|session| sync::ReviewRequestSyncTarget {
                folder: session.folder.clone(),
                linked_review_request: session.review_request.clone(),
                published_upstream_ref: session.published_upstream_ref.clone(),
                session_id: session.id.clone(),
            })
            .collect()
    }

    /// Returns the currently selected follow-up task payload for one session.
    fn selected_follow_up_task_snapshot(
        &self,
        session_id: &str,
    ) -> Option<(usize, String, Option<SessionId>)> {
        let position = self.sessions.selected_follow_up_task_position(session_id)?;
        let session = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)?;
        let follow_up_task = session.follow_up_task(position)?;

        Some((
            follow_up_task.position,
            follow_up_task.text.clone(),
            follow_up_task.launched_session_id.clone(),
        ))
    }

    /// Mirrors one launched sibling-session link into the in-memory session
    /// snapshot.
    fn set_follow_up_task_launched_session_id(
        &mut self,
        session_id: &str,
        position: usize,
        launched_session_id: Option<SessionId>,
    ) {
        self.sessions.set_follow_up_task_launched_session_id(
            session_id,
            position,
            launched_session_id,
        );
    }

    /// Opens one linked sibling session when it still exists in memory.
    ///
    /// Returns `true` when the target session was found and opened.
    fn open_session_if_present(&mut self, target_session_id: &str) -> bool {
        let Some(session_index) = self.session_index_for_id(target_session_id) else {
            return false;
        };
        self.open_session_by_index(target_session_id, session_index);

        true
    }

    /// Opens one session by id and preserves question mode for clarification
    /// sessions.
    fn open_session(&mut self, target_session_id: &str) {
        let Some(session_index) = self.session_index_for_id(target_session_id) else {
            return;
        };
        self.open_session_by_index(target_session_id, session_index);
    }

    /// Opens one session by list index and preserves question mode for
    /// clarification sessions.
    fn open_session_by_index(&mut self, target_session_id: &str, session_index: usize) {
        self.sessions.select_session_index(Some(session_index));
        self.restore_review_output(target_session_id);

        let Some(session) = self.sessions.session_at(session_index) else {
            return;
        };
        if session.status == Status::Question && session.accepts_user_turns() {
            let questions = session.questions.clone();
            self.enter_question_mode(target_session_id, questions);

            return;
        }

        self.mode = AppMode::View {
            session_id: SessionId::from(target_session_id),
            scroll_offset: None,
        };
    }

    /// Enters the interactive clarification panel when the actively viewed
    /// session has reached [`Status::Question`] but the UI is still on the
    /// plain session view.
    ///
    /// The live transition into `AppMode::Question` is a one-shot side effect
    /// of the `AgentResponseReceived` projection, gated on the session being
    /// viewed at the instant the turn completes. When that projection is
    /// missed — an overlay was open, the projection coalesced to empty, or the
    /// worker fell back to a reload-only recovery — the durable `Question`
    /// status still reaches the snapshot, stranding the question behind the
    /// session view until a manual reopen. This reconciliation mirrors the
    /// reopen path so the panel appears without one.
    ///
    /// A session view showing `Status::Question` is always an anomaly: every
    /// path that leaves the panel (answering, `Ctrl+C`/`Esc`, or `q`) moves the
    /// session off `Question` or out of `AppMode::View`, so entering the panel
    /// here cannot fight a legitimate view state.
    pub(crate) async fn reconcile_open_session_question_mode(&mut self) {
        let AppMode::View { session_id, .. } = &self.mode else {
            self.question_reconcile_reload_attempted = None;

            return;
        };

        let session_id = session_id.clone();
        let is_pending_question =
            self.sessions
                .session_for_id(&session_id)
                .is_some_and(|session| {
                    session.status == Status::Question && session.accepts_user_turns()
                });
        if !is_pending_question {
            self.question_reconcile_reload_attempted = None;

            return;
        }

        let mut questions = self
            .sessions
            .session_for_id(&session_id)
            .map(|session| session.questions.clone())
            .unwrap_or_default();
        if questions.is_empty() {
            // The list snapshot only carries persisted questions for the
            // active session, so reload detail before giving up, mirroring the
            // reopen path in `open_session_by_index`. A `Question` status with
            // no persisted questions is malformed, so reload at most once per
            // stuck session: without this guard `run_cycle` would reissue the
            // async load on every render frame while the view stays stuck.
            if self.question_reconcile_reload_attempted.as_deref() == Some(session_id.as_str()) {
                return;
            }

            self.question_reconcile_reload_attempted = Some(session_id.clone());
            self.sessions
                .load_session_detail_into_state(self.services.db(), &session_id)
                .await;
            questions = self
                .sessions
                .session_for_id(&session_id)
                .map(|session| session.questions.clone())
                .unwrap_or_default();
        }
        if questions.is_empty() {
            return;
        }

        self.question_reconcile_reload_attempted = None;
        self.enter_question_mode(&session_id, questions);
    }

    /// Enters question mode for a clarification session.
    ///
    /// Consumes saved partial answers from a previous visit when they still
    /// match the session's question list, so leaving question mode with `q`
    /// does not lose already-submitted answers.
    pub(crate) fn enter_question_mode(&mut self, session_id: &str, questions: Vec<QuestionItem>) {
        let progress = self
            .question_progress
            .remove(session_id)
            .filter(|progress| progress.applies_to(&questions));
        let (current_index, input, responses, selected_option_index) = match progress {
            Some(progress) => (
                progress.current_index,
                progress.input,
                progress.responses,
                progress.selected_option_index,
            ),
            None => (
                0,
                InputState::default(),
                Vec::new(),
                default_option_index(&questions, 0),
            ),
        };

        self.mode = AppMode::Question {
            at_mention_state: None,
            current_index,
            focus: ChatFocus::Input,
            input,
            questions,
            responses,
            scroll_offset: None,
            selected_option_index,
            session_id: SessionId::from(session_id),
        };
    }

    /// Saves one prompt composer for restoration after returning to the
    /// sessions list.
    pub(crate) fn save_prompt_progress(&mut self, snapshot: PromptModeSnapshot) {
        let session_id = snapshot.session_id.clone();

        self.prompt_progress.insert(session_id, snapshot);
    }

    /// Discards a saved prompt composer and cleans up its attachment files.
    pub(crate) async fn discard_prompt_progress(&mut self, session_id: &str) {
        let Some(snapshot) = self.prompt_progress.remove(session_id) else {
            return;
        };

        let attachments = snapshot
            .attachment_state
            .attachments
            .into_iter()
            .chain(snapshot.attachment_state.archived_attachments)
            .collect();
        self.cleanup_prompt_attachments(attachments).await;
    }

    /// Restores and consumes the saved prompt composer for `session_id`.
    ///
    /// Returns `true` when a saved composer was found and installed as the
    /// active mode. Restored composers always focus the input panel so typing
    /// can resume immediately. Snapshots for queued, merging, or terminal
    /// sessions are discarded with their attachment files instead.
    pub(crate) async fn restore_prompt_progress(&mut self, session_id: &str) -> bool {
        let Some(status) = self
            .sessions
            .session_for_id(session_id)
            .map(|session| session.status)
        else {
            return false;
        };
        if !matches!(
            status,
            Status::Draft
                | Status::InProgress
                | Status::Review
                | Status::AgentReview
                | Status::Rebasing
        ) {
            if matches!(
                status,
                Status::Queued | Status::Merging | Status::Merged | Status::Done | Status::Canceled
            ) {
                self.discard_prompt_progress(session_id).await;
            }

            return false;
        }

        if status != Status::Draft && !self.sessions.can_reply_to_session_in_stack(session_id) {
            return false;
        }

        let Some(snapshot) = self.prompt_progress.remove(session_id) else {
            return false;
        };

        self.mode = snapshot.into_prompt_mode();

        true
    }

    /// Validates whether a session is currently eligible for merge queueing.
    ///
    /// Sessions are eligible while actively under review or already marked as
    /// `Queued` (for example, after app restart). A parent with idle
    /// materialized children can enter merge queueing because merge completion
    /// retargets those children; linked forge review requests and active stack
    /// work still block the request.
    ///
    /// # Errors
    /// Returns an error when the session does not exist or has an ineligible
    /// status, or when stack consistency blocks branch mutation.
    fn validate_merge_request(&self, session_id: &str) -> Result<(), AppError> {
        let session = self.sessions.session_or_err(session_id)?;
        if !(session.status.allows_review_actions() || session.status == Status::Queued) {
            return Err(AppError::Workflow(
                "Session must be in review or queued status".to_string(),
            ));
        }
        if !self.sessions.can_merge_session_branch_in_stack(session_id) {
            return Err(AppError::Workflow(
                "Merge cannot run for linked review requests or while another stack session is \
                 active"
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Marks one session as waiting in the merge queue.
    ///
    /// # Errors
    /// Returns an error when status transition to `Queued` is invalid.
    async fn mark_session_as_queued_for_merge(&self, session_id: &str) -> Result<(), AppError> {
        let handles = self.sessions.session_handles_or_err(session_id)?;
        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let status_updated = status_transition.apply(Status::Queued).await;

        if !status_updated {
            return Err(AppError::Workflow(
                "Invalid status transition to Queued".to_string(),
            ));
        }

        Ok(())
    }

    /// Restores a queued session to `Review` if merge start fails.
    async fn restore_queued_session_to_review(&self, session_id: &str) {
        let session_status = self
            .sessions
            .session_or_err(session_id)
            .map(|session| session.status);
        if !matches!(session_status, Ok(Status::Queued)) {
            return;
        }

        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        // Best-effort: status transition failure is non-critical.
        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let _ = status_transition.apply(Status::Review).await;
    }

    /// Starts the next pending merge request when no merge is currently active.
    ///
    /// When `stop_on_failure` is `true`, returns the first start error.
    /// Otherwise, failed entries are skipped and the queue continues.
    ///
    /// # Errors
    /// Returns an error when starting a queued merge fails and
    /// `stop_on_failure` is enabled.
    async fn start_next_merge_from_queue(&mut self, stop_on_failure: bool) -> Result<(), AppError> {
        if self.merge_queue.has_active() {
            return Ok(());
        }
        if self
            .ensure_project_checkout_available(self.projects.active_project_id())
            .is_err()
        {
            return Ok(());
        }

        while let Some(next_session_id) = self.merge_queue.pop_next() {
            match self
                .sessions
                .merge_session(&next_session_id, &self.projects, &self.services)
                .await
            {
                Ok(()) => {
                    self.merge_queue.set_active(next_session_id);

                    return Ok(());
                }
                Err(error) => {
                    self.restore_queued_session_to_review(&next_session_id)
                        .await;

                    let merge_error = TranscriptNotice::MergeError.format(&error);
                    self.append_output_for_session(&next_session_id, &merge_error)
                        .await;

                    if stop_on_failure {
                        return Err(error.into());
                    }
                }
            }
        }

        Ok(())
    }

    /// Advances queue state after reducer-applied status changes.
    ///
    /// The queue advances when the active merge session transitions away from
    /// `Merging` or disappears from the refreshed session list.
    pub(super) async fn handle_merge_queue_progress(
        &mut self,
        session_ids: &HashSet<SessionId>,
        previous_session_states: &HashMap<SessionId, Status>,
    ) {
        let current_status = self
            .merge_queue
            .active_session_id()
            .and_then(|active_session_id| {
                self.sessions
                    .sessions()
                    .iter()
                    .find(|session| session.id == active_session_id)
                    .map(|session| session.status)
            });
        let progress = self.merge_queue.progress_from_status_updates(
            current_status,
            session_ids,
            previous_session_states,
        );
        if progress == MergeQueueProgress::StartNext {
            self.resume_base_checkout_work().await;
        }
    }

    /// Drops thinking text for sessions that are no longer actively running.
    pub(super) fn retain_valid_session_progress_messages(&mut self) {
        self.session_progress_messages.retain(|session_id, _| {
            self.sessions
                .sessions()
                .iter()
                .find(|session| session.id == *session_id)
                .is_some_and(|session| matches!(session.status, Status::InProgress))
        });
    }

    /// Builds one branch-publish task snapshot with its shared operation lock.
    pub(crate) fn branch_publish_task_context(
        &self,
        session_id: &str,
    ) -> Option<BranchPublishTaskContext> {
        let (session, handles) = self.sessions.session_and_handles_or_err(session_id).ok()?;
        let mut branch_publish_session = BranchPublishTaskSession::from_session(session);
        branch_publish_session.base_branch = self.review_target_branch_for_session(session);

        Some(BranchPublishTaskContext {
            branch_operation_lock: Arc::clone(&handles.branch_operation_lock),
            session: branch_publish_session,
        })
    }

    /// Resolves the forge review-request target branch for one session.
    ///
    /// Root sessions target their stored base branch. Stacked children target
    /// their parent session branch while the parent link exists, preferring
    /// the parent's linked review-request source branch, then the parent's
    /// pushed upstream branch, then the child row's stored local parent branch.
    fn review_target_branch_for_session(&self, session: &Session) -> String {
        self.stacked_parent_review_target_branch(session)
            .unwrap_or_else(|| session.base_branch.clone())
    }

    /// Returns the best review target branch for one stacked child's parent.
    fn stacked_parent_review_target_branch(&self, session: &Session) -> Option<String> {
        let parent_session_id = session.parent_session_id.as_ref()?;
        let parent_session = self
            .sessions
            .sessions()
            .iter()
            .find(|candidate| candidate.id.as_str() == parent_session_id.as_str())?;

        parent_session
            .review_request
            .as_ref()
            .map(|review_request| review_request.summary.source_branch.clone())
            .or_else(|| {
                parent_session
                    .published_upstream_ref
                    .as_deref()
                    .map(session::remote_branch_name_from_upstream_ref)
            })
    }
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;

#[cfg(test)]
mod fork_tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::session::{SessionDiffState, SessionStats};
    use crate::domain::session_message::SessionMessageKind;
    use crate::infra::tmux::MockTmuxClient;

    /// Prompt text copied through fork snapshot tests.
    const FORK_SOURCE_PROMPT: &str = "Build the fork workflow";
    /// Assistant text copied through fork snapshot tests.
    const FORK_SOURCE_ANSWER: &str = "Fork workflow complete";

    /// Creates a real git-backed source session and marks it review-ready for
    /// fork tests.
    async fn create_review_source_session_for_fork_test(app: &mut App) -> String {
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create source session");
        let source_status = Status::Review.to_string();
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&source_session_id, &source_status, 0)
            .await
            .expect("failed to mark source as review");

        persist_fork_source_runtime_linkage(app, &source_session_id).await;
        persist_fork_source_transcript(app, &source_session_id).await;
        let source_folder = app
            .sessions
            .session_for_id(&source_session_id)
            .expect("missing source session")
            .folder
            .clone();
        std::fs::write(source_folder.join("README.md"), "dirty source worktree")
            .expect("failed to modify source worktree");
        crate::test_support::set_session_status_for_test(app, &source_session_id, Status::Review);

        source_session_id
    }

    /// Persists source-only linkage that a fork must intentionally clear.
    async fn persist_fork_source_runtime_linkage(app: &App, source_session_id: &str) {
        app.services
            .db()
            .sessions()
            .update_session_provider_conversation_id(
                source_session_id,
                Some("provider-thread".to_string()),
            )
            .await
            .expect("failed to persist provider conversation id");
        app.services
            .db()
            .sessions()
            .update_session_instruction_conversation_id(
                source_session_id,
                Some("instruction-thread".to_string()),
            )
            .await
            .expect("failed to persist instruction conversation id");
        app.services
            .db()
            .sessions()
            .update_session_published_upstream_ref(
                source_session_id,
                Some("origin/wt/source".to_string()),
            )
            .await
            .expect("failed to persist upstream ref");
        app.services
            .db()
            .sessions()
            .update_session_stats(
                source_session_id,
                &SessionStats {
                    input_tokens: 13,
                    output_tokens: 21,
                    ..SessionStats::default()
                },
            )
            .await
            .expect("failed to persist source usage stats");
        app.services
            .db()
            .sessions()
            .update_session_diff_stats(1, 0, true, source_session_id, "XS")
            .await
            .expect("failed to persist source diff stats");
    }

    /// Persists source transcript rows that a fork must copy.
    async fn persist_fork_source_transcript(app: &App, source_session_id: &str) {
        app.services
            .db()
            .sessions()
            .append_session_message(
                source_session_id,
                SessionMessageKind::UserPrompt,
                FORK_SOURCE_PROMPT,
            )
            .await
            .expect("failed to append source user prompt");
        app.services
            .db()
            .sessions()
            .append_session_message(
                source_session_id,
                SessionMessageKind::AssistantAnswer,
                FORK_SOURCE_ANSWER,
            )
            .await
            .expect("failed to append source assistant answer");
    }

    /// Asserts that a fork is open and contains the copied transcript without
    /// source runtime linkage.
    async fn assert_forked_session_snapshot(app: &App, forked_session_id: &str) {
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                ..
            } if session_id.as_str() == forked_session_id
        ));
        assert!(matches!(
            app.selected_session(),
            Some(session) if session.id == forked_session_id
                && session.status == Status::Review
                && session.parent_session_id.is_none()
                && session.published_upstream_ref.is_none()
                && session.stats.added_lines == 0
                && session.stats.deleted_lines == 0
                && session.stats.diff_state == SessionDiffState::Empty
                && session.stats.input_tokens == 0
                && session.stats.output_tokens == 0
        ));
        let forked_session = app
            .sessions
            .session_for_id(forked_session_id)
            .expect("missing forked session");
        assert_eq!(
            std::fs::read_to_string(forked_session.folder.join("README.md"))
                .expect("failed to read forked worktree"),
            "test"
        );

        let forked_messages = app
            .services
            .db()
            .sessions()
            .load_session_messages(forked_session_id)
            .await
            .expect("failed to load forked messages");
        assert_eq!(forked_messages.len(), 2);
        assert_eq!(forked_messages[0].kind, "user_prompt");
        assert_eq!(forked_messages[0].content, FORK_SOURCE_PROMPT);
        assert_eq!(forked_messages[1].kind, "assistant_answer");
        assert_eq!(forked_messages[1].content, FORK_SOURCE_ANSWER);
        assert_eq!(
            app.services
                .db()
                .sessions()
                .get_session_provider_conversation_id(forked_session_id)
                .await
                .expect("failed to load fork provider conversation id"),
            None
        );
        assert_eq!(
            app.services
                .db()
                .sessions()
                .get_session_instruction_conversation_id(forked_session_id)
                .await
                .expect("failed to load fork instruction conversation id"),
            None
        );
    }

    #[tokio::test]
    async fn test_fork_session_from_dirty_source_refreshes_fork_diff_state() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let source_session_id = create_review_source_session_for_fork_test(&mut app).await;

        // Act
        let forked_session_id = app
            .fork_session(&source_session_id)
            .await
            .expect("expected session fork to succeed");

        // Assert
        assert_ne!(forked_session_id, source_session_id);
        assert_forked_session_snapshot(&app, &forked_session_id).await;
    }

    #[tokio::test]
    async fn test_fork_session_rejects_non_review_source_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create source session");

        // Act
        let result = app.fork_session(&source_session_id).await;

        // Assert
        assert!(matches!(
            result,
            Err(AppError::Session(crate::app::SessionError::Workflow(message)))
                if message == "Only root review-ready sessions can be forked"
        ));
    }

    #[tokio::test]
    async fn test_fork_session_rejects_stacked_child_source_session() {
        // Arrange
        let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
            Arc::new(MockTmuxClient::new()),
        )
        .await;
        let child_session = crate::test_support::SessionFixtureBuilder::new()
            .id("child-source")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .status(Status::Review)
            .build();
        app.sessions.push_session(child_session);

        // Act
        let result = app.fork_session("child-source").await;

        // Assert
        assert!(matches!(
            result,
            Err(AppError::Session(crate::app::SessionError::Workflow(message)))
                if message == "Only root review-ready sessions can be forked"
        ));
    }
}
