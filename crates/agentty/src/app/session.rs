//! Session lifecycle orchestration for creation, refresh, prompt handling,
//! history management, merge, and cleanup.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use crate::app::settings::SettingName;
use crate::app::{AppServices, SessionState};
use crate::domain::agent::AgentModel;
use crate::domain::permission::PermissionMode;
use crate::domain::session::{AllTimeModelUsage, CodexUsageLimits, DailyActivity, Session, Status};

mod access;
mod lifecycle;
mod load;
mod merge;
mod refresh;
mod worker;

pub(crate) use merge::SyncSessionStartError;

/// Render payload tuple returned by [`SessionManager::render_parts`].
type SessionRenderParts<'a> = (
    &'a [Session],
    &'a [DailyActivity],
    &'a [AllTimeModelUsage],
    u64,
    Option<CodexUsageLimits>,
    &'a mut TableState,
);

/// Low-frequency fallback interval for metadata-based session refresh.
pub(super) const SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Default commit message used for automatic worktree commits.
pub(super) const COMMIT_MESSAGE: &str = "Beautiful commit (made by Agentty)";

/// Defaults used when creating new sessions from the UI.
#[derive(Clone, Copy)]
pub(crate) struct SessionDefaults {
    pub(crate) model: AgentModel,
    pub(crate) permission_mode: PermissionMode,
}

/// Session domain state and worker orchestration state.
pub struct SessionManager {
    all_time_model_usage: Vec<AllTimeModelUsage>,
    codex_usage_limits: Option<CodexUsageLimits>,
    default_session_model: AgentModel,
    default_session_permission_mode: PermissionMode,
    git_client: Arc<dyn crate::infra::git::GitClient>,
    longest_session_duration_seconds: u64,
    pending_history_replay: HashSet<String>,
    state: SessionState,
    stats_activity: Vec<DailyActivity>,
    workers: HashMap<String, mpsc::UnboundedSender<crate::app::session::worker::SessionCommand>>,
}

impl SessionManager {
    /// Creates a session manager from persisted snapshot state and defaults.
    pub(crate) fn new(
        all_time_model_usage: Vec<AllTimeModelUsage>,
        codex_usage_limits: Option<CodexUsageLimits>,
        defaults: SessionDefaults,
        git_client: Arc<dyn crate::infra::git::GitClient>,
        longest_session_duration_seconds: u64,
        state: SessionState,
        stats_activity: Vec<DailyActivity>,
    ) -> Self {
        Self {
            all_time_model_usage,
            codex_usage_limits,
            default_session_model: defaults.model,
            default_session_permission_mode: defaults.permission_mode,
            git_client,
            longest_session_duration_seconds,
            pending_history_replay: HashSet::new(),
            state,
            stats_activity,
            workers: HashMap::new(),
        }
    }

    /// Returns the configured session git client used by orchestration flows.
    pub(crate) fn git_client(&self) -> Arc<dyn crate::infra::git::GitClient> {
        Arc::clone(&self.git_client)
    }

    /// Returns the default model used for session-scoped agent workflows.
    pub(crate) fn default_session_model(&self) -> AgentModel {
        self.default_session_model
    }

    /// Returns the default permission mode used for session-scoped agent
    /// workflows.
    pub(crate) fn default_session_permission_mode(&self) -> PermissionMode {
        self.default_session_permission_mode
    }

    /// Loads the default model persisted for new sessions.
    pub(crate) async fn load_default_session_model(
        services: &AppServices,
        fallback_model: AgentModel,
    ) -> AgentModel {
        services
            .db()
            .get_setting(SettingName::DefaultModel.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|setting_value| setting_value.parse().ok())
            .unwrap_or(fallback_model)
    }

    /// Returns session snapshots and stats payloads required for rendering.
    ///
    /// The tuple contains live sessions, activity heatmap data, all-time model
    /// usage, persisted longest session duration, Codex usage limits, and list
    /// table state.
    pub(crate) fn render_parts(&mut self) -> SessionRenderParts<'_> {
        (
            &self.state.sessions,
            &self.stats_activity,
            &self.all_time_model_usage,
            self.longest_session_duration_seconds,
            self.codex_usage_limits,
            &mut self.state.table_state,
        )
    }

    /// Applies reducer updates after session history was cleared.
    pub(crate) fn apply_session_history_cleared(&mut self, session_id: &str) {
        self.pending_history_replay.remove(session_id);

        if let Some(handles) = self.state.handles.get(session_id) {
            if let Ok(mut output_buffer) = handles.output.lock() {
                output_buffer.clear();
            }

            if let Ok(mut status_value) = handles.status.lock() {
                *status_value = Status::New;
            }
        }

        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.output.clear();
            session.prompt = String::new();
            session.summary = None;
            session.title = None;
            session.status = Status::New;
        }
    }

    /// Applies reducer updates after session agent/model changes are
    /// persisted.
    pub(crate) fn apply_session_model_updated(
        &mut self,
        session_id: &str,
        session_model: AgentModel,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.model = session_model;
        }

        self.default_session_model = session_model;
    }

    /// Applies reducer updates after session permission mode changes are
    /// persisted.
    pub(crate) fn apply_session_permission_mode_updated(
        &mut self,
        session_id: &str,
        permission_mode: PermissionMode,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.permission_mode = permission_mode;
        }

        self.default_session_permission_mode = permission_mode;
    }
}

impl Deref for SessionManager {
    type Target = SessionState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for SessionManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

/// Returns the folder path for a session under the given base directory.
fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    let len = session_id.len().min(8);
    base.join(&session_id[..len])
}

/// Returns the worktree branch name for a session.
pub(crate) fn session_branch(session_id: &str) -> String {
    let len = session_id.len().min(8);
    format!("agentty/{}", &session_id[..len])
}

#[cfg(test)]
mod tests {
    //! Session module tests.

    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::*;
    use crate::app::settings::SettingName;
    use crate::app::{App, SyncSessionStartError, Tab};
    use crate::domain::agent::{AgentKind, AgentModel};
    use crate::domain::permission::PermissionMode;
    use crate::domain::project::Project;
    use crate::domain::session::{
        DailyActivity, SESSION_DATA_DIR, Session, SessionHandles, SessionSize, SessionStats, Status,
    };
    use crate::infra::agent::MockAgentBackend;
    use crate::infra::db::Database;
    use crate::infra::git;
    use crate::ui::state::app_mode::AppMode;

    fn create_mock_backend() -> MockAgentBackend {
        let mut mock = MockAgentBackend::new();
        mock.expect_build_start_command()
            .returning(|folder, _, _, _, _| {
                let mut cmd = Command::new("echo");
                cmd.arg("mock-start")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        mock
    }

    fn create_mock_git_client_for_successful_noop_merges(
        expected_merge_count: usize,
        repo_root: PathBuf,
    ) -> git::MockGitClient {
        let mut mock = git::MockGitClient::new();
        mock.expect_find_git_repo_root()
            .times(expected_merge_count)
            .returning(move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Some(repo_root) })
            });
        mock.expect_commit_all_preserving_single_commit()
            .times(expected_merge_count)
            .returning(|_, _, _| {
                Box::pin(async { Err("Nothing to commit: no changes detected".to_string()) })
            });
        mock.expect_is_rebase_in_progress()
            .times(expected_merge_count)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock.expect_rebase_start()
            .times(expected_merge_count)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock.expect_squash_merge_diff()
            .times(expected_merge_count)
            .returning(|_, _, _| Box::pin(async { Ok(String::new()) }));
        mock.expect_remove_worktree()
            .times(expected_merge_count)
            .returning(|worktree_path| {
                Box::pin(async move {
                    let _ = tokio::fs::remove_dir_all(worktree_path).await;

                    Ok(())
                })
            });
        mock.expect_delete_branch()
            .times(expected_merge_count)
            .returning(|_, _| Box::pin(async { Ok(()) }));

