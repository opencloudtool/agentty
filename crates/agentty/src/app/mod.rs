//! App-layer composition root and shared state container.
//!
//! This module wires app submodules and exposes [`App`] and [`SessionState`]
//! used by runtime mode handlers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use crate::agent::{AgentKind, AgentModel};
use crate::db::Database;
use crate::model::{
    AppMode, PermissionMode, PlanFollowupAction, Session, SessionHandles, Status, Tab,
};

mod assist;
mod pr;
mod project;
mod service;
pub(crate) mod session;
mod task;
mod title;

pub use pr::PrManager;
pub use project::ProjectManager;
pub use service::AppServices;
pub use session::SessionManager;

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
    /// Indicates a session history reset has been persisted.
    SessionHistoryCleared { session_id: String },
    /// Indicates a session agent/model selection has been persisted.
    SessionAgentModelUpdated {
        session_agent: AgentKind,
        session_id: String,
        session_model: AgentModel,
    },
    /// Indicates a session permission mode selection has been persisted.
    SessionPermissionModeUpdated {
        permission_mode: PermissionMode,
        session_id: String,
    },
    /// Indicates a PR creation task has finished for a session.
    PrCreationCleared { session_id: String },
    /// Indicates a PR polling task has stopped for a session.
    PrPollingStopped { session_id: String },
    /// Requests a full session list refresh.
    RefreshSessions,
    /// Indicates that a session handle snapshot changed in-memory.
    SessionUpdated { session_id: String },
}

#[derive(Default)]
struct AppEventBatch {
    cleared_pr_creation_ids: HashSet<String>,
    cleared_session_history_ids: HashSet<String>,
    git_status_update: Option<(u32, u32)>,
    has_git_status_update: bool,
    session_agent_model_updates: HashMap<String, (AgentKind, AgentModel)>,
    session_ids: HashSet<String>,
    session_permission_mode_updates: HashMap<String, PermissionMode>,
    should_force_reload: bool,
    stopped_pr_poll_ids: HashSet<String>,
}

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    pub handles: HashMap<String, SessionHandles>,
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    refresh_deadline: std::time::Instant,
    row_count: i64,
    updated_at_max: i64,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    pub fn new(
        handles: HashMap<String, SessionHandles>,
        sessions: Vec<Session>,
        table_state: TableState,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        Self {
            handles,
            sessions,
            table_state,
            refresh_deadline: std::time::Instant::now() + session::SESSION_REFRESH_INTERVAL,
            row_count,
            updated_at_max,
        }
    }

    /// Copies current values from one runtime handle into its `Session`
    /// snapshot.
    pub fn sync_session_from_handle(&mut self, session_id: &str) {
        let Some(session_handles) = self.handles.get(session_id) else {
            return;
        };
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        Self::sync_session_with_handles(session, session_handles);
    }

    /// Copies current values from runtime handles into plain `Session` fields.
    pub fn sync_from_handles(&mut self) {
        let handles = &self.handles;

        for session in &mut self.sessions {
            let Some(session_handles) = handles.get(&session.id) else {
                continue;
            };

            Self::sync_session_with_handles(session, session_handles);
        }
    }

    fn sync_session_with_handles(session: &mut Session, session_handles: &SessionHandles) {
        if let Ok(output) = session_handles.output.lock()
            && session.output.len() != output.len()
        {
            session.output.clone_from(&*output);
        }

        if let Ok(status) = session_handles.status.lock() {
            session.status = *status;
        }
    }
}

/// Stores application state and coordinates session/project workflows.
pub struct App {
    pub current_tab: Tab,
    pub mode: AppMode,
    pub(crate) projects: ProjectManager,
    pub(crate) prs: PrManager,
    pub(crate) services: AppServices,
    pub(crate) sessions: SessionManager,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    plan_followup_actions: HashMap<String, PlanFollowupAction>,
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

        ProjectManager::discover_sibling_projects(&working_dir, &db).await;
        SessionManager::fail_unfinished_operations_from_previous_run(&db).await;

        let projects = ProjectManager::load_projects_from_db(&db).await;

