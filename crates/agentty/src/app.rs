//! App-layer composition root and shared state container.
//!
//! This module wires app submodules and exposes [`App`] used by runtime mode
//! handlers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::permission::{PermissionMode, PlanFollowup};
use crate::domain::plan::extract_plan_questions;
use crate::domain::session::{CodexUsageLimits, Session, Status};
use crate::infra::db::Database;
use crate::infra::git::{GitClient, RealGitClient};
use crate::ui::state::app_mode::AppMode;

mod assist;
mod merge_queue;
mod project;
mod service;
pub(crate) mod session;
pub(crate) mod settings;
pub(crate) mod tab;
mod task;

// Export state for use by runtime
pub mod state;
use merge_queue::{MergeQueue, MergeQueueProgress};
pub use project::ProjectManager;
pub use service::AppServices;
pub use session::SessionManager;
pub(crate) use session::SyncSessionStartError;
pub use settings::SettingsManager;
pub use state::SessionState;
pub use tab::{Tab, TabManager};
use task::TaskService;

/// Relative directory name used for session git worktrees under `~/.agentty`.
pub const AGENTTY_WT_DIR: &str = "wt";

/// Returns the agentty home directory (`~/.agentty`).
pub fn agentty_home() -> PathBuf {
    if let Some(home_dir) = dirs::home_dir() {
        return home_dir.join(".agentty");
    }

    PathBuf::from(".agentty")
}

/// Internal app events emitted by background workers and workflows.
///
/// Producers should emit events only; state mutation is centralized in
/// [`App::apply_app_events`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    /// Indicates latest ahead/behind information from the git status worker.
    GitStatusUpdated { status: Option<(u32, u32)> },
    /// Indicates latest account-level Codex usage limits from the poller.
    CodexUsageLimitsUpdated {
        codex_usage_limits: Option<CodexUsageLimits>,
    },
    /// Indicates whether a newer stable `agentty` release is available.
    VersionAvailabilityUpdated {
        latest_available_version: Option<String>,
    },
    /// Indicates a session history reset has been persisted.
    SessionHistoryCleared { session_id: String },
    /// Indicates a session model selection has been persisted.
    SessionModelUpdated {
        session_id: String,
        session_model: AgentModel,
    },
    /// Indicates a session permission mode selection has been persisted.
    SessionPermissionModeUpdated {
        permission_mode: PermissionMode,
        session_id: String,
    },
    /// Requests a full session list refresh.
    RefreshSessions,
    /// Indicates compact live progress text for an in-progress session.
    SessionProgressUpdated {
        progress_message: Option<String>,
        session_id: String,
    },
    /// Indicates completion of a list-mode sync workflow.
    SyncMainCompleted {
        result: Result<(), SyncSessionStartError>,
    },
    /// Indicates that a session handle snapshot changed in-memory.
    SessionUpdated { session_id: String },
}

#[derive(Default)]
struct AppEventBatch {
    codex_usage_limits_update: CodexUsageLimitsBatchUpdate,
    cleared_session_history_ids: HashSet<String>,
    git_status_update: Option<(u32, u32)>,
    has_git_status_update: bool,
    has_latest_available_version_update: bool,
    latest_available_version_update: Option<String>,
    session_model_updates: HashMap<String, AgentModel>,
    session_progress_updates: HashMap<String, Option<String>>,
    session_ids: HashSet<String>,
    session_permission_mode_updates: HashMap<String, PermissionMode>,
    should_force_reload: bool,
    sync_main_result: Option<Result<(), SyncSessionStartError>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CodexUsageLimitsBatchUpdate {
    #[default]
    NotSet,
    PreservePrevious,
    Replace(CodexUsageLimits),
}

// SessionState definition moved to state.rs

/// Stores application state and coordinates session/project workflows.
pub struct App {
    pub mode: AppMode,
    pub settings: SettingsManager,
    /// Manages the selected top-level list tab.
    pub tabs: TabManager,
    pub(crate) projects: ProjectManager,
    pub(crate) services: AppServices,
    pub(crate) sessions: SessionManager,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    latest_available_version: Option<String>,
    merge_queue: MergeQueue,
    plan_followups: HashMap<String, PlanFollowup>,
    session_progress_messages: HashMap<String, String>,
}

impl App {
    /// Builds the app state from persisted data and starts background
    /// housekeeping tasks.
    pub async fn new(
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
    ) -> Self {
        let active_project_id = db
            .upsert_project(&working_dir.to_string_lossy(), git_branch.as_deref())
            .await
            .unwrap_or(0);

        let _ = db.backfill_session_project(active_project_id).await;

        let git_client: Arc<dyn GitClient> = Arc::new(RealGitClient);

        ProjectManager::discover_sibling_projects(&working_dir, &db, Arc::clone(&git_client)).await;
        SessionManager::fail_unfinished_operations_from_previous_run(&db).await;

        let projects = ProjectManager::load_projects_from_db(&db).await;

        let mut table_state = TableState::default();
        let mut handles = HashMap::new();
        let (sessions, stats_activity) = SessionManager::load_sessions(
            &base_path,
            &db,
            &projects,
            &mut handles,
            Arc::clone(&git_client),
        )
        .await;
        let all_time_model_usage = SessionManager::load_all_time_model_usage(&db).await;
        let codex_usage_limits = SessionManager::load_codex_usage_limits().await;
        let longest_session_duration_seconds =
            SessionManager::load_longest_session_duration_seconds(&db).await;
        let (sessions_row_count, sessions_updated_at_max) =
            db.load_sessions_metadata().await.unwrap_or((0, 0));
        let default_session_permission_mode = sessions
            .first()
            .map_or(PermissionMode::default(), |session| session.permission_mode);
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }

