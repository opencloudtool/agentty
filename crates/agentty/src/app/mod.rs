use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use crate::db::Database;
use crate::health::{self, HealthEntry};
use crate::model::{AppMode, Project, Session, Tab};

mod pr;
mod project;
pub(crate) mod session;
mod task;
mod title;

pub const AGENTTY_WT_DIR: &str = "wt";

#[derive(Debug)]
pub enum AgentEvent {
    Output {
        session_id: String,
        text: String,
    },
    Error {
        session_id: String,
        error: String,
    },
    Finished {
        session_id: String,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    },
}

/// Returns the agentty home directory (`~/.agentty`).
///
/// # Panics
///
/// Panics if the home directory cannot be determined.
pub fn agentty_home() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".agentty")
}

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    refresh_deadline: std::time::Instant,
    row_count: i64,
    updated_at_max: i64,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    pub fn new(
        sessions: Vec<Session>,
        table_state: TableState,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        Self {
            sessions,
            table_state,
            refresh_deadline: std::time::Instant::now() + session::SESSION_REFRESH_INTERVAL,
            row_count,
            updated_at_max,
        }
    }
}

/// Stores application state and coordinates session/project workflows.
pub struct App {
    pub agent_tx: mpsc::UnboundedSender<AgentEvent>,
    pub agent_rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    pub current_tab: Tab,
    pub mode: AppMode,
    pub projects: Vec<Project>,
    pub session_state: SessionState,
    active_project_id: i64,
    base_path: PathBuf,
    db: Database,
    git_branch: Option<String>,
    git_status: Arc<Mutex<Option<(u32, u32)>>>,
    git_status_cancel: Arc<AtomicBool>,
    health_checks: Arc<Mutex<Vec<HealthEntry>>>,
    pr_creation_in_flight: Arc<Mutex<HashSet<String>>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    working_dir: PathBuf,
}

impl App {
    pub async fn new(
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
    ) -> Self {
        let (agent_tx, agent_rx) = mpsc::unbounded_channel();
        let active_project_id = db
            .upsert_project(&working_dir.to_string_lossy(), git_branch.as_deref())
            .await
            .unwrap_or(0);

        let _ = db.backfill_sessions_project(active_project_id).await;

        Self::discover_sibling_projects(&working_dir, &db).await;

        let projects = Self::load_projects_from_db(&db).await;

        let mut table_state = TableState::default();
        let sessions = Self::load_sessions(&base_path, &db, &projects, &[]).await;
        let (sessions_row_count, sessions_updated_at_max) =
            db.load_sessions_metadata().await.unwrap_or((0, 0));
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }

        let git_status = Arc::new(Mutex::new(None));
        let git_status_cancel = Arc::new(AtomicBool::new(false));
        let pr_creation_in_flight = Arc::new(Mutex::new(HashSet::new()));
        let pr_poll_cancel = Arc::new(Mutex::new(HashMap::new()));

        if git_branch.is_some() {
            Self::spawn_git_status_task(
                &working_dir,
                Arc::clone(&git_status),
                Arc::clone(&git_status_cancel),
            );
        }

        let app = Self {
            agent_tx,
            agent_rx: Some(agent_rx),
            current_tab: Tab::Sessions,
            mode: AppMode::List,
            session_state: SessionState::new(
                sessions,
                table_state,
                sessions_row_count,
                sessions_updated_at_max,
            ),
            active_project_id,
            base_path,
            db,
            git_branch,
            git_status,
            git_status_cancel,
            health_checks: Arc::new(Mutex::new(Vec::new())),
            pr_creation_in_flight,
            pr_poll_cancel,
            projects,
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

    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.git_status.lock().ok().and_then(|status| *status)
    }

    pub fn health_checks(&self) -> &Arc<Mutex<Vec<HealthEntry>>> {
        &self.health_checks
    }

    /// Returns whether the onboarding screen should be shown.
    pub fn should_show_onboarding(&self) -> bool {
        self.session_state.sessions.is_empty()
    }

    pub fn start_health_checks(&mut self) {
        self.health_checks = health::run_health_checks(self.git_branch.clone());
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
}
