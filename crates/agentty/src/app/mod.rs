use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use crate::agent::{AgentKind, AgentModel};
use crate::db::Database;
use crate::model::{AppMode, PermissionMode, Project, Session, SessionHandles, Tab};

mod pr;
mod project;
pub(crate) mod session;
mod task;
mod title;
mod worker;

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
    /// Indicates a PR creation task has finished for a session.
    PrCreationCleared { session_id: String },
    /// Indicates a PR polling task has stopped for a session.
    PrPollingStopped { session_id: String },
    /// Requests a full session list refresh.
    RefreshSessions,
    /// Indicates that a session handle snapshot changed in-memory.
    SessionUpdated { session_id: String },
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
        let Some(handles) = self.handles.get(session_id) else {
            return;
        };
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        if let Ok(output) = handles.output.lock()
            && session.output.len() != output.len()
        {
            session.output.clone_from(&*output);
        }
        if let Ok(status) = handles.status.lock() {
            session.status = *status;
        }
        if let Ok(count) = handles.commit_count.lock() {
            session.commit_count = *count;
        }
    }

    /// Copies current values from runtime handles into plain `Session` fields.
    pub fn sync_from_handles(&mut self) {
        let session_ids: Vec<String> = self
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();

        for session_id in session_ids {
            self.sync_session_from_handle(&session_id);
        }
    }
}

/// Stores application state and coordinates session/project workflows.
pub struct App {
    pub current_tab: Tab,
    pub mode: AppMode,
    pub projects: Vec<Project>,
    pub session_state: SessionState,
    active_project_id: i64,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    base_path: PathBuf,
    db: Database,
    default_session_agent: AgentKind,
    default_session_model: AgentModel,
    default_session_permission_mode: PermissionMode,
    git_branch: Option<String>,
    git_status: Option<(u32, u32)>,
    git_status_cancel: Arc<AtomicBool>,
    pr_creation_in_flight: Arc<Mutex<HashSet<String>>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    session_workers: HashMap<String, mpsc::UnboundedSender<worker::SessionCommand>>,
    working_dir: PathBuf,
}

impl App {
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

        Self::discover_sibling_projects(&working_dir, &db).await;
        Self::fail_unfinished_operations_from_previous_run(&db).await;

        let projects = Self::load_projects_from_db(&db).await;

        let mut table_state = TableState::default();
        let mut handles = HashMap::new();
        let sessions = Self::load_sessions(&base_path, &db, &projects, &mut handles).await;
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
                        Self::resolve_session_agent_and_model(session);