        let mut table_state = TableState::default();
        let mut handles = HashMap::new();
        let sessions =
            SessionManager::load_sessions(&base_path, &db, &projects, &mut handles).await;
        let (sessions_row_count, sessions_updated_at_max) =
            db.load_sessions_metadata().await.unwrap_or((0, 0));
        let (default_session_agent, default_session_model, default_session_permission_mode) =
            sessions.first().map_or(
                (
                    AgentKind::Gemini,
                    AgentKind::Gemini.default_model(),
                    PermissionMode::default(),
                ),
                |session| {
                    let (session_agent, session_model) =
                        title::TitleService::resolve_session_agent_and_model(session);

                    (session_agent, session_model, session.permission_mode)
                },
            );
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }

        let git_status_cancel = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let services = AppServices::new(base_path, db.clone(), event_tx.clone());
        let projects = ProjectManager::new(
            active_project_id,
            git_branch,
            Arc::clone(&git_status_cancel),
            projects,
            working_dir,
        );
        let sessions = SessionManager::new(
            default_session_agent,
            default_session_model,
            default_session_permission_mode,
            SessionState::new(
                handles,
                sessions,
                table_state,
                sessions_row_count,
                sessions_updated_at_max,
            ),
        );
        let prs = PrManager::new();

        if projects.has_git_branch() {
            task::TaskService::spawn_git_status_task(
                projects.working_dir(),
                projects.git_status_cancel(),
                event_tx,
            );
        }

        let app = Self {
            current_tab: Tab::Sessions,
            mode: AppMode::List,
            projects,
            prs,
            services,
            sessions,
            event_rx,
            plan_followup_actions: HashMap::new(),
        };

        app.start_pr_polling_for_pull_request_sessions();

        app
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

    /// Returns whether the onboarding screen should be shown.
    pub fn should_show_onboarding(&self) -> bool {
        self.sessions.sessions.is_empty()
    }

    /// Selects the next top-level tab.
    pub fn next_tab(&mut self) {
        self.current_tab = self.current_tab.next();
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

    /// Persists and applies an agent/model selection for a session.
    ///
    /// # Errors
    /// Returns an error if the model is invalid for the selected agent or
    /// persistence fails.
    pub async fn set_session_agent_and_model(
        &mut self,
        session_id: &str,
        session_agent: AgentKind,
        session_model: AgentModel,
    ) -> Result<(), String> {
        self.sessions
            .set_session_agent_and_model(&self.services, session_id, session_agent, session_model)
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

    /// Returns the currently selected post-plan action for a session.
    pub fn plan_followup_action(&self, session_id: &str) -> Option<PlanFollowupAction> {
        self.plan_followup_actions.get(session_id).copied()
    }

    /// Returns a snapshot of pending post-plan actions by session id.
    pub fn plan_followup_actions_snapshot(&self) -> HashMap<String, PlanFollowupAction> {
        self.plan_followup_actions.clone()
    }

    /// Returns whether a session has pending post-plan actions.
    pub fn has_plan_followup_action(&self, session_id: &str) -> bool {
        self.plan_followup_actions.contains_key(session_id)
    }

    /// Selects the previous post-plan action for a session.
    pub fn select_previous_plan_followup_action(&mut self, session_id: &str) {
        if let Some(action) = self.plan_followup_actions.get_mut(session_id) {
            *action = action.previous();
        }
    }

    /// Selects the next post-plan action for a session.
    pub fn select_next_plan_followup_action(&mut self, session_id: &str) {
        if let Some(action) = self.plan_followup_actions.get_mut(session_id) {
            *action = action.next();
        }
    }

    /// Clears and returns the pending post-plan action for a session.
    pub fn consume_plan_followup_action(&mut self, session_id: &str) -> Option<PlanFollowupAction> {
        self.plan_followup_actions.remove(session_id)
    }

    /// Deletes the selected session and schedules list refresh.
    pub async fn delete_selected_session(&mut self) {
        self.sessions
            .delete_selected_session(&self.projects, &self.prs, &self.services)
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

    /// Opens the selected session's worktree in a new tmux window.
    pub async fn open_session_worktree_in_tmux(&self) {
        if let Some(session) = self.selected_session() {
            let _ = tokio::process::Command::new("tmux")
                .arg("new-window")
                .arg("-c")
                .arg(&session.folder)
                .output()
                .await;
        }
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

    /// Starts squash-merge workflow for a reviewed session.
    ///
    /// # Errors
    /// Returns an error if session is not mergeable or state transition is
    /// invalid.
    pub async fn merge_session(&self, session_id: &str) -> Result<(), String> {
        self.sessions
            .merge_session(session_id, &self.projects, &self.prs, &self.services)
            .await
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

    /// Starts pull-request creation for a reviewed session.
    ///
    /// # Errors
    /// Returns an error if session is not eligible.
    pub async fn create_pr_session(&self, session_id: &str) -> Result<(), String> {
        self.prs
            .create_pr_session(&self.services, &self.sessions, session_id)
            .await
    }

    /// Reloads sessions when metadata cache indicates changes.
    pub async fn refresh_sessions_if_needed(&mut self) {
        self.sessions
            .refresh_sessions_if_needed(&mut self.mode, &self.projects, &self.prs, &self.services)
            .await;
    }

    /// Forces immediate session list reload.
    pub(crate) async fn refresh_sessions_now(&mut self) {
        self.sessions
            .refresh_sessions_now(&mut self.mode, &self.projects, &self.prs, &self.services)
            .await;
    }

    /// Ensures PR polling tasks run for sessions currently in `PullRequest`.
    pub(super) fn start_pr_polling_for_pull_request_sessions(&self) {
        self.prs
            .start_pr_polling_for_pull_request_sessions(&self.services, &self.sessions);
    }

    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh and
    /// git-status updates, then applies session-handle sync for touched
    /// sessions.
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

        if let Ok(mut in_flight) = self.prs.pr_creation_in_flight().lock() {
            for session_id in &event_batch.cleared_pr_creation_ids {
                in_flight.remove(session_id);
            }
        }

        if let Ok(mut polling) = self.prs.pr_poll_cancel().lock() {
            for session_id in &event_batch.stopped_pr_poll_ids {
                polling.remove(session_id);
            }
        }

        for session_id in &event_batch.cleared_session_history_ids {
            self.sessions.apply_session_history_cleared(session_id);
        }

        for (session_id, (session_agent, session_model)) in event_batch.session_agent_model_updates
        {
            self.sessions.apply_session_agent_model_updated(
                &session_id,
                session_agent,
                session_model,
            );
        }

        for (session_id, permission_mode) in event_batch.session_permission_mode_updates {
            self.sessions
                .apply_session_permission_mode_updated(&session_id, permission_mode);
        }

        for session_id in &event_batch.session_ids {
            self.sessions.sync_session_from_handle(session_id);
        }

        self.mark_plan_followup_actions(&event_batch.session_ids, &previous_session_states);
        self.retain_valid_plan_followup_actions();
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
                self.plan_followup_actions.remove(session_id);

                continue;
            };

            if session.permission_mode != PermissionMode::Plan || session.status != Status::Review {
                self.plan_followup_actions.remove(session_id);

                continue;
            }

            let Some((_, previous_status)) = previous_session_states.get(session_id) else {
                continue;
            };

            if *previous_status == Status::InProgress {
                self.plan_followup_actions
                    .insert(session_id.clone(), PlanFollowupAction::ImplementPlan);
            }
        }
    }

    fn retain_valid_plan_followup_actions(&mut self) {
        self.plan_followup_actions.retain(|session_id, _| {
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
}

impl AppEventBatch {
    fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::GitStatusUpdated { status } => {
                self.has_git_status_update = true;
                self.git_status_update = status;
            }
            AppEvent::SessionHistoryCleared { session_id } => {
                self.cleared_session_history_ids.insert(session_id);
            }
            AppEvent::SessionAgentModelUpdated {
                session_agent,
                session_id,
                session_model,
            } => {
                self.session_agent_model_updates
                    .insert(session_id, (session_agent, session_model));
            }
            AppEvent::SessionPermissionModeUpdated {
                permission_mode,
                session_id,
            } => {
                self.session_permission_mode_updates
                    .insert(session_id, permission_mode);
            }
            AppEvent::PrCreationCleared { session_id } => {
                self.cleared_pr_creation_ids.insert(session_id);
            }
            AppEvent::PrPollingStopped { session_id } => {
                self.stopped_pr_poll_ids.insert(session_id);
            }
            AppEvent::RefreshSessions => {
                self.should_force_reload = true;
            }
            AppEvent::SessionUpdated { session_id } => {
                self.session_ids.insert(session_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn test_should_show_onboarding_when_database_is_empty() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let app = App::new(base_path, working_dir, None, database).await;

        // Assert
        assert!(app.should_show_onboarding());
    }

    #[tokio::test]
    async fn test_should_show_onboarding_when_database_has_sessions() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&working_dir.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "seed0000",
                "gemini",
                "gemini-2.5-flash",
                "main",
                "Done",
                project_id,
            )
            .await
            .expect("failed to insert session");

        // Act
        let app = App::new(base_path, working_dir, None, database).await;

        // Assert
        assert!(!app.should_show_onboarding());
    }

    #[test]
    fn test_app_event_batch_collects_git_status_and_refresh() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::GitStatusUpdated {
            status: Some((2, 1)),
        });
        event_batch.collect_event(AppEvent::RefreshSessions);

        // Assert
        assert!(event_batch.has_git_status_update);
        assert_eq!(event_batch.git_status_update, Some((2, 1)));
        assert!(event_batch.should_force_reload);
    }

    #[test]
    fn test_app_event_batch_collects_session_updates() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::SessionUpdated {
            session_id: "session-1".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionHistoryCleared {
            session_id: "session-2".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionAgentModelUpdated {
            session_agent: AgentKind::Claude,
            session_id: "session-3".to_string(),
            session_model: AgentModel::ClaudeOpus46,
        });
        event_batch.collect_event(AppEvent::SessionPermissionModeUpdated {
            permission_mode: PermissionMode::Autonomous,
            session_id: "session-4".to_string(),
        });

        // Assert
        assert!(event_batch.session_ids.contains("session-1"));
        assert!(
            event_batch
                .cleared_session_history_ids
                .contains("session-2")
        );
        assert_eq!(
            event_batch.session_agent_model_updates.get("session-3"),
            Some(&(AgentKind::Claude, AgentModel::ClaudeOpus46))
        );
        assert_eq!(
            event_batch.session_permission_mode_updates.get("session-4"),
            Some(&PermissionMode::Autonomous)
        );
    }

    #[test]
    fn test_app_event_batch_collects_pr_updates() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::PrCreationCleared {
            session_id: "session-1".to_string(),
        });
        event_batch.collect_event(AppEvent::PrPollingStopped {
            session_id: "session-2".to_string(),
        });

        // Assert
        assert!(event_batch.cleared_pr_creation_ids.contains("session-1"));
        assert!(event_batch.stopped_pr_poll_ids.contains("session-2"));
    }

    #[tokio::test]
    async fn test_apply_app_events_updates_git_status() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(base_path, working_dir, None, database).await;

        // Act
        app.apply_app_events(AppEvent::GitStatusUpdated {
            status: Some((5, 3)),
        })
        .await;

        // Assert
        assert_eq!(app.git_status_info(), Some((5, 3)));
    }

    #[tokio::test]
    async fn test_apply_app_events_marks_plan_followup_after_plan_response() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&working_dir.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "seed0000",
                "gemini",
                "gemini-2.5-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        std::fs::create_dir_all(base_path.join("seed0000"))
            .expect("failed to create session folder");
        let mut app = App::new(base_path, working_dir, None, database).await;
        let session_id = app.sessions.sessions[0].id.clone();
        app.sessions.sessions[0].permission_mode = PermissionMode::Plan;
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut status) = handles.status.lock()
        {
            *status = Status::Review;
        }

        // Act
        app.apply_app_events(AppEvent::SessionUpdated {
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(app.has_plan_followup_action(&session_id));
        assert_eq!(
            app.plan_followup_action(&session_id),
            Some(PlanFollowupAction::ImplementPlan)
        );
    }

    #[tokio::test]
    async fn test_apply_app_events_does_not_mark_plan_followup_for_non_plan_mode() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&working_dir.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "seed0000",
                "gemini",
                "gemini-2.5-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        std::fs::create_dir_all(base_path.join("seed0000"))
            .expect("failed to create session folder");
        let mut app = App::new(base_path, working_dir, None, database).await;
        let session_id = app.sessions.sessions[0].id.clone();
        app.sessions.sessions[0].permission_mode = PermissionMode::AutoEdit;
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut status) = handles.status.lock()
        {
            *status = Status::Review;
        }

        // Act
        app.apply_app_events(AppEvent::SessionUpdated {
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(!app.has_plan_followup_action(&session_id));
    }

    #[tokio::test]
    async fn test_apply_app_events_clears_plan_followup_when_permission_mode_changes() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&working_dir.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "seed0000",
                "gemini",
                "gemini-2.5-flash",
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");
        std::fs::create_dir_all(base_path.join("seed0000"))
            .expect("failed to create session folder");
        let mut app = App::new(base_path, working_dir, None, database).await;
        let session_id = app.sessions.sessions[0].id.clone();
        app.sessions.sessions[0].permission_mode = PermissionMode::Plan;
        app.plan_followup_actions
            .insert(session_id.clone(), PlanFollowupAction::TypeFeedback);

        // Act
        app.apply_app_events(AppEvent::SessionPermissionModeUpdated {
            permission_mode: PermissionMode::AutoEdit,
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert_eq!(
            app.sessions.sessions[0].permission_mode,
            PermissionMode::AutoEdit
        );
        assert!(!app.has_plan_followup_action(&session_id));
    }

    #[tokio::test]
    async fn test_set_session_permission_mode_updates_session_and_database() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&working_dir.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "seed0000",
                "gemini",
                "gemini-2.5-flash",
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");
        std::fs::create_dir_all(base_path.join("seed0000"))
            .expect("failed to create session folder");
        let mut app = App::new(base_path, working_dir, None, database).await;
        let session_id = app.sessions.sessions[0].id.clone();

        // Act
        app.set_session_permission_mode(&session_id, PermissionMode::Plan)
            .await
            .expect("failed to set session permission mode");

        // Assert
        assert_eq!(
            app.sessions.sessions[0].permission_mode,
            PermissionMode::Plan
        );
        let db_session = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(db_session[0].permission_mode, PermissionMode::Plan.label());
    }
}