        let git_status_cancel = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let services = AppServices::new(base_path, db.clone(), event_tx.clone(), git_client);
        let projects = ProjectManager::new(
            active_project_id,
            git_branch,
            Arc::clone(&git_status_cancel),
            projects,
            working_dir,
        );
        let settings = SettingsManager::new(&services).await;
        let default_session_model = SessionManager::load_default_session_model(
            &services,
            AgentKind::Gemini.default_model(),
        )
        .await;
        let sessions = SessionManager::new(
            all_time_model_usage,
            codex_usage_limits,
            session::SessionDefaults {
                model: default_session_model,
                permission_mode: default_session_permission_mode,
            },
            services.git_client(),
            longest_session_duration_seconds,
            SessionState::new(
                handles,
                sessions,
                table_state,
                sessions_row_count,
                sessions_updated_at_max,
            ),
            stats_activity,
        );

        task::TaskService::spawn_codex_usage_limits_task(&event_tx);
        task::TaskService::spawn_version_check_task(&event_tx);
        if projects.has_git_branch() {
            task::TaskService::spawn_git_status_task(
                projects.working_dir(),
                projects.git_status_cancel(),
                event_tx,
                services.git_client(),
            );
        }

        Self {
            mode: AppMode::List,
            settings,
            tabs: TabManager::new(),
            projects,
            services,
            sessions,
            event_rx,
            latest_available_version: None,
            merge_queue: MergeQueue::default(),
            plan_followups: HashMap::new(),
            session_progress_messages: HashMap::new(),
        }
    }

    /// Returns the active project identifier.
    pub fn active_project_id(&self) -> i64 {
        self.projects.active_project_id()
    }

    /// Returns the working directory for the active project.
    pub fn working_dir(&self) -> &Path {
        self.projects.working_dir()
    }

    /// Returns the git branch of the active project, when available.
    pub fn git_branch(&self) -> Option<&str> {
        self.projects.git_branch()
    }