                    (session_agent, session_model, session.permission_mode)
                },
            );
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }

        let git_status = None;
        let git_status_cancel = Arc::new(AtomicBool::new(false));
        let pr_creation_in_flight = Arc::new(Mutex::new(HashSet::new()));
        let pr_poll_cancel = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        if git_branch.is_some() {
            Self::spawn_git_status_task(
                &working_dir,
                Arc::clone(&git_status_cancel),
                event_tx.clone(),
            );
        }

        let app = Self {
            current_tab: Tab::Sessions,
            mode: AppMode::List,
            session_state: SessionState::new(
                handles,
                sessions,
                table_state,
                sessions_row_count,
                sessions_updated_at_max,
            ),
            active_project_id,
            event_rx,
            event_tx,
            base_path,
            db,
            default_session_agent,
            default_session_model,
            default_session_permission_mode,
            git_branch,
            git_status,
            git_status_cancel,
            pr_creation_in_flight,
            pr_poll_cancel,
            projects,
            session_workers: HashMap::new(),
            working_dir,
        };

        app.start_pr_polling_for_pull_request_sessions();

        app
    }

    pub fn active_project_id(&self) -> i64 {
        self.active_project_id
    }

    pub fn working_dir(&self) -> &PathBuf {
        &self.working_dir
    }

    pub fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Returns the latest ahead/behind snapshot from reducer-applied events.
    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.git_status
    }

    /// Returns whether the onboarding screen should be shown.
    pub fn should_show_onboarding(&self) -> bool {
        self.session_state.sessions.is_empty()
    }

    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh and
    /// git-status updates, then applies session-handle sync for touched
    /// sessions.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let mut cleared_pr_creation_ids: HashSet<String> = HashSet::new();
        let mut has_git_status_update = false;
        let mut stopped_pr_poll_ids: HashSet<String> = HashSet::new();
        let mut should_force_reload = false;
        let mut git_status_update = None;
        let mut session_ids: HashSet<String> = HashSet::new();
        Self::collect_app_event(
            &mut cleared_pr_creation_ids,
            &mut has_git_status_update,
            &mut stopped_pr_poll_ids,
            &mut should_force_reload,
            &mut git_status_update,
            &mut session_ids,
            first_event,
        );

        while let Ok(event) = self.event_rx.try_recv() {
            Self::collect_app_event(
                &mut cleared_pr_creation_ids,
                &mut has_git_status_update,
                &mut stopped_pr_poll_ids,
                &mut should_force_reload,
                &mut git_status_update,
                &mut session_ids,
                event,
            );
        }

        if should_force_reload {
            self.refresh_sessions_now().await;
        }

        if has_git_status_update {
            self.git_status = git_status_update;
        }

        if let Ok(mut in_flight) = self.pr_creation_in_flight.lock() {
            for session_id in &cleared_pr_creation_ids {
                in_flight.remove(session_id);
            }
        }

        if let Ok(mut polling) = self.pr_poll_cancel.lock() {
            for session_id in &stopped_pr_poll_ids {
                polling.remove(session_id);
            }
        }

        for session_id in &session_ids {
            self.session_state.sync_session_from_handle(session_id);
        }
    }

    /// Enqueues an app event onto the internal app event bus.
    pub(crate) fn emit_app_event(&self, event: AppEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Processes currently queued app events without waiting.
    pub(crate) async fn process_pending_app_events(&mut self) {
        let Ok(first_event) = self.event_rx.try_recv() else {
            return;
        };

        self.apply_app_events(first_event).await;
    }

    /// Emits one app event and immediately processes the pending app queue.
    pub(crate) async fn dispatch_app_event(&mut self, event: AppEvent) {
        self.emit_app_event(event);
        self.process_pending_app_events().await;
    }

    /// Returns a clone of the internal app event sender.
    pub(crate) fn app_event_sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.event_tx.clone()
    }

    /// Waits for the next internal app event.
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Collects one event into aggregate reducer state for this loop pass.
    fn collect_app_event(
        cleared_pr_creation_ids: &mut HashSet<String>,
        has_git_status_update: &mut bool,
        stopped_pr_poll_ids: &mut HashSet<String>,
        should_force_reload: &mut bool,
        git_status_update: &mut Option<(u32, u32)>,
        session_ids: &mut HashSet<String>,
        event: AppEvent,
    ) {
        match event {
            AppEvent::GitStatusUpdated { status } => {
                *has_git_status_update = true;
                *git_status_update = status;
            }
            AppEvent::PrCreationCleared { session_id } => {
                cleared_pr_creation_ids.insert(session_id);
            }
            AppEvent::PrPollingStopped { session_id } => {
                stopped_pr_poll_ids.insert(session_id);
            }
            AppEvent::RefreshSessions => {
                *should_force_reload = true;
            }
            AppEvent::SessionUpdated { session_id } => {
                session_ids.insert(session_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

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
    fn test_collect_app_event_tracks_git_status_and_refresh() {
        // Arrange
        let mut cleared_pr_creation_ids = HashSet::new();
        let mut has_git_status_update = false;
        let mut stopped_pr_poll_ids = HashSet::new();
        let mut should_force_reload = false;
        let mut git_status_update = None;
        let mut session_ids = HashSet::new();

        // Act
        App::collect_app_event(
            &mut cleared_pr_creation_ids,
            &mut has_git_status_update,
            &mut stopped_pr_poll_ids,
            &mut should_force_reload,
            &mut git_status_update,
            &mut session_ids,
            AppEvent::SessionUpdated {
                session_id: "session-1".to_string(),
            },
        );
        App::collect_app_event(
            &mut cleared_pr_creation_ids,
            &mut has_git_status_update,
            &mut stopped_pr_poll_ids,
            &mut should_force_reload,
            &mut git_status_update,
            &mut session_ids,
            AppEvent::GitStatusUpdated {
                status: Some((2, 1)),
            },
        );
        App::collect_app_event(
            &mut cleared_pr_creation_ids,
            &mut has_git_status_update,
            &mut stopped_pr_poll_ids,
            &mut should_force_reload,
            &mut git_status_update,
            &mut session_ids,
            AppEvent::RefreshSessions,
        );
        App::collect_app_event(
            &mut cleared_pr_creation_ids,
            &mut has_git_status_update,
            &mut stopped_pr_poll_ids,
            &mut should_force_reload,
            &mut git_status_update,
            &mut session_ids,
            AppEvent::PrCreationCleared {
                session_id: "session-2".to_string(),
            },
        );
        App::collect_app_event(
            &mut cleared_pr_creation_ids,
            &mut has_git_status_update,
            &mut stopped_pr_poll_ids,
            &mut should_force_reload,
            &mut git_status_update,
            &mut session_ids,
            AppEvent::PrPollingStopped {
                session_id: "session-3".to_string(),
            },
        );

        // Assert
        assert_eq!(cleared_pr_creation_ids.len(), 1);
        assert!(cleared_pr_creation_ids.contains("session-2"));
        assert!(has_git_status_update);
        assert_eq!(stopped_pr_poll_ids.len(), 1);
        assert!(stopped_pr_poll_ids.contains("session-3"));
        assert!(should_force_reload);
        assert_eq!(git_status_update, Some((2, 1)));
        assert_eq!(session_ids.len(), 1);
        assert!(session_ids.contains("session-1"));
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
}