        mock
    }

    async fn new_test_app(path: PathBuf) -> App {
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        App::new(path, working_dir, None, db).await
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
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
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

    async fn new_test_app_with_git(path: &Path) -> App {
        setup_test_git_repo(path);
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        App::new(
            path.to_path_buf(),
            path.to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await
    }

    /// Adds a manual review session snapshot for tests that do not require
    /// status customization.
    fn add_manual_session(app: &mut App, base_path: &Path, id: &str, prompt: &str) {
        add_manual_session_with_status(app, base_path, id, prompt, Status::Review);
    }

    /// Adds a manual session snapshot with an explicit status.
    fn add_manual_session_with_status(
        app: &mut App,
        base_path: &Path,
        id: &str,
        prompt: &str,
        status: Status,
    ) {
        let folder = session_folder(base_path, id);
        let data_dir = folder.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        app.sessions
            .handles
            .insert(id.to_string(), SessionHandles::new(String::new(), status));
        app.sessions.sessions.push(Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder,
            id: id.to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            permission_mode: PermissionMode::default(),
            project_name: String::new(),
            prompt: prompt.to_string(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: Some(prompt.to_string()),
            updated_at: 0,
        });
        if app.sessions.table_state.selected().is_none() {
            app.sessions.table_state.select(Some(0));
        }
    }

    /// Helper: creates a session and starts it with the given prompt (two-step
    /// flow).
    async fn create_and_start_session(app: &mut App, prompt: &str) {
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let start_backend = create_mock_backend();
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                prompt,
                &start_backend,
                AgentModel::Gemini3FlashPreview,
            )
            .await;
    }

    async fn wait_for_status(app: &mut App, session_id: &str, expected: Status) {
        wait_for_status_with_retries(app, session_id, expected, 2000).await;
    }

    async fn wait_for_status_with_retries(
        app: &mut App,
        session_id: &str,
        expected: Status,
        retries: usize,
    ) {
        for _ in 0..retries {
            app.process_pending_app_events().await;
            app.sessions.sync_from_handles();
            let Some(session) = app
                .sessions
                .sessions
                .iter()
                .find(|session| session.id == session_id)
            else {
                break;
            };
            if session.status == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        app.process_pending_app_events().await;
        app.sessions.sync_from_handles();
        let session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing session while waiting for status");
        assert_eq!(
            session.status, expected,
            "session output while waiting for status: {}",
            session.output
        );
    }

    /// Forces one session status in both render snapshot and runtime handle.
    fn set_session_status_for_test(app: &mut App, session_id: &str, status: Status) {
        if let Some(session) = app
            .sessions
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.status = status;
        }

        if let Some(handles) = app.sessions.handles.get(session_id)
            && let Ok(mut current_status) = handles.status.lock()
        {
            *current_status = status;
        }
    }

    async fn wait_for_output_contains(
        app: &mut App,
        session_id: &str,
        expected_output: &str,
        retries: usize,
    ) {
        for _ in 0..retries {
            app.sessions.sync_from_handles();
            let Some(session) = app
                .sessions
                .sessions
                .iter()
                .find(|session| session.id == session_id)
            else {
                break;
            };
            if session.output.contains(expected_output) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        app.sessions.sync_from_handles();
        let session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing session while waiting for output");
        assert!(
            session.output.contains(expected_output),
            "expected output to contain: {expected_output}, actual output: {}",
            session.output
        );
    }

    /// Returns the current session status or `Done` when session is missing.
    fn session_status_or_done(app: &App, session_id: &str) -> Status {
        app.sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map_or(Status::Done, |session| session.status)
    }

    /// Returns whether a session currently has `Done` status.
    fn is_session_done(app: &App, session_id: &str) -> bool {
        app.sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| session.status == Status::Done)
    }

    /// Waits for the first merge to finish and asserts second merge is queued
    /// first instead of starting prematurely.
    async fn wait_for_first_merge_to_complete_before_second_starts(
        app: &mut App,
        first_session_id: &str,
        second_session_id: &str,
    ) {
        let mut first_merge_completed = false;
        let mut first_merge_pending_observed = false;
        let mut second_merge_was_queued = false;

        for _ in 0..5000 {
            app.process_pending_app_events().await;
            app.sessions.sync_from_handles();

            let first_status = session_status_or_done(app, first_session_id);
            let second_status = session_status_or_done(app, second_session_id);
            if second_status == Status::Queued {
                second_merge_was_queued = true;
            }
            if first_status == Status::Done {
                first_merge_completed = true;

                break;
            }
            first_merge_pending_observed = true;

            assert_ne!(
                second_status,
                Status::Merging,
                "second merge started before first completed"
            );

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        assert!(
            first_merge_completed,
            "first merge did not complete within timeout"
        );
        if first_merge_pending_observed {
            assert!(
                second_merge_was_queued,
                "second merge never entered queued status before first completed"
            );
        }
    }

    /// Waits for the queued second merge to enter `Merging` or `Done`.
    async fn wait_for_second_merge_to_start(app: &mut App, second_session_id: &str) {
        let mut second_merge_started = false;

        for _ in 0..5000 {
            app.process_pending_app_events().await;
            app.sessions.sync_from_handles();

            let second_status = session_status_or_done(app, second_session_id);
            if matches!(second_status, Status::Merging | Status::Done) {
                second_merge_started = true;

                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        assert!(
            second_merge_started,
            "second merge did not start after first completed"
        );
    }

    /// Waits until both provided sessions are marked as `Done`.
    async fn wait_for_all_sessions_done(
        app: &mut App,
        first_session_id: &str,
        second_session_id: &str,
    ) {
        for _ in 0..5000 {
            app.process_pending_app_events().await;
            app.sessions.sync_from_handles();

            if is_session_done(app, first_session_id) && is_session_done(app, second_session_id) {
                return;
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_new_app_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Assert
        assert!(app.sessions.sessions.is_empty());
        assert_eq!(app.sessions.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_working_dir_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let working_dir = app.working_dir();

        // Assert
        assert_eq!(working_dir, Path::new("/tmp/test"));
    }

    #[tokio::test]
    async fn test_git_branch_getter_with_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            dir.path().to_path_buf(),
            working_dir,
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let branch = app.git_branch();

        // Assert
        assert_eq!(branch, Some("main"));
    }

    #[tokio::test]
    async fn test_git_branch_getter_without_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let branch = app.git_branch();

        // Assert
        assert_eq!(branch, None);
    }

    #[tokio::test]
    async fn test_navigation() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        create_and_start_session(&mut app, "B").await;

        // Act & Assert (Next)
        app.sessions.table_state.select(Some(0));
        app.next();
        assert_eq!(app.sessions.table_state.selected(), Some(1));
        app.next();
        assert_eq!(app.sessions.table_state.selected(), Some(0)); // Loop back

        // Act & Assert (Previous)
        app.previous();
        assert_eq!(app.sessions.table_state.selected(), Some(1)); // Loop back
        app.previous();
        assert_eq!(app.sessions.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_navigation_follows_grouped_order_and_skips_group_headers() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session_with_status(
            &mut app,
            dir.path(),
            "archive-1",
            "Archive 1",
            Status::Done,
        );
        add_manual_session_with_status(
            &mut app,
            dir.path(),
            "active-1",
            "Active 1",
            Status::Review,
        );
        add_manual_session_with_status(
            &mut app,
            dir.path(),
            "queued-1",
            "Queued 1",
            Status::Queued,
        );
        add_manual_session_with_status(
            &mut app,
            dir.path(),
            "archive-2",
            "Archive 2",
            Status::Canceled,
        );
        add_manual_session_with_status(&mut app, dir.path(), "merge-1", "Merge 1", Status::Merging);
        add_manual_session_with_status(&mut app, dir.path(), "active-2", "Active 2", Status::New);
        app.sessions.table_state.select(Some(3));

        // Act & Assert
        app.next();
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("queued-1")
        );

        app.next();
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("merge-1")
        );

        app.next();
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("active-1")
        );

        app.next();
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("active-2")
        );

        app.next();
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("archive-1")
        );

        app.next();
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("archive-2")
        );

        app.previous();
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("archive-1")
        );
    }

    #[tokio::test]
    async fn test_navigation_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        app.next();
        assert_eq!(app.sessions.table_state.selected(), None);

        app.previous();
        assert_eq!(app.sessions.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_navigation_recovery() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;

        // Act & Assert — next recovers from None
        app.sessions.table_state.select(None);
        app.next();
        assert_eq!(app.sessions.table_state.selected(), Some(0));

        // Act & Assert — previous recovers from None
        app.sessions.table_state.select(None);
        app.previous();
        assert_eq!(app.sessions.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_create_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;

        // Act
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Assert — blank session
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(session_id, app.sessions.sessions[0].id);
        assert!(app.sessions.sessions[0].prompt.is_empty());
        assert_eq!(app.sessions.sessions[0].title, None);
        assert_eq!(app.sessions.sessions[0].display_title(), "No title");
        assert_eq!(app.sessions.sessions[0].status, Status::New);
        assert_eq!(app.sessions.table_state.selected(), Some(0));
        assert_eq!(
            app.sessions.sessions[0].model,
            AgentKind::Gemini.default_model()
        );
        assert_eq!(
            app.sessions.sessions[0].permission_mode,
            PermissionMode::AutoEdit
        );

        // Check filesystem
        let session_dir = &app.sessions.sessions[0].folder;
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        assert!(session_dir.exists());
        assert!(data_dir.is_dir());

        // Check DB
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert_eq!(db_sessions.len(), 1);
        assert_eq!(db_sessions[0].base_branch, "main");
        assert_eq!(
            db_sessions[0].model,
            AgentKind::Gemini.default_model().as_str()
        );
        assert_eq!(
            db_sessions[0].permission_mode,
            PermissionMode::AutoEdit.label()
        );
        assert_eq!(db_sessions[0].status, "New");
    }

    #[tokio::test]
    async fn test_create_session_keeps_default_model_setting_when_session_model_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let first_session_id = app
            .create_session()
            .await
            .expect("failed to create first session");
        app.set_session_model(&first_session_id, AgentModel::Gpt52Codex)
            .await
            .expect("failed to set session model");
        app.toggle_session_permission_mode(&first_session_id)
            .await
            .expect("failed to toggle permission mode");
        let default_model_setting = app
            .services
            .db()
            .get_setting(SettingName::DefaultModel.as_str())
            .await
            .expect("failed to load setting");

        // Act
        let second_session_id = app
            .create_session()
            .await
            .expect("failed to create second session");

        // Assert
        let second_session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == second_session_id)
            .expect("missing second session");
        assert_eq!(second_session.model, AgentModel::Gpt52Codex);
        assert_eq!(second_session.permission_mode, PermissionMode::Autonomous);
        assert_eq!(default_model_setting, None);

        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        let db_second_session = db_sessions
            .iter()
            .find(|session| session.id == second_session_id)
            .expect("missing second session in db");
        assert_eq!(db_second_session.model, "gpt-5.2-codex");
        assert_eq!(
            db_second_session.permission_mode,
            PermissionMode::Autonomous.label()
        );
    }

    #[tokio::test]
    async fn test_create_session_reads_default_model_from_db_setting() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.services
            .db()
            .upsert_setting(
                SettingName::DefaultModel.as_str(),
                AgentModel::ClaudeHaiku4520251001.as_str(),
            )
            .await
            .expect("failed to upsert default model setting");

        // Act
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Assert
        let created_session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing created session");
        assert_eq!(created_session.model, AgentModel::ClaudeHaiku4520251001);
    }

    #[tokio::test]
    async fn test_start_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Act
        app.start_session(&session_id, "Hello".to_string())
            .await
            .expect("failed to start session");

        // Assert
        assert_eq!(app.sessions.sessions[0].prompt, "Hello");
        assert_eq!(app.sessions.sessions[0].title, Some("Hello".to_string()));
        app.sessions.sync_from_handles();
        let output = app.sessions.sessions[0].output.clone();
        assert!(output.contains("Hello"));
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert_eq!(db_sessions[0].prompt, "Hello");
        assert_eq!(db_sessions[0].output, " › Hello\n\n");
    }

    #[tokio::test]
    async fn test_start_session_uses_full_prompt_text_as_title() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let prompt = "First line\nSecond line is intentionally long to avoid truncation.";

        // Act
        app.start_session(&session_id, prompt.to_string())
            .await
            .expect("failed to start session");

        // Assert
        assert_eq!(app.sessions.sessions[0].title, Some(prompt.to_string()));
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert_eq!(db_sessions[0].title, Some(prompt.to_string()));
    }

    #[tokio::test]
    async fn test_reply_first_message_uses_full_prompt_text_as_title() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let prompt = "Line one\nLine two with more words for title text";
        let backend = create_mock_backend();

        // Act
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                prompt,
                &backend,
                AgentModel::Gemini3FlashPreview,
            )
            .await;

        // Assert
        assert_eq!(app.sessions.sessions[0].title, Some(prompt.to_string()));
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert_eq!(db_sessions[0].title, Some(prompt.to_string()));
    }

    #[tokio::test]
    async fn test_esc_deletes_blank_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing session index");
        let session_folder = app.sessions.sessions[session_index].folder.clone();
        assert!(session_folder.exists());

        // Act — simulate Esc: delete the blank session
        app.delete_selected_session().await;

        // Assert
        assert!(app.sessions.sessions.is_empty());
        assert!(!session_folder.exists());
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert!(db_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_reply() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Initial").await;
        let session_id = app.sessions.sessions[0].id.clone();

        // Act
        app.reply(&session_id, "Reply").await;

        // Assert
        app.sessions.sync_from_handles();
        let session = &app.sessions.sessions[0];
        let output = &session.output;
        assert!(output.contains("Reply"));
    }

    #[tokio::test]
    async fn test_selected_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Test").await;

        // Act & Assert
        assert!(app.selected_session().is_some());

        app.sessions.table_state.select(None);
        assert!(app.selected_session().is_none());
    }

    #[tokio::test]
    async fn test_delete_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        let session_folder = app.sessions.sessions[0].folder.clone();

        // Act
        app.delete_selected_session().await;

        // Assert
        assert!(app.sessions.sessions.is_empty());
        assert_eq!(app.sessions.table_state.selected(), None);
        assert!(!session_folder.exists());
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert!(db_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_delete_session_persists_longest_duration_setting() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let _session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.sessions[0].created_at = 0;
        app.sessions.sessions[0].updated_at = 18_000;

        // Act
        app.delete_selected_session().await;

        // Assert
        let longest_duration_setting = app
            .services
            .db()
            .get_setting(SettingName::LongestSessionDurationSeconds.as_str())
            .await
            .expect("failed to load longest session duration setting");
        assert_eq!(longest_duration_setting, Some("18000".to_string()));
    }

    #[tokio::test]
    async fn test_delete_selected_session_edge_cases() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "1").await;
        create_and_start_session(&mut app, "2").await;

        // Act & Assert — index out of bounds
        app.sessions.table_state.select(Some(99));
        app.delete_selected_session().await;
        assert_eq!(app.sessions.sessions.len(), 2);

        // Act & Assert — None selected
        app.sessions.table_state.select(None);
        app.delete_selected_session().await;
        assert_eq!(app.sessions.sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_last_session_update_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "1").await;
        create_and_start_session(&mut app, "2").await;

        // Act & Assert — delete last item
        app.sessions.table_state.select(Some(1));
        app.delete_selected_session().await;
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(app.sessions.table_state.selected(), Some(0));

        // Act & Assert — delete remaining item
        app.delete_selected_session().await;
        assert!(app.sessions.sessions.is_empty());
        assert_eq!(app.sessions.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_load_existing_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session("12345678", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        let session_dir = dir.path().join("12345678");
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir(&session_dir).expect("failed to create session dir");
        std::fs::create_dir(&data_dir).expect("failed to create data dir");
        db.update_session_prompt("12345678", "Existing")
            .await
            .expect("failed to update prompt");
        db.append_session_output("12345678", "Output")
            .await
            .expect("failed to update output");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(app.sessions.sessions[0].id, "12345678");
        assert_eq!(app.sessions.sessions[0].prompt, "Existing");
        let output = app.sessions.sessions[0].output.clone();
        assert_eq!(output, "Output");
        assert_eq!(app.sessions.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_create_session_uses_default_model_setting_and_most_recent_permission_mode() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project(&dir.path().to_string_lossy(), Some("main"))
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha0001",
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert alpha0001");
        db.insert_session(
            "beta00002",
            AgentModel::ClaudeHaiku4520251001.as_str(),
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta00002");
        db.update_session_permission_mode("beta00002", PermissionMode::Autonomous.label())
            .await
            .expect("failed to update beta00002 permission mode");
        db.upsert_setting(
            SettingName::DefaultModel.as_str(),
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to upsert default model setting");
        sqlx::query(
            r"
    UPDATE session
    SET updated_at = ?
    WHERE id = ?
    ",
        )
        .bind(1_i64)
        .bind("alpha0001")
        .execute(db.pool())
        .await
        .expect("failed to update alpha0001 timestamp");
        sqlx::query(
            r"
    UPDATE session
    SET updated_at = ?
    WHERE id = ?
    ",
        )
        .bind(2_i64)
        .bind("beta00002")
        .execute(db.pool())
        .await
        .expect("failed to update beta00002 timestamp");
        for session_id in ["alpha0001", "beta00002"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create session data dir");
        }
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let created_session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Assert
        let created_session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == created_session_id)
            .expect("missing created session");
        assert_eq!(created_session.model, AgentModel::ClaudeHaiku4520251001);
        assert_eq!(created_session.permission_mode, PermissionMode::Autonomous);
    }

    #[tokio::test]
    async fn test_load_existing_sessions_ordered_by_updated_at_desc() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session("alpha000", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert alpha000");
        db.insert_session(
            "beta0000",
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta0000");

        sqlx::query(
            r"
    UPDATE session
    SET updated_at = ?
    WHERE id = ?
    ",
        )
        .bind(1_i64)
        .bind("alpha000")
        .execute(db.pool())
        .await
        .expect("failed to update alpha000 timestamp");
        sqlx::query(
            r"
    UPDATE session
    SET updated_at = ?
    WHERE id = ?
    ",
        )
        .bind(2_i64)
        .bind("beta0000")
        .execute(db.pool())
        .await
        .expect("failed to update beta0000 timestamp");

        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert
        let session_names: Vec<&str> = app
            .sessions
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        assert_eq!(session_names, vec!["beta0000", "alpha000"]);
    }

    #[tokio::test]
    async fn test_load_sessions_aggregates_daily_activity() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session("alpha000", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert alpha000");
        db.insert_session("beta0000", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert beta0000");
        db.insert_session("gamma000", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert gamma000");
        let seconds_per_day = 86_400_i64;
        let day_key_one = 10_i64;
        let day_key_two = 11_i64;

        sqlx::query("UPDATE session SET created_at = ? WHERE id = ?")
            .bind(day_key_one * seconds_per_day + 10)
            .bind("alpha000")
            .execute(db.pool())
            .await
            .expect("failed to update alpha000 created_at");
        sqlx::query("UPDATE session SET created_at = ? WHERE id = ?")
            .bind(day_key_one * seconds_per_day + 600)
            .bind("beta0000")
            .execute(db.pool())
            .await
            .expect("failed to update beta0000 created_at");
        sqlx::query("UPDATE session SET created_at = ? WHERE id = ?")
            .bind(day_key_two * seconds_per_day + 50)
            .bind("gamma000")
            .execute(db.pool())
            .await
            .expect("failed to update gamma000 created_at");
        let projects: Vec<Project> = Vec::new();
        let mut handles: HashMap<String, SessionHandles> = HashMap::new();

        // Act
        let (sessions, stats_activity) = SessionManager::load_sessions(
            dir.path(),
            &db,
            &projects,
            &mut handles,
            Arc::new(git::RealGitClient),
        )
        .await;

        // Assert
        assert_eq!(sessions.len(), 3);
        assert_eq!(
            stats_activity,
            vec![
                DailyActivity {
                    day_key: day_key_one,
                    session_count: 2,
                },
                DailyActivity {
                    day_key: day_key_two,
                    session_count: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_refresh_sessions_if_needed_reloads_and_preserves_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha000",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
        db.insert_session("beta0000", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert beta0000");
        sqlx::query(
            r"
    UPDATE session
    SET updated_at = 1
    WHERE id = 'alpha000'
    ",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha000 timestamp");
        sqlx::query(
            r"
    UPDATE session
    SET updated_at = 2
    WHERE id = 'beta0000'
    ",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta0000 timestamp");
        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;
        app.sessions.table_state.select(Some(1));

        // Act
        app.services
            .db()
            .update_session_status("alpha000", "Done")
            .await
            .expect("failed to update session status");
        app.refresh_sessions_now().await;

        // Assert
        assert_eq!(app.sessions.sessions[0].id, "alpha000");
        let selected_index = app
            .sessions
            .table_state
            .selected()
            .expect("missing selection");
        assert_eq!(app.sessions.sessions[selected_index].id, "alpha000");
    }

    #[tokio::test]
    async fn test_refresh_sessions_if_needed_remaps_view_mode_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha000",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
        db.insert_session("beta0000", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert beta0000");
        sqlx::query(
            r"
    UPDATE session
    SET updated_at = 1
    WHERE id = 'alpha000'
    ",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha000 timestamp");
        sqlx::query(
            r"
    UPDATE session
    SET updated_at = 2
    WHERE id = 'beta0000'
    ",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta0000 timestamp");
        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;
        let selected_session_id = app.sessions.sessions[1].id.clone();
        app.mode = AppMode::View {
            session_id: selected_session_id.clone(),
            scroll_offset: None,
        };

        // Act
        app.services
            .db()
            .update_session_status("alpha000", "Done")
            .await
            .expect("failed to update session status");
        app.refresh_sessions_now().await;

        // Assert
        assert_eq!(app.sessions.sessions[0].id, "alpha000");
        assert!(matches!(app.mode, AppMode::View { .. }));
        if let AppMode::View { session_id, .. } = app.mode {
            assert_eq!(session_id, selected_session_id);
        }
    }

    #[tokio::test]
    async fn test_load_sessions_invalid_path() {
        // Arrange
        let path = PathBuf::from("/invalid/path/that/does/not/exist");

        // Act
        let app = new_test_app(path).await;

        // Assert
        assert!(app.sessions.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_load_done_session_without_folder_kept() {
        // Arrange — DB has a terminal row but no matching folder on disk
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "missing01",
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert — terminal session is kept even after folder cleanup
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(app.sessions.sessions[0].id, "missing01");
        assert_eq!(app.sessions.sessions[0].status, Status::Done);
    }

    #[tokio::test]
    async fn test_load_in_progress_session_without_folder_skipped() {
        // Arrange — DB has a non-terminal row but no matching folder on disk
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "missing02",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert — non-terminal session is skipped because folder doesn't exist
        assert!(app.sessions.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_load_sessions_persists_size_for_non_terminal_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing created session");
        let session_folder = app.sessions.sessions[session_index].folder.clone();
        let changed_lines = "line\n".repeat(20);
        std::fs::write(session_folder.join("size-test.txt"), changed_lines)
            .expect("failed to write test file");

        // Act
        let (reloaded_sessions, _) = SessionManager::load_sessions(
            app.services.base_path(),
            app.services.db(),
            &app.projects,
            &mut app.sessions.handles,
            Arc::new(git::RealGitClient),
        )
        .await;
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");

        // Assert
        let reloaded_session = reloaded_sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        let db_session = db_sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing persisted session");
        assert_eq!(reloaded_session.size, SessionSize::S);
        assert_eq!(db_session.size, "S");
    }

    #[tokio::test]
    async fn test_load_sessions_uses_persisted_size_for_done_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.services
            .db()
            .update_session_size(&session_id, "L")
            .await
            .expect("failed to update size");
        app.services
            .db()
            .update_session_status(&session_id, "Done")
            .await
            .expect("failed to update status");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing created session");
        let session_folder = app.sessions.sessions[session_index].folder.clone();
        let changed_lines = "line\n".repeat(700);
        std::fs::write(session_folder.join("done-size-test.txt"), changed_lines)
            .expect("failed to write test file");

        // Act
        let (reloaded_sessions, _) = SessionManager::load_sessions(
            app.services.base_path(),
            app.services.db(),
            &app.projects,
            &mut app.sessions.handles,
            Arc::new(git::RealGitClient),
        )
        .await;
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");

        // Assert
        let reloaded_session = reloaded_sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        let db_session = db_sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing persisted session");
        assert_eq!(reloaded_session.status, Status::Done);
        assert_eq!(reloaded_session.size, SessionSize::L);
        assert_eq!(db_session.size, "L");
    }

    #[tokio::test]
    async fn test_spawn_integration() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut mock = MockAgentBackend::new();
        mock.expect_build_start_command()
            .returning(|folder, prompt, _, _, _| {
                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("--model")
                    .arg("gemini-3-flash-preview")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act — create and start session (start command)
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                "SpawnInit",
                &mock,
                AgentModel::Gemini3FlashPreview,
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Assert
        {
            app.sessions.sync_from_handles();
            let session = &app.sessions.sessions[0];
            let output = session.output.clone();
            assert!(output.contains("--prompt"));
            assert!(output.contains("SpawnInit"));
            assert!(!output.contains("--resume"));
            assert_eq!(session.status, Status::Review);
        }

        // Act — reply (resume command)
        let mut resume_mock = MockAgentBackend::new();
        resume_mock.expect_build_resume_command().returning(
            |folder, prompt, _, _, _, session_output| {
                assert!(session_output.is_none());

                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("--model")
                    .arg("gemini-3-flash-preview")
                    .arg("--resume")
                    .arg("latest")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            },
        );
        let session_id = app.sessions.sessions[0].id.clone();
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                "SpawnReply",
                &resume_mock,
                AgentModel::Gemini3FlashPreview,
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Assert
        {
            app.sessions.sync_from_handles();
            let session = &app.sessions.sessions[0];
            let output = session.output.clone();
            assert!(output.contains("SpawnReply"));
            assert!(output.contains("--resume"));
            assert!(output.contains("latest"));
            assert_eq!(session.status, Status::Review);
        }
    }

    #[tokio::test]
    async fn test_reply_with_backend_replays_history_once_after_model_switch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await;

        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let initial_output = " › Initial prompt\n\nmock-start\n".to_string();
        if let Some(session) = app
            .sessions
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.output.clone_from(&initial_output);
            session.prompt = "Initial prompt".to_string();
            session.status = Status::Review;
        }
        if let Some(handles) = app.sessions.handles.get(&session_id) {
            if let Ok(mut output) = handles.output.lock() {
                output.clone_from(&initial_output);
            }
            if let Ok(mut status) = handles.status.lock() {
                *status = Status::Review;
            }
        }

        app.set_session_model(&session_id, AgentModel::Gpt53Codex)
            .await
            .expect("failed to switch model");

        // Act
        let mut first_resume_mock = MockAgentBackend::new();
        first_resume_mock.expect_build_resume_command().returning(
            |folder, prompt, _, _, _, session_output| {
                let session_output = session_output.expect("expected session output");
                assert!(session_output.contains("Initial prompt"));
                assert!(session_output.contains("mock-start"));

                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("history-replayed")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            },
        );
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                "Switch reply",
                &first_resume_mock,
                AgentModel::Gpt53Codex,
            )
            .await;
        set_session_status_for_test(&mut app, &session_id, Status::Review);

        // Assert
        let mut second_resume_mock = MockAgentBackend::new();
        second_resume_mock.expect_build_resume_command().returning(
            |folder, prompt, _, _, _, session_output| {
                assert!(session_output.is_none());

                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("history-not-replayed")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            },
        );
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                "Second reply",
                &second_resume_mock,
                AgentModel::Gpt53Codex,
            )
            .await;
    }

    #[tokio::test]
    async fn test_spawn_session_task_auto_commits_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await;
        let repo_root = dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Some(repo_root) })
            });
        mock_git_client
            .expect_create_worktree()
            .times(1)
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_diff()
            .times(1..)
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        let base_path = app.services.base_path().to_path_buf();
        let db = app.services.db().clone();
        let event_sender = app.services.event_sender();
        app.services =
            crate::app::AppServices::new(base_path, db, event_sender, Arc::new(mock_git_client));

        // Create a session that writes a file so commit_all has something to commit
        let mut mock = MockAgentBackend::new();
        mock.expect_build_start_command()
            .returning(|folder, _, _, _, _| {
                let mut cmd = Command::new("bash");
                cmd.arg("-c")
                    .arg("echo auto-content > auto-committed.txt")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                "AutoCommit",
                &mock,
                AgentModel::Gemini3FlashPreview,
            )
            .await;

        // Act — wait for agent to finish and auto-commit
        wait_for_status(&mut app, &session_id, Status::Review).await;

        // Assert — output should include commit completion details
        let session = &app.sessions.sessions[0];
        let output = session.output.clone();
        assert!(
            output.contains("[Commit] committed with hash") || output.contains("[Commit Error]"),
            "expected commit completion output, got: {output}"
        );
    }

    #[tokio::test]
    async fn test_commit_changes_amends_existing_session_commit() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.create_session()
            .await
            .expect("failed to create session worktree");
        let session_folder = app.sessions.sessions[0].folder.clone();

        // Act
        std::fs::write(session_folder.join("session-amend.txt"), "first change")
            .expect("failed to write first change");
        let first_hash = SessionManager::commit_changes(&session_folder, false)
            .await
            .expect("failed to create first session commit");

        let first_count_output = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&session_folder)
            .output()
            .expect("failed to read first commit count");
        assert!(
            first_count_output.status.success(),
            "failed to read first commit count: {}",
            String::from_utf8_lossy(&first_count_output.stderr)
        );
        let first_count = String::from_utf8_lossy(&first_count_output.stdout)
            .trim()
            .parse::<usize>()
            .expect("failed to parse first commit count");

        std::fs::write(session_folder.join("session-amend.txt"), "second change")
            .expect("failed to write second change");
        let second_hash = SessionManager::commit_changes(&session_folder, false)
            .await
            .expect("failed to amend session commit");

        let second_count_output = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&session_folder)
            .output()
            .expect("failed to read second commit count");
        assert!(
            second_count_output.status.success(),
            "failed to read second commit count: {}",
            String::from_utf8_lossy(&second_count_output.stderr)
        );
        let second_count = String::from_utf8_lossy(&second_count_output.stdout)
            .trim()
            .parse::<usize>()
            .expect("failed to parse second commit count");

        let head_message_output = Command::new("git")
            .args(["log", "-1", "--pretty=%B"])
            .current_dir(&session_folder)
            .output()
            .expect("failed to read session commit message");
        assert!(
            head_message_output.status.success(),
            "failed to read session commit message: {}",
            String::from_utf8_lossy(&head_message_output.stderr)
        );
        let head_message = String::from_utf8_lossy(&head_message_output.stdout)
            .trim()
            .to_string();

        // Assert
        assert_ne!(
            first_hash, second_hash,
            "amending should rewrite the session commit hash"
        );
        assert_eq!(
            first_count, second_count,
            "session worktree should keep one evolving session commit"
        );
        assert_eq!(head_message, COMMIT_MESSAGE);
    }

    #[tokio::test]
    async fn test_spawn_session_task_skips_commit_when_nothing_to_commit() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await;

        // Agent that produces no file changes
        let mut mock = MockAgentBackend::new();
        mock.expect_build_start_command()
            .returning(|folder, _, _, _, _| {
                let mut cmd = Command::new("echo");
                cmd.arg("no-changes")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                "NoChanges",
                &mock,
                AgentModel::Gemini3FlashPreview,
            )
            .await;

        // Act — wait for agent to finish
        wait_for_status(&mut app, &session_id, Status::Review).await;

        // Assert — no commit output (nothing to commit is silently ignored)
        let session = &app.sessions.sessions[0];
        let output = session.output.clone();
        assert!(
            !output.contains("[Commit]"),
            "should not contain commit output when nothing to commit"
        );
        assert!(
            !output.contains("[Commit Error]"),
            "should not contain commit error when nothing to commit"
        );
    }

    #[tokio::test]
    async fn test_capture_raw_output() {
        // Arrange
        let buffer = Arc::new(Mutex::new(String::new()));
        let source = "Line 1\nLine 2".as_bytes();

        // Act
        crate::app::task::TaskService::capture_raw_output(source, &buffer, None).await;

        // Assert
        let out = buffer.lock().expect("failed to lock buffer").clone();
        assert!(out.contains("Line 1"));
        assert!(out.contains("Line 2"));
    }

    #[tokio::test]
    async fn test_next_tab() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert_eq!(app.tabs.current(), Tab::Sessions);
        app.tabs.next();
        assert_eq!(app.tabs.current(), Tab::Stats);
        app.tabs.next();
        assert_eq!(app.tabs.current(), Tab::Settings);
        app.tabs.next();
        assert_eq!(app.tabs.current(), Tab::Sessions);
    }

    #[tokio::test]
    async fn test_create_session_without_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.create_session().await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Git branch is required")
        );
        assert!(app.sessions.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_create_session_with_git_no_actual_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let result = app.create_session().await;

        // Assert - should fail because git repo doesn't actually exist
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("git repository root")
        );
    }

    #[tokio::test]
    async fn test_create_session_cleans_up_on_error() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let result = app.create_session().await;

        // Assert - session should not be created
        assert!(result.is_err());
        assert_eq!(app.sessions.sessions.len(), 0);

        // Verify no session folder was left behind
        let entries = std::fs::read_dir(dir.path())
            .expect("failed to read dir")
            .count();
        assert_eq!(entries, 0, "Session folder should be cleaned up on error");
    }

    #[tokio::test]
    async fn test_delete_session_without_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        app.delete_selected_session().await;

        // Assert
        assert_eq!(app.sessions.sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_merge_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.merge_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_merge_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.merge_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_merge_session_removes_worktree_and_branch_after_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create merge session");
        set_session_status_for_test(&mut app, &session_id, Status::Review);
        let session_folder = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing created session")
            .folder
            .clone();
        let mock_git =
            create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
        app.sessions.git_client = Arc::new(mock_git);

        // Act
        let result = app.merge_session(&session_id).await;

        // Assert
        assert!(result.is_ok(), "merge should enqueue successfully");
        wait_for_status_with_retries(&mut app, &session_id, Status::Done, 200).await;

        app.sessions.sync_from_handles();
        let merged_session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing merged session");
        assert!(!merged_session.output.contains("[Merge Error]"));
        assert!(!session_folder.exists(), "worktree should be removed");
    }

    #[tokio::test]
    async fn test_merge_session_marks_done_when_changes_are_already_in_base() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create merge session");
        set_session_status_for_test(&mut app, &session_id, Status::Review);
        let mock_git =
            create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
        app.sessions.git_client = Arc::new(mock_git);

        // Act
        let result = app.merge_session(&session_id).await;

        // Assert
        assert!(result.is_ok(), "merge should enqueue successfully");
        wait_for_status_with_retries(&mut app, &session_id, Status::Done, 200).await;

        app.sessions.sync_from_handles();
        let session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing session after merge");
        assert!(!session.output.contains("[Merge Error]"));
    }

    #[tokio::test]
    async fn test_merge_session_queue_processes_sessions_in_fifo_order() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let first_session_id = app
            .create_session()
            .await
            .expect("failed to create first queue session");
        let second_session_id = app
            .create_session()
            .await
            .expect("failed to create second queue session");
        set_session_status_for_test(&mut app, &first_session_id, Status::Review);
        set_session_status_for_test(&mut app, &second_session_id, Status::Review);
        let mock_git =
            create_mock_git_client_for_successful_noop_merges(2, dir.path().to_path_buf());
        app.sessions.git_client = Arc::new(mock_git);

        // Act
        let first_merge_result = app.merge_session(&first_session_id).await;
        let second_merge_result = app.merge_session(&second_session_id).await;

        // Assert
        assert!(
            first_merge_result.is_ok(),
            "first merge request should succeed: {:?}",
            first_merge_result.err()
        );
        assert!(
            second_merge_result.is_ok(),
            "second merge request should enqueue: {:?}",
            second_merge_result.err()
        );

        wait_for_first_merge_to_complete_before_second_starts(
            &mut app,
            &first_session_id,
            &second_session_id,
        )
        .await;
        wait_for_second_merge_to_start(&mut app, &second_session_id).await;

        assert!(
            session_status_or_done(&app, &first_session_id) == Status::Done,
            "first merge should be complete before second starts"
        );

        wait_for_all_sessions_done(&mut app, &first_session_id, &second_session_id).await;

        app.sessions.sync_from_handles();
        let first_status = session_status_or_done(&app, &first_session_id);
        let second_status = session_status_or_done(&app, &second_session_id);
        assert_eq!(first_status, Status::Done);
        assert_eq!(second_status, Status::Done);
    }

    #[tokio::test]
    async fn test_rebase_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.rebase_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_rebase_session_requires_review_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Act
        let result = app.rebase_session(&session_id).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("must be in review")
        );
    }

    #[tokio::test]
    async fn test_rebase_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app_with_git(dir.path()).await;

        // Act
        let result = app.rebase_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_rebase_session_updates_session_worktree_to_base_head() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_folder = app.sessions.sessions[0].folder.clone();
        app.sessions.sessions[0].status = Status::Review;
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut session_status) = handles.status.lock()
        {
            *session_status = Status::Review;
        }
        std::fs::write(dir.path().join("main-only.txt"), "main change")
            .expect("failed to write main change");
        Command::new("git")
            .args(["add", "main-only.txt"])
            .current_dir(dir.path())
            .output()
            .expect("failed to stage main change");
        Command::new("git")
            .args(["commit", "-m", "main update"])
            .current_dir(dir.path())
            .output()
            .expect("failed to commit main change");

        // Act
        let result = app.rebase_session(&session_id).await;

        // Assert
        assert!(result.is_ok(), "rebase should succeed: {:?}", result.err());
        wait_for_output_contains(&mut app, &session_id, "[Rebase] Successfully rebased", 200).await;

        let base_head_output = Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(dir.path())
            .output()
            .expect("failed to resolve base head");
        let base_head = String::from_utf8_lossy(&base_head_output.stdout)
            .trim()
            .to_string();

        let session_head_output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(session_folder)
            .output()
            .expect("failed to resolve session head");
        let session_head = String::from_utf8_lossy(&session_head_output.stdout)
            .trim()
            .to_string();

        assert_eq!(session_head, base_head);
    }

    #[tokio::test]
    async fn test_rebase_session_auto_commits_uncommitted_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_folder = app.sessions.sessions[0].folder.clone();
        app.sessions.sessions[0].status = Status::Review;
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut session_status) = handles.status.lock()
        {
            *session_status = Status::Review;
        }

        // Create an uncommitted change in the session worktree
        std::fs::write(session_folder.join("dirty.txt"), "uncommitted content")
            .expect("failed to write dirty file");

        // Act
        let result = app.rebase_session(&session_id).await;

        // Assert
        assert!(result.is_ok(), "rebase should succeed: {:?}", result.err());
        wait_for_output_contains(&mut app, &session_id, "[Rebase] Successfully rebased", 200).await;

        // Verify worktree is clean
        let status_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&session_folder)
            .output()
            .expect("failed to check status");
        assert!(
            status_output.stdout.is_empty(),
            "worktree should be clean after auto-commit"
        );
        app.refresh_sessions_now().await;
    }

    #[tokio::test]
    async fn test_sync_main_uses_active_project_branch_from_context() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.projects.replace_context(
            app.active_project_id(),
            Some("develop".to_string()),
            dir.path().to_path_buf(),
        );
        std::fs::write(dir.path().join("README.md"), "dirty develop")
            .expect("failed to write file");

        // Act
        let result = SessionManager::sync_main_for_project(
            app.projects.git_branch().map(str::to_string),
            app.projects.working_dir().to_path_buf(),
            app.services.git_client(),
            PermissionMode::AutoEdit,
            AgentModel::Gemini3FlashPreview,
        )
        .await;

        // Assert
        assert_eq!(
            result,
            Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: "develop".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn test_sync_main_requires_clean_selected_project_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app_with_git(dir.path()).await;
        std::fs::write(dir.path().join("README.md"), "dirty main").expect("failed to write file");

        // Act
        let result = SessionManager::sync_main_for_project(
            app.projects.git_branch().map(str::to_string),
            app.projects.working_dir().to_path_buf(),
            app.services.git_client(),
            PermissionMode::AutoEdit,
            AgentModel::Gemini3FlashPreview,
        )
        .await;

        // Assert
        assert_eq!(
            result,
            Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: "main".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn test_sync_main_returns_error_without_upstream_remote() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app_with_git(dir.path()).await;

        // Act
        let result = SessionManager::sync_main_for_project(
            app.projects.git_branch().map(str::to_string),
            app.projects.working_dir().to_path_buf(),
            app.services.git_client(),
            PermissionMode::AutoEdit,
            AgentModel::Gemini3FlashPreview,
        )
        .await;

        // Assert
        assert!(matches!(result, Err(SyncSessionStartError::Other(_))));
    }

    #[tokio::test]
    /// Verifies `sync_main_for_project` pushes local commits to `origin`.
    async fn test_sync_main_pushes_local_commits_to_remote() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        Command::new("git")
            .args(["init", "--bare"])
            .arg(remote_dir.path())
            .output()
            .expect("failed to init bare remote");
        let remote_url = remote_dir
            .path()
            .to_str()
            .expect("remote path invalid")
            .to_string();
        Command::new("git")
            .args(["remote", "add", "origin", &remote_url])
            .current_dir(dir.path())
            .output()
            .expect("failed to add remote");
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(dir.path())
            .output()
            .expect("failed to push initial commit");

        std::fs::write(dir.path().join("README.md"), "local change")
            .expect("failed to write local change");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "local work"])
            .current_dir(dir.path())
            .output()
            .expect("git commit failed");

        let local_head_output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("git rev-parse failed");
        let local_head = String::from_utf8_lossy(&local_head_output.stdout)
            .trim()
            .to_string();
        let remote_head_before = std::fs::read_to_string(remote_dir.path().join("refs/heads/main"))
            .expect("failed to read remote head");
        assert_ne!(remote_head_before.trim(), local_head);

        // Act
        let result = SessionManager::sync_main_for_project(
            Some("main".to_string()),
            dir.path().to_path_buf(),
            Arc::new(git::RealGitClient),
            PermissionMode::AutoEdit,
            AgentModel::Gemini3FlashPreview,
        )
        .await;

        // Assert
        assert!(result.is_ok());
        let remote_head_after = std::fs::read_to_string(remote_dir.path().join("refs/heads/main"))
            .expect("failed to read remote head after sync");
        assert_eq!(remote_head_after.trim(), local_head);
    }

    #[tokio::test]
    async fn test_cancel_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let repo_root = dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Some(repo_root) })
            });
        mock_git_client
            .expect_create_worktree()
            .times(1)
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_diff()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        let base_path = app.services.base_path().to_path_buf();
        let db = app.services.db().clone();
        let event_sender = app.services.event_sender();
        app.services =
            crate::app::AppServices::new(base_path, db, event_sender, Arc::new(mock_git_client));
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        set_session_status_for_test(&mut app, &session_id, Status::Review);

        // Act
        app.sessions
            .cancel_session(&app.services, &session_id)
            .await
            .expect("failed to cancel session");

        // Assert
        app.sessions.sync_from_handles();
        let session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing session");
        assert_eq!(session.status, Status::Canceled);
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        let db_session = db_sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing persisted session");
        assert_eq!(db_session.status, "Canceled");
    }

    #[tokio::test]
    async fn test_cancel_session_requires_review_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        // Status is New

        // Act
        let result = app
            .sessions
            .cancel_session(&app.services, &session_id)
            .await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("must be in review")
        );
    }

    #[tokio::test]
    async fn test_cancel_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.sessions.cancel_session(&app.services, "missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_cleanup_merged_session_worktree_without_repo_hint() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let worktree_folder = dir.path().join("merged-worktree");
        let branch_name = "agentty/cleanup123";
        git::create_worktree(
            dir.path().to_path_buf(),
            worktree_folder.clone(),
            branch_name.to_string(),
            "main".to_string(),
        )
        .await
        .expect("failed to create worktree");
        assert!(
            worktree_folder.exists(),
            "worktree should exist before cleanup"
        );

        // Act
        let result = SessionManager::cleanup_merged_session_worktree(
            worktree_folder.clone(),
            Arc::new(git::RealGitClient),
            branch_name.to_string(),
            None,
        )
        .await;

        // Assert
        assert!(result.is_ok(), "cleanup should succeed: {:?}", result.err());
        assert!(
            !worktree_folder.exists(),
            "worktree should be removed after cleanup"
        );

        let branch_output = Command::new("git")
            .args(["branch", "--list", branch_name])
            .current_dir(dir.path())
            .output()
            .expect("failed to list branches");
        let branches = String::from_utf8_lossy(&branch_output.stdout);
        assert!(
            branches.trim().is_empty(),
            "branch should be removed after cleanup"
        );
    }

    #[tokio::test]
    async fn test_active_project_id_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert!(app.active_project_id() > 0);
    }

    #[tokio::test]
    async fn test_projects_auto_registered() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert — cwd auto-registered as a project
        assert!(
            app.projects
                .iter()
                .any(|project| project.path == Path::new("/tmp/test"))
        );
    }

    #[tokio::test]
    async fn test_switch_project() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        let other_id = app
            .services
            .db()
            .upsert_project("/tmp/other", Some("develop"))
            .await
            .expect("failed to upsert");

        // Act
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert
        assert_eq!(app.active_project_id(), other_id);
        assert_eq!(app.working_dir(), Path::new("/tmp/other"));
        assert_eq!(app.git_branch(), Some("develop"));
        assert!(app.sessions.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_switch_project_not_found() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.switch_project(999).await;

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("expected missing project error");
        assert!(error.contains("Project not found"));
    }

    #[tokio::test]
    async fn test_switch_project_shows_all_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Session A").await;
        assert_eq!(app.sessions.sessions.len(), 1);

        let other_id = app
            .services
            .db()
            .upsert_project("/tmp/other", None)
            .await
            .expect("failed to upsert");

        // Act — switch to other project
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert — all sessions still visible after switching projects
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(app.active_project_id(), other_id);
    }

    #[tokio::test]
    async fn test_create_session_scoped_to_project() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let project_id = app.active_project_id();

        // Act
        app.create_session()
            .await
            .expect("failed to create session");

        // Assert — session belongs to the active project
        let sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_discover_sibling_projects() {
        // Arrange — create a parent dir with two git repo subdirectories
        let parent = tempdir().expect("failed to create temp dir");
        let repo_a = parent.path().join("repo_a");
        let repo_b = parent.path().join("repo_b");
        let not_repo = parent.path().join("plain_dir");
        std::fs::create_dir(&repo_a).expect("failed to create repo_a");
        std::fs::create_dir(&repo_b).expect("failed to create repo_b");
        std::fs::create_dir(&not_repo).expect("failed to create plain_dir");
        setup_test_git_repo(&repo_a);
        setup_test_git_repo(&repo_b);

        // Act — launch app from repo_a
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            parent.path().to_path_buf(),
            repo_a.clone(),
            Some("main".to_string()),
            db,
        )
        .await;

        // Assert — repo_a (cwd) and repo_b (sibling) are discovered, plain_dir is not
        assert_eq!(app.projects.len(), 2);
        let paths: Vec<&Path> = app.projects.iter().map(|p| p.path.as_path()).collect();
        assert!(paths.contains(&repo_a.as_path()));
        assert!(paths.contains(&repo_b.as_path()));
    }

    #[test]
    fn test_parse_merge_commit_message_response_with_json() {
        // Arrange
        let content = r#"{"title":"Title","description":"- Detail"}"#;

        // Act
        let parsed = SessionManager::parse_merge_commit_message_response(content);

        // Assert
        assert!(parsed.is_some());
        assert_eq!(
            parsed.as_ref().map(|value| value.title.as_str()),
            Some("Title")
        );
        assert_eq!(
            parsed.as_ref().map(|value| value.description.as_str()),
            Some("- Detail")
        );
    }

    #[test]
    fn test_parse_merge_commit_message_response_with_wrapped_json() {
        // Arrange
        let content = "response:\n{\"title\":\"Title\",\"description\":\"- Detail\"}\n";

        // Act
        let parsed = SessionManager::parse_merge_commit_message_response(content);

        // Assert
        assert!(parsed.is_some());
        assert_eq!(
            parsed.as_ref().map(|value| value.title.as_str()),
            Some("Title")
        );
    }

    #[test]
    fn test_merge_commit_message_prompt_uses_git_commit_default_format() {
        // Arrange
        let diff = "diff --git a/file.txt b/file.txt\n+change";

        // Act
        let prompt = SessionManager::merge_commit_message_prompt(diff);

        // Assert
        assert!(prompt.contains("`title` must be one line, concise, and in present simple tense."));
        assert!(prompt.contains("Do not use Conventional Commit prefixes like `feat:` or `fix:`."));
        assert!(prompt.contains("use `-` bullets when listing multiple points."));
        assert!(prompt.contains(
            "Include `Co-Authored-By: [Agentty](https://github.com/opencloudtool/agentty)` at the \
             end of the final message."
        ));
        assert!(
            prompt
                .contains("Use repository default commit format unless explicit user instructions")
        );
    }

    #[test]
    fn test_fallback_merge_commit_message() {
        // Arrange
        let source_branch = "agentty/12345678";
        let target_branch = "main";

        // Act
        let message = SessionManager::fallback_merge_commit_message(source_branch, target_branch);

        // Assert
        assert_eq!(
            message,
            "Apply session updates\n\n- Squash merge `agentty/12345678` into `main`."
        );
    }

    // --- session_folder / session_branch ---

    #[test]
    fn test_session_folder_uses_first_8_chars() {
        // Arrange
        let base = Path::new("/home/user/.agentty/wt");
        let session_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        // Act
        let folder = session_folder(base, session_id);

        // Assert
        assert_eq!(folder, PathBuf::from("/home/user/.agentty/wt/a1b2c3d4"));
    }

    #[test]
    fn test_session_branch_uses_first_8_chars() {
        // Arrange
        let session_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        // Act
        let branch = session_branch(session_id);

        // Assert
        assert_eq!(branch, "agentty/a1b2c3d4");
    }

    // --- clear_session_history ---

    #[tokio::test]
    async fn test_clear_session_history() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        set_session_status_for_test(&mut app, &session_id, Status::Review);
        app.sessions.sessions[0].output = " › Fix the bug\n\nmock-start\n".to_string();
        app.sessions.sessions[0].prompt = "Fix the bug".to_string();
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut output) = handles.output.lock()
        {
            *output = " › Fix the bug\n\nmock-start\n".to_string();
        }
        let session_id = app.sessions.sessions[0].id.clone();
        let stats = SessionStats {
            input_tokens: 100,
            output_tokens: 50,
        };
        app.services
            .db()
            .update_session_stats(&session_id, &stats)
            .await
            .expect("failed to update stats");
        app.services
            .db()
            .update_session_summary(&session_id, "Summary to reset")
            .await
            .expect("failed to update summary");
        app.sessions.sessions[0].stats = stats;
        app.sessions.sessions[0].summary = Some("Summary to reset".to_string());

        // Act
        let result = app.clear_session_history(&session_id).await;

        // Assert
        assert!(result.is_ok());
        let session = &app.sessions.sessions[0];
        let output = session.output.clone();
        assert!(output.is_empty());
        assert!(session.prompt.is_empty());
        assert_eq!(session.summary, None);
        assert_eq!(session.title, None);
        assert_eq!(session.status, Status::New);
        assert_eq!(session.stats.input_tokens, 100);
        assert_eq!(session.stats.output_tokens, 50);

        // Verify DB was updated
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");
        assert_eq!(db_sessions[0].output, "");
        assert_eq!(db_sessions[0].prompt, "");
        assert_eq!(db_sessions[0].summary, None);
        assert_eq!(db_sessions[0].title, None);
        assert_eq!(db_sessions[0].status, "New");
        assert_eq!(db_sessions[0].input_tokens, 100);
        assert_eq!(db_sessions[0].output_tokens, 50);
    }

    #[tokio::test]
    async fn test_clear_session_history_preserves_agent_and_model() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        set_session_status_for_test(&mut app, &session_id, Status::Review);
        let session_id = app.sessions.sessions[0].id.clone();
        let _ = app
            .set_session_model(&session_id, AgentKind::Claude.default_model())
            .await;

        // Act
        app.clear_session_history(&session_id)
            .await
            .expect("failed to clear");

        // Assert
        let session = &app.sessions.sessions[0];
        assert_eq!(session.model, AgentKind::Claude.default_model());
    }

    #[tokio::test]
    async fn test_clear_session_history_preserves_worktree() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        set_session_status_for_test(&mut app, &session_id, Status::Review);
        let session_id = app.sessions.sessions[0].id.clone();
        let folder = app.sessions.sessions[0].folder.clone();
        assert!(folder.exists());

        // Act
        app.clear_session_history(&session_id)
            .await
            .expect("failed to clear");

        // Assert — worktree folder still exists
        assert!(folder.exists());
        assert_eq!(app.sessions.sessions[0].folder, folder);
    }

    #[tokio::test]
    async fn test_clear_session_history_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;

        // Act
        let result = app.clear_session_history("nonexistent").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_clear_session_history_resets_for_fresh_agent_context() {
        // Arrange — a session with a non-empty prompt uses `build_resume_command`.
        // After clearing, prompt is empty so the next reply uses `build_start_command`.
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Initial prompt").await;
        let session_id = app.sessions.sessions[0].id.clone();
        wait_for_status(&mut app, &session_id, Status::Review).await;
        let session_id = app.sessions.sessions[0].id.clone();

        // Act
        app.clear_session_history(&session_id)
            .await
            .expect("failed to clear");

        // Assert — prompt is empty, meaning reply_with_backend will treat the next
        // message as is_first_message=true and use build_start_command (no --resume)
        let session = &app.sessions.sessions[0];
        assert!(session.prompt.is_empty());
        assert_eq!(session.status, Status::New);
    }

    #[tokio::test]
    async fn test_stop_session_errors_when_not_in_progress() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(base_dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        assert_eq!(app.sessions.sessions[0].status, Status::New);

        // Act
        let result = app.stop_session(&session_id).await;

        // Assert
        assert!(result.is_err());
        assert_eq!(
            result.expect_err("expected error"),
            "Session is not in progress"
        );
    }

    #[tokio::test]
    async fn test_stop_session_errors_when_no_pid() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(base_dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Ok(mut status) = app.sessions.handles[&session_id].status.lock() {
            *status = Status::InProgress;
        }
        app.sessions.sessions[0].status = Status::InProgress;

        // Act
        let result = app.stop_session(&session_id).await;

        // Assert
        assert!(result.is_err());
        assert_eq!(
            result.expect_err("expected error"),
            "No running agent process"
        );
    }

    #[tokio::test]
    async fn test_stop_session_errors_when_session_not_found() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app_with_git(base_dir.path()).await;

        // Act
        let result = app.stop_session("nonexistent").await;

        // Assert
        assert!(result.is_err());
        assert_eq!(result.expect_err("expected error"), "Session not found");
    }
}