    /// Returns the latest ahead/behind snapshot from reducer-applied events.
    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.projects.git_status()
    }

    /// Returns the newer stable `agentty` version when an update is available.
    pub fn latest_available_version(&self) -> Option<&str> {
        self.latest_available_version.as_deref()
    }

    /// Returns whether the onboarding screen should be shown.
    pub fn should_show_onboarding(&self) -> bool {
        self.sessions.sessions.is_empty()
    }

    /// Moves selection to the next session in the list.
    pub fn next(&mut self) {
        self.sessions.next();
    }

    /// Moves selection to the previous session in the list.
    pub fn previous(&mut self) {
        self.sessions.previous();
    }

    /// Creates a blank session and schedules list refresh through events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_session(&mut self) -> Result<String, String> {
        let session_id = self
            .sessions
            .create_session(&self.projects, &self.services)
            .await?;
        self.process_pending_app_events().await;

        let index = self
            .sessions
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .unwrap_or(0);
        self.sessions.table_state.select(Some(index));

        Ok(session_id)
    }

    /// Submits the initial prompt for a newly created session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or task enqueue fails.
    pub async fn start_session(&mut self, session_id: &str, prompt: String) -> Result<(), String> {
        self.sessions
            .start_session(&self.services, session_id, prompt)
            .await
    }

    /// Sends SIGINT to the running session process.
    ///
    /// # Errors
    /// Returns an error if the session is not currently running.
    pub async fn stop_session(&self, session_id: &str) -> Result<(), String> {
        self.sessions.stop_session(&self.services, session_id).await
    }

    /// Submits a follow-up prompt for an existing session.
    pub async fn reply(&mut self, session_id: &str, prompt: &str) {
        self.sessions
            .reply(&self.services, session_id, prompt)
            .await;
    }

    /// Persists and applies a model selection for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_model(
        &mut self,
        session_id: &str,
        session_model: AgentModel,
    ) -> Result<(), String> {
        self.sessions
            .set_session_model(&self.services, session_id, session_model)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Toggles and persists a session permission mode.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn toggle_session_permission_mode(&mut self, session_id: &str) -> Result<(), String> {
        self.sessions
            .toggle_session_permission_mode(&self.services, session_id)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Sets and persists a session permission mode.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_permission_mode(
        &mut self,
        session_id: &str,
        permission_mode: PermissionMode,
    ) -> Result<(), String> {
        self.sessions
            .set_session_permission_mode(&self.services, session_id, permission_mode)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Clears persisted and in-memory history for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn clear_session_history(&mut self, session_id: &str) -> Result<(), String> {
        self.sessions
            .clear_session_history(&self.services, session_id)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.selected_session()
    }

    /// Returns session id by list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<String> {
        self.sessions.session_id_for_index(session_index)
    }

    /// Resolves a session id to current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.sessions.session_index_for_id(session_id)
    }

    /// Returns post-plan follow-up state for a session.
    pub fn plan_followup(&self, session_id: &str) -> Option<&PlanFollowup> {
        self.plan_followups.get(session_id)
    }

    /// Returns a snapshot of pending post-plan follow-ups by session id.
    pub fn plan_followup_snapshot(&self) -> HashMap<String, PlanFollowup> {
        self.plan_followups.clone()
    }

    /// Returns compact live progress text for a session, if available.
    pub fn session_progress_message(&self, session_id: &str) -> Option<&str> {
        self.session_progress_messages
            .get(session_id)
            .map(std::string::String::as_str)
    }

    /// Returns a snapshot of compact live progress text by session id.
    pub fn session_progress_snapshot(&self) -> HashMap<String, String> {
        self.session_progress_messages.clone()
    }

    /// Returns whether a session has pending post-plan actions.
    pub fn has_plan_followup_action(&self, session_id: &str) -> bool {
        self.plan_followups.contains_key(session_id)
    }

    /// Selects the previous post-plan action for a session.
    pub fn select_previous_plan_followup_action(&mut self, session_id: &str) {
        if let Some(followup) = self.plan_followups.get_mut(session_id) {
            followup.select_previous();
        }
    }

    /// Selects the next post-plan action for a session.
    pub fn select_next_plan_followup_action(&mut self, session_id: &str) {
        if let Some(followup) = self.plan_followups.get_mut(session_id) {
            followup.select_next();
        }
    }

    /// Removes and returns the full plan followup state for a session.
    pub fn take_plan_followup(&mut self, session_id: &str) -> Option<PlanFollowup> {
        self.plan_followups.remove(session_id)
    }

    /// Re-inserts an updated plan followup state for a session.
    pub fn put_plan_followup(&mut self, session_id: &str, followup: PlanFollowup) {
        self.plan_followups.insert(session_id.to_string(), followup);
    }

    /// Deletes the selected session and schedules list refresh.
    pub async fn delete_selected_session(&mut self) {
        self.sessions
            .delete_selected_session(&self.projects, &self.services)
            .await;
        self.process_pending_app_events().await;
    }

    /// Cancels a session in review status.
    ///
    /// # Errors
    /// Returns an error if the session is not found or not in review status.
    pub async fn cancel_session(&self, session_id: &str) -> Result<(), String> {
        self.sessions
            .cancel_session(&self.services, session_id)
            .await
    }

    /// Opens the selected session worktree in tmux and optionally starts the
    /// configured Dev Server command.
    pub async fn open_session_worktree_in_tmux(&self) {
        let Some(session) = self.selected_session() else {
            return;
        };

        let Some(window_id) = Self::open_tmux_window_for_folder(&session.folder).await else {
            return;
        };

        let Some(dev_server_command) =
            Self::dev_server_command_to_run(self.settings.dev_server.as_str())
        else {
            return;
        };

        Self::run_tmux_command_in_window(&window_id, dev_server_command).await;
    }

    /// Appends output text to a session stream and persists it.
    pub(crate) async fn append_output_for_session(&self, session_id: &str, output: &str) {
        self.sessions
            .append_output_for_session(&self.services, session_id, output)
            .await;
    }

    /// Switches active project and refreshes session snapshot state.
    ///
    /// # Errors
    /// Returns an error if the project id does not exist.
    pub async fn switch_project(&mut self, project_id: i64) -> Result<(), String> {
        self.projects
            .switch_project(project_id, &self.services, &mut self.sessions)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Starts squash-merge workflow for a review-ready session.
    ///
    /// # Errors
    /// Returns an error if session is not mergeable, queueing fails, or
    /// immediate merge start fails while the queue is idle.
    pub async fn merge_session(&mut self, session_id: &str) -> Result<(), String> {
        if self.merge_queue.is_queued_or_active(session_id) {
            return Ok(());
        }

        self.validate_merge_request(session_id)?;
        if self.merge_queue.has_active() {
            self.mark_session_as_queued_for_merge(session_id).await?;
            self.merge_queue.enqueue(session_id.to_string());

            return Ok(());
        }

        self.merge_queue.enqueue(session_id.to_string());

        self.start_next_merge_from_queue(true).await
    }

    /// Rebases a reviewed session branch onto its base branch.
    ///
    /// # Errors
    /// Returns an error if session cannot start rebasing.
    pub async fn rebase_session(&self, session_id: &str) -> Result<(), String> {
        self.sessions
            .rebase_session(&self.services, session_id)
            .await
    }

    /// Starts selected-project branch sync in the background and immediately
    /// opens a loading popup.
    pub(crate) fn start_sync_main(&mut self) {
        self.mode = AppMode::SyncBlockedPopup {
            is_loading: true,
            message: "Synchronizing the selected project branch with its upstream.".to_string(),
            title: "Sync in progress".to_string(),
        };

        let app_event_tx = self.services.event_sender();
        let default_branch = self.projects.git_branch().map(str::to_string);
        let working_dir = self.projects.working_dir().to_path_buf();
        let git_client = self.services.git_client();
        let permission_mode = self.sessions.default_session_permission_mode();
        let session_model = self.sessions.default_session_model();

        tokio::spawn(async move {
            let result = SessionManager::sync_main_for_project(
                default_branch,
                working_dir,
                git_client,
                permission_mode,
                session_model,
            )
            .await;
            let _ = app_event_tx.send(AppEvent::SyncMainCompleted { result });
        });
    }

    /// Reloads sessions when metadata cache indicates changes.
    pub async fn refresh_sessions_if_needed(&mut self) {
        self.sessions
            .refresh_sessions_if_needed(&mut self.mode, &self.projects, &self.services)
            .await;
    }

    /// Forces immediate session list reload.
    pub(crate) async fn refresh_sessions_now(&mut self) {
        self.sessions
            .refresh_sessions_now(&mut self.mode, &self.projects, &self.services)
            .await;
    }

    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh,
    /// git-status, and Codex usage updates, then applies session-handle sync
    /// for touched sessions.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = self.drain_app_events(first_event);
        let event_batch = Self::reduce_app_events(drained_events);

        self.apply_app_event_batch(event_batch).await;
    }

    /// Processes currently queued app events without waiting.
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

    fn drain_app_events(&mut self, first_event: AppEvent) -> Vec<AppEvent> {
        let mut events = vec![first_event];
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }

        events
    }

    fn reduce_app_events(events: Vec<AppEvent>) -> AppEventBatch {
        let mut event_batch = AppEventBatch::default();
        for event in events {
            event_batch.collect_event(event);
        }

        event_batch
    }

    async fn apply_app_event_batch(&mut self, event_batch: AppEventBatch) {
        let previous_session_states = event_batch
            .session_ids
            .iter()
            .filter_map(|session_id| {
                self.sessions
                    .sessions
                    .iter()
                    .find(|session| session.id == *session_id)
                    .map(|session| {
                        (
                            session_id.clone(),
                            (session.permission_mode, session.status),
                        )
                    })
            })
            .collect::<HashMap<_, _>>();

        if event_batch.should_force_reload {
            self.refresh_sessions_now().await;
        }

        if event_batch.has_git_status_update {
            self.projects.set_git_status(event_batch.git_status_update);
        }

        if event_batch.has_latest_available_version_update {
            self.latest_available_version = event_batch.latest_available_version_update;
        }

        match event_batch.codex_usage_limits_update {
            CodexUsageLimitsBatchUpdate::NotSet => {}
            CodexUsageLimitsBatchUpdate::PreservePrevious => {
                self.sessions.apply_codex_usage_limits_update(None);
            }
            CodexUsageLimitsBatchUpdate::Replace(codex_usage_limits) => {
                self.sessions
                    .apply_codex_usage_limits_update(Some(codex_usage_limits));
            }
        }

        for session_id in &event_batch.cleared_session_history_ids {
            self.sessions.apply_session_history_cleared(session_id);
        }

        for (session_id, session_model) in event_batch.session_model_updates {
            self.sessions
                .apply_session_model_updated(&session_id, session_model);
        }

        for (session_id, permission_mode) in event_batch.session_permission_mode_updates {
            self.sessions
                .apply_session_permission_mode_updated(&session_id, permission_mode);
        }

        for (session_id, progress_message) in event_batch.session_progress_updates {
            if let Some(progress_message) = progress_message {
                self.session_progress_messages
                    .insert(session_id, progress_message);
            } else {
                self.session_progress_messages.remove(&session_id);
            }
        }

        for session_id in &event_batch.session_ids {
            self.sessions.sync_session_from_handle(session_id);
        }

        if let Some(sync_main_result) = event_batch.sync_main_result {
            self.mode = Self::sync_main_popup_mode(sync_main_result);
        }

        self.handle_merge_queue_progress(&event_batch.session_ids, &previous_session_states)
            .await;
        self.mark_plan_followup_actions(&event_batch.session_ids, &previous_session_states);
        self.retain_valid_plan_followup_actions();
        self.retain_valid_session_progress_messages();
    }

    /// Validates whether a session is currently eligible for merge queueing.
    ///
    /// Sessions are eligible while actively under review or already marked as
    /// `Queued` (for example, after app restart).
    ///
    /// # Errors
    /// Returns an error when the session does not exist or has an ineligible
    /// status.
    fn validate_merge_request(&self, session_id: &str) -> Result<(), String> {
        let session = self.sessions.session_or_err(session_id)?;
        if !matches!(session.status, Status::Review | Status::Queued) {
            return Err("Session must be in review or queued status".to_string());
        }

        Ok(())
    }

    /// Marks one session as waiting in the merge queue.
    ///
    /// # Errors
    /// Returns an error when status transition to `Queued` is invalid.
    async fn mark_session_as_queued_for_merge(&self, session_id: &str) -> Result<(), String> {
        let handles = self.sessions.session_handles_or_err(session_id)?;
        let app_event_tx = self.services.event_sender();
        let status_updated = TaskService::update_status(
            handles.status.as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Queued,
        )
        .await;

        if !status_updated {
            return Err("Invalid status transition to Queued".to_string());
        }

        Ok(())
    }

    /// Restores a queued session to `Review` if merge start fails.
    async fn restore_queued_session_to_review(&self, session_id: &str) {
        let session_status = self
            .sessions
            .session_or_err(session_id)
            .map(|session| session.status);
        if session_status != Ok(Status::Queued) {
            return;
        }

        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = self.services.event_sender();
        let _ = TaskService::update_status(
            handles.status.as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Review,
        )
        .await;
    }

    /// Starts the next pending merge request when no merge is currently active.
    ///
    /// When `stop_on_failure` is `true`, returns the first start error.
    /// Otherwise, failed entries are skipped and the queue continues.
    ///
    /// # Errors
    /// Returns an error when starting a queued merge fails and
    /// `stop_on_failure` is enabled.
    async fn start_next_merge_from_queue(&mut self, stop_on_failure: bool) -> Result<(), String> {
        if self.merge_queue.has_active() {
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

                    let merge_error = format!("\n[Merge Error] {error}\n");
                    self.append_output_for_session(&next_session_id, &merge_error)
                        .await;

                    if stop_on_failure {
                        return Err(error);
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
    async fn handle_merge_queue_progress(
        &mut self,
        session_ids: &HashSet<String>,
        previous_session_states: &HashMap<String, (PermissionMode, Status)>,
    ) {
        let current_status = self
            .merge_queue
            .active_session_id()
            .and_then(|active_session_id| {
                self.sessions
                    .sessions
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
            let _ = self.start_next_merge_from_queue(false).await;
        }
    }

    fn mark_plan_followup_actions(
        &mut self,
        session_ids: &HashSet<String>,
        previous_session_states: &HashMap<String, (PermissionMode, Status)>,
    ) {
        for session_id in session_ids {
            let Some(session) = self
                .sessions
                .sessions
                .iter()
                .find(|session| session.id == *session_id)
            else {
                self.plan_followups.remove(session_id);

                continue;
            };

            if session.permission_mode != PermissionMode::Plan || session.status != Status::Review {
                self.plan_followups.remove(session_id);

                continue;
            }

            let Some((_, previous_status)) = previous_session_states.get(session_id) else {
                continue;
            };

            if *previous_status == Status::InProgress {
                let questions = extract_plan_questions(&session.output);
                self.plan_followups
                    .insert(session_id.clone(), PlanFollowup::new(questions));
            }
        }
    }

    fn retain_valid_plan_followup_actions(&mut self) {
        self.plan_followups.retain(|session_id, _| {
            self.sessions
                .sessions
                .iter()
                .find(|session| session.id == *session_id)
                .is_some_and(|session| {
                    session.permission_mode == PermissionMode::Plan
                        && session.status == Status::Review
                })
        });
    }

    fn retain_valid_session_progress_messages(&mut self) {
        self.session_progress_messages.retain(|session_id, _| {
            self.sessions
                .sessions
                .iter()
                .find(|session| session.id == *session_id)
                .is_some_and(|session| {
                    matches!(
                        session.status,
                        Status::InProgress | Status::Rebasing | Status::Merging
                    )
                })
        });
    }

    /// Opens a new tmux window in the provided folder and returns its window
    /// id.
    async fn open_tmux_window_for_folder(session_folder: &Path) -> Option<String> {
        let output = tokio::process::Command::new("tmux")
            .arg("new-window")
            .arg("-P")
            .arg("-F")
            .arg("#{window_id}")
            .arg("-c")
            .arg(session_folder)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        Self::parse_tmux_window_id(&output.stdout)
    }

    /// Returns the Dev Server command to execute when it is configured.
    fn dev_server_command_to_run(dev_server: &str) -> Option<&str> {
        let command = dev_server.trim();
        if command.is_empty() {
            return None;
        }

        Some(command)
    }

    /// Sends the provided command and Enter key to a tmux window.
    async fn run_tmux_command_in_window(window_id: &str, command: &str) {
        let send_literal_output = tokio::process::Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(window_id)
            .arg("-l")
            .arg(command)
            .output()
            .await;

        let Ok(send_literal_output) = send_literal_output else {
            return;
        };
        if !send_literal_output.status.success() {
            return;
        }

        let _ = tokio::process::Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(window_id)
            .arg("C-m")
            .output()
            .await;
    }

    /// Parses a tmux window id from command output bytes.
    fn parse_tmux_window_id(stdout: &[u8]) -> Option<String> {
        let window_id = std::str::from_utf8(stdout).ok()?.trim();
        if window_id.is_empty() {
            return None;
        }

        Some(window_id.to_string())
    }

    /// Builds final sync popup mode from background sync completion result.
    fn sync_main_popup_mode(sync_main_result: Result<(), SyncSessionStartError>) -> AppMode {
        match sync_main_result {
            Ok(()) => AppMode::SyncBlockedPopup {
                is_loading: false,
                message: "Successfully synchronized the selected project branch with its upstream."
                    .to_string(),
                title: "Sync complete".to_string(),
            },
            Err(sync_error @ SyncSessionStartError::MainHasUncommittedChanges { .. }) => {
                AppMode::SyncBlockedPopup {
                    is_loading: false,
                    message: sync_error.detail_message(),
                    title: "Sync blocked".to_string(),
                }
            }
            Err(sync_error @ SyncSessionStartError::Other(_)) => AppMode::SyncBlockedPopup {
                is_loading: false,
                message: sync_error.detail_message(),
                title: "Sync failed".to_string(),
            },
        }
    }
}

impl AppEventBatch {
    fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::GitStatusUpdated { status } => {
                self.has_git_status_update = true;
                self.git_status_update = status;
            }
            AppEvent::CodexUsageLimitsUpdated { codex_usage_limits } => {
                if let Some(codex_usage_limits) = codex_usage_limits {
                    self.codex_usage_limits_update =
                        CodexUsageLimitsBatchUpdate::Replace(codex_usage_limits);
                } else if matches!(
                    self.codex_usage_limits_update,
                    CodexUsageLimitsBatchUpdate::NotSet
                ) {
                    self.codex_usage_limits_update = CodexUsageLimitsBatchUpdate::PreservePrevious;
                }
            }
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            } => {
                self.has_latest_available_version_update = true;
                self.latest_available_version_update = latest_available_version;
            }
            AppEvent::SessionHistoryCleared { session_id } => {
                self.cleared_session_history_ids.insert(session_id);
            }
            AppEvent::SessionModelUpdated {
                session_id,
                session_model,
            } => {
                self.session_model_updates.insert(session_id, session_model);
            }
            AppEvent::SessionPermissionModeUpdated {
                permission_mode,
                session_id,
            } => {
                self.session_permission_mode_updates
                    .insert(session_id, permission_mode);
            }
            AppEvent::RefreshSessions => {
                self.should_force_reload = true;
            }
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => {
                self.session_progress_updates
                    .insert(session_id, progress_message);
            }
            AppEvent::SyncMainCompleted { result } => {
                self.sync_main_result = Some(result);
            }
            AppEvent::SessionUpdated { session_id } => {
                self.session_ids.insert(session_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{CodexUsageLimitWindow, CodexUsageLimits};

    #[test]
    fn dev_server_command_to_run_returns_none_for_empty_input() {
        // Arrange
        let dev_server = "   ";

        // Act
        let command = App::dev_server_command_to_run(dev_server);

        // Assert
        assert_eq!(command, None);
    }

    #[test]
    fn dev_server_command_to_run_trims_and_returns_command() {
        // Arrange
        let dev_server = "  npm run dev -- --port 5173  ";

        // Act
        let command = App::dev_server_command_to_run(dev_server);

        // Assert
        assert_eq!(command, Some("npm run dev -- --port 5173"));
    }

    #[test]
    fn parse_tmux_window_id_returns_none_for_invalid_utf8() {
        // Arrange
        let stdout = [0x80];

        // Act
        let window_id = App::parse_tmux_window_id(&stdout);

        // Assert
        assert_eq!(window_id, None);
    }

    #[test]
    fn parse_tmux_window_id_trims_newline_and_returns_window_id() {
        // Arrange
        let stdout = b"@42\n";

        // Act
        let window_id = App::parse_tmux_window_id(stdout);

        // Assert
        assert_eq!(window_id, Some("@42".to_string()));
    }

    #[test]
    fn app_event_batch_collect_event_keeps_latest_successful_codex_usage_limits_update() {
        // Arrange
        let mut event_batch = AppEventBatch::default();
        let initial_limits = limits_fixture(24, 33);

        // Act
        event_batch.collect_event(AppEvent::CodexUsageLimitsUpdated {
            codex_usage_limits: Some(initial_limits),
        });
        event_batch.collect_event(AppEvent::CodexUsageLimitsUpdated {
            codex_usage_limits: None,
        });

        // Assert
        assert_eq!(
            event_batch.codex_usage_limits_update,
            CodexUsageLimitsBatchUpdate::Replace(initial_limits)
        );
    }

    /// Builds deterministic Codex usage-limit snapshots for tests.
    fn limits_fixture(primary_used_percent: u8, secondary_used_percent: u8) -> CodexUsageLimits {
        CodexUsageLimits {
            primary: Some(CodexUsageLimitWindow {
                resets_at: Some(1),
                used_percent: primary_used_percent,
                window_minutes: Some(300),
            }),
            secondary: Some(CodexUsageLimitWindow {
                resets_at: Some(2),
                used_percent: secondary_used_percent,
                window_minutes: Some(10_080),
            }),
        }
    }
}
