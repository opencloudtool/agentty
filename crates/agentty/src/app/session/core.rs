//! Session lifecycle orchestration for creation, refresh, prompt handling,
//! history management, merge, and cleanup.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;

use super::workflow::merge::SessionMergeService;
pub(crate) use super::workflow::merge::{SyncMainOutcome, SyncSessionStartError};
pub(crate) use super::workflow::task::{RunAgentAssistTaskInput, SessionTaskService};
use super::workflow::worker::SessionWorkerService;
use crate::app::{AppServices, SessionState, setting};
use crate::domain::agent::AgentModel;
use crate::domain::session::{DailyActivity, Session};
use crate::infra::git;

/// Render payload tuple returned by [`SessionManager::render_parts`].
type SessionRenderParts<'a> = (&'a [Session], &'a [DailyActivity], &'a mut TableState);

/// Low-frequency fallback interval for metadata-based session refresh.
pub(crate) const SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Defaults used when creating new sessions from the UI.
#[derive(Clone, Copy)]
pub(crate) struct SessionDefaults {
    /// Default model selected for newly created sessions.
    pub(crate) model: AgentModel,
}

/// Provides wall-clock values for session refresh decision logic.
pub(crate) trait Clock: Send + Sync {
    /// Returns the current monotonic instant.
    fn now_instant(&self) -> Instant;

    /// Returns the current wall-clock system time.
    fn now_system_time(&self) -> SystemTime;
}

/// Production clock backed by `std::time`.
pub(crate) struct RealClock;

impl Clock for RealClock {
    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Session domain state and worker orchestration state.
pub struct SessionManager {
    pub(super) default_session_model: AgentModel,
    pub(super) git_client: Arc<dyn git::GitClient>,
    pub(super) merge_service: SessionMergeService,
    pub(super) pending_history_replay: HashSet<String>,
    pub(super) state: SessionState,
    pub(super) stats_activity: Vec<DailyActivity>,
    pub(super) worker_service: SessionWorkerService,
}

impl SessionManager {
    /// Creates a session manager from persisted snapshot state and defaults.
    ///
    /// Review sessions are marked for one-time transcript replay so the next
    /// reply can rehydrate provider context after app restart.
    pub(crate) fn new(
        defaults: SessionDefaults,
        git_client: Arc<dyn git::GitClient>,
        state: SessionState,
        stats_activity: Vec<DailyActivity>,
    ) -> Self {
        let pending_history_replay = Self::startup_history_replay_set(&state.sessions);

        Self {
            default_session_model: defaults.model,
            git_client,
            merge_service: SessionMergeService,
            pending_history_replay,
            state,
            stats_activity,
            worker_service: SessionWorkerService::new(),
        }
    }

    /// Returns the internal merge orchestration service.
    pub(crate) fn merge_service(&self) -> &SessionMergeService {
        &self.merge_service
    }

    /// Returns the configured session git client used by orchestration flows.
    pub(crate) fn git_client(&self) -> Arc<dyn git::GitClient> {
        Arc::clone(&self.git_client)
    }

    /// Returns mutable access to worker orchestration state.
    pub(crate) fn worker_service_mut(&mut self) -> &mut SessionWorkerService {
        &mut self.worker_service
    }

    /// Returns the default smart model used for session-scoped agent
    /// workflows.
    pub(crate) fn default_session_model(&self) -> AgentModel {
        self.default_session_model
    }

    /// Replaces the default smart model used for newly created sessions.
    pub(crate) fn set_default_session_model(&mut self, session_model: AgentModel) {
        self.default_session_model = session_model;
    }

    /// Loads the default smart model persisted for new sessions.
    pub(crate) async fn load_default_session_model(
        services: &AppServices,
        project_id: Option<i64>,
        fallback_model: AgentModel,
    ) -> AgentModel {
        setting::load_default_smart_model_setting(services, project_id, fallback_model).await
    }

    /// Returns session snapshots and stats payloads required for rendering.
    ///
    /// The tuple contains live sessions, activity heatmap data, and list table
    /// state.
    pub(crate) fn render_parts(&mut self) -> SessionRenderParts<'_> {
        (
            &self.state.sessions,
            &self.stats_activity,
            &mut self.state.table_state,
        )
    }

    /// Returns shared immutable access to session render and refresh state.
    pub(crate) fn state(&self) -> &SessionState {
        &self.state
    }

    /// Applies reducer updates after session agent/model changes are
    /// persisted.
    ///
    /// This updates only the target session snapshot. Global default-model
    /// selection is managed by settings persistence and loaded when creating
    /// new sessions.
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
    }

    /// Applies one persisted published-upstream reference to the matching
    /// in-memory session snapshot.
    pub(crate) fn apply_published_upstream_ref(
        &mut self,
        session_id: &str,
        published_upstream_ref: String,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.published_upstream_ref = Some(published_upstream_ref);
        }
    }

    /// Replaces cached session git-status snapshots from the latest
    /// background poll.
    pub(crate) fn replace_session_git_statuses(
        &mut self,
        session_git_statuses: HashMap<String, Option<(u32, u32)>>,
    ) {
        self.state
            .replace_session_git_statuses(session_git_statuses);
    }

    /// Returns cached session git-status snapshots keyed by session id.
    pub(crate) fn session_git_statuses(&self) -> &HashMap<String, Option<(u32, u32)>> {
        &self.state.session_git_statuses
    }
}

/// Backward-compatible state-field access shim while call sites migrate to
/// explicit `state()` APIs.
impl Deref for SessionManager {
    type Target = SessionState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

/// Backward-compatible mutable state access shim for legacy call sites.
impl DerefMut for SessionManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

/// Returns the folder path for a session under the given base directory.
pub(crate) fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    let len = session_id.len().min(8);
    base.join(&session_id[..len])
}

/// Returns the worktree branch name for a session.
pub(crate) fn session_branch(session_id: &str) -> String {
    let len = session_id.len().min(8);
    format!("agentty/{}", &session_id[..len])
}

/// Converts one wall-clock timestamp into Unix seconds.
pub(crate) fn unix_timestamp_from_system_time(system_time: SystemTime) -> i64 {
    system_time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    //! Session module tests.

    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::*;
    use crate::app::{App, SyncSessionStartError, Tab};
    use crate::domain::agent::{AgentKind, AgentModel};
    use crate::domain::session::{
        DailyActivity, SESSION_DATA_DIR, Session, SessionHandles, SessionSize, SessionStats, Status,
    };
    use crate::domain::setting::SettingName;
    use crate::infra::agent::AgentResponse;
    use crate::infra::agent::tests::MockAgentBackend;
    use crate::infra::app_server::{AppServerTurnResponse, MockAppServerClient};
    use crate::infra::channel::{AgentRequestKind, MockAgentChannel, TurnResult};
    use crate::infra::db::Database;
    use crate::infra::fs::{self as fs, FsClient};
    use crate::infra::{app_server, git};
    use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode};
    use crate::ui::util;

    /// Returns a mock app-server client wrapped in `Arc` for test injection.
    fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
        Arc::new(MockAppServerClient::new())
    }

    /// Builds a filesystem mock that delegates operations to local disk.
    fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_create_dir_all()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    tokio::fs::create_dir_all(path)
                        .await
                        .map_err(fs::FsError::from)
                })
            });
        mock_fs_client
            .expect_remove_dir_all()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    tokio::fs::remove_dir_all(path)
                        .await
                        .map_err(fs::FsError::from)
                })
            });
        mock_fs_client
            .expect_read_file()
            .times(0..)
            .returning(|path| {
                Box::pin(async move { tokio::fs::read(path).await.map_err(fs::FsError::from) })
            });
        mock_fs_client
            .expect_remove_file()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    match tokio::fs::remove_file(path).await {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(fs::FsError::from(error)),
                    }
                })
            });
        mock_fs_client
            .expect_is_dir()
            .times(0..)
            .returning(|path| path.is_dir());

        mock_fs_client
    }

    fn create_mock_backend() -> MockAgentBackend {
        let mut mock = MockAgentBackend::new();
        mock.expect_build_command().returning(|request| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg("printf '{\"answer\":\"mock-start\",\"questions\":[],\"summary\":null}'")
                .current_dir(request.folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            Ok(cmd)
        });
        mock
    }

    /// Builds a merge-focused mock git client for no-op merge scenarios.
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
        mock.expect_is_worktree_clean()
            .times(expected_merge_count)
            .returning(|_| Box::pin(async { Ok(true) }));
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
                    let fs_client = create_passthrough_mock_fs_client();
                    let _ = fs_client.remove_dir_all(worktree_path).await;

                    Ok(())
                })
            });
        mock.expect_delete_branch()
            .times(expected_merge_count)
            .returning(|_, _| Box::pin(async { Ok(()) }));

        mock
    }

    /// Builds a permissive mock git client for session tests.
    ///
    /// The mock returns successful defaults and performs lightweight
    /// filesystem side effects for worktree creation/removal.
    fn create_default_mock_git_client(repo_root: PathBuf) -> git::MockGitClient {
        let mut mock = git::MockGitClient::new();

        setup_mock_worktree_expectations(&mut mock, repo_root);
        setup_mock_merge_and_rebase_expectations(&mut mock);
        setup_mock_commit_and_branch_expectations(&mut mock);

        mock
    }

    /// Configures worktree, repo discovery, and remote expectations.
    fn setup_mock_worktree_expectations(mock: &mut git::MockGitClient, repo_root: PathBuf) {
        let find_repo_root = repo_root.clone();

        mock.expect_detect_git_info()
            .times(0..)
            .returning(|_| Box::pin(async { Some("main".to_string()) }));
        mock.expect_current_upstream_reference()
            .times(0..)
            .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
        mock.expect_find_git_repo_root()
            .times(0..)
            .returning(move |_| {
                let repo_root = find_repo_root.clone();
                Box::pin(async move { Some(repo_root) })
            });
        mock.expect_create_worktree()
            .times(0..)
            .returning(|_, worktree_path, _, _| {
                Box::pin(async move {
                    let fs_client = create_passthrough_mock_fs_client();
                    fs_client
                        .create_dir_all(worktree_path.clone())
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock worktree directory: {error}"
                            ))
                        })?;
                    fs_client
                        .create_dir_all(worktree_path.join(SESSION_DATA_DIR))
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock session data directory: {error}"
                            ))
                        })?;

                    Ok(())
                })
            });
        mock.expect_remove_worktree()
            .times(0..)
            .returning(|worktree_path| {
                Box::pin(async move {
                    let fs_client = create_passthrough_mock_fs_client();
                    let _ = fs_client.remove_dir_all(worktree_path).await;

                    Ok(())
                })
            });
        mock.expect_pull_rebase().times(0..).returning(|_| {
            Box::pin(async {
                Err(git::GitError::OutputParse(
                    "No upstream branch configured for pull".to_string(),
                ))
            })
        });
        mock.expect_push_current_branch()
            .times(0..)
            .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
        mock.expect_fetch_remote()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock.expect_branch_tracking_statuses()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
        mock.expect_get_ahead_behind()
            .times(0..)
            .returning(|_| Box::pin(async { Ok((0, 0)) }));
        mock.expect_list_upstream_commit_titles()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
        mock.expect_list_local_commit_titles()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
        mock.expect_repo_url()
            .times(0..)
            .returning(|_| Box::pin(async { Ok("https://example.invalid/repo.git".to_string()) }));
        mock.expect_main_repo_root().times(0..).returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Ok(repo_root) })
        });
    }

    /// Configures merge, rebase, and conflict resolution expectations.
    fn setup_mock_merge_and_rebase_expectations(mock: &mut git::MockGitClient) {
        mock.expect_squash_merge_diff()
            .times(0..)
            .returning(|_, _, _| Box::pin(async { Ok(String::new()) }));
        mock.expect_squash_merge()
            .times(0..)
            .returning(|_, _, _, _| Box::pin(async { Ok(git::SquashMergeOutcome::Committed) }));
        mock.expect_rebase()
            .times(0..)
            .returning(|_, _| Box::pin(async { Ok(()) }));
        mock.expect_rebase_start()
            .times(0..)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock.expect_rebase_continue()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock.expect_abort_rebase()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock.expect_is_rebase_in_progress()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock.expect_has_unmerged_paths()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock.expect_list_staged_conflict_marker_files()
            .times(0..)
            .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
        mock.expect_list_conflicted_files()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    }

    /// Builds a synthetic git-diff payload from a session worktree.
    ///
    /// Production tests rely on file-edit volume to estimate session size.
    /// This helper counts lines in non-metadata files so mocked git clients
    /// can still drive size-related assertions without invoking shell `git`.
    async fn synthetic_diff_from_session_folder(folder: &Path) -> String {
        /// Counts lines across one worktree using the async filesystem boundary
        /// for file reads while keeping directory traversal simple in tests.
        async fn count_lines_recursively(fs_client: &dyn fs::FsClient, root: &Path) -> usize {
            let mut pending_entries = vec![root.to_path_buf()];
            let mut line_count = 0;

            while let Some(entry) = pending_entries.pop() {
                if !entry.is_dir() {
                    if let Ok(content) = fs_client.read_file(entry).await {
                        line_count += String::from_utf8_lossy(&content).lines().count();
                    }

                    continue;
                }

                if entry
                    .file_name()
                    .is_some_and(|name| name == SESSION_DATA_DIR)
                {
                    continue;
                }

                if let Ok(entries) = std::fs::read_dir(entry) {
                    pending_entries.extend(
                        entries
                            .filter_map(Result::ok)
                            .map(|dir_entry| dir_entry.path()),
                    );
                }
            }

            line_count
        }

        let fs_client = create_passthrough_mock_fs_client();
        let line_count = count_lines_recursively(&fs_client, folder).await;

        if line_count == 0 {
            String::new()
        } else {
            "+\n".repeat(line_count)
        }
    }

    /// Configures commit, staging, and branch operation expectations.
    fn setup_mock_commit_and_branch_expectations(mock: &mut git::MockGitClient) {
        mock.expect_commit_all()
            .times(0..)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));
        mock.expect_commit_all_preserving_single_commit()
            .times(0..)
            .returning(|_, _, _, _, _| {
                Box::pin(async {
                    Err(git::GitError::OutputParse(
                        "Nothing to commit: no changes detected".to_string(),
                    ))
                })
            });
        mock.expect_stage_all()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock.expect_head_short_hash()
            .times(0..)
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        mock.expect_delete_branch()
            .times(0..)
            .returning(|_, _| Box::pin(async { Ok(()) }));
        mock.expect_diff().times(0..).returning(|folder, _| {
            Box::pin(async move { Ok(synthetic_diff_from_session_folder(&folder).await) })
        });
        mock.expect_is_worktree_clean()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock.expect_has_commits_since()
            .times(0..)
            .returning(|_, _| Box::pin(async { Ok(false) }));
        mock.expect_head_commit_message()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(None) }));
    }

    /// Replaces app-level git dependencies with the provided mock client.
    fn install_mock_git_client(app: &mut App, mock_git_client: git::MockGitClient) {
        let mock_git_client: Arc<dyn git::GitClient> = Arc::new(mock_git_client);
        let base_path = app.services.base_path().to_path_buf();
        let db = app.services.db().clone();
        let event_sender = app.services.event_sender();
        let app_server_client_override = app.services.app_server_client_override();
        let fs_client = app.services.fs_client();
        let review_request_client = app.services.review_request_client();

        app.services = AppServices::new(
            base_path,
            db,
            event_sender,
            fs_client,
            Arc::clone(&mock_git_client),
            review_request_client,
            app_server_client_override,
        );
        app.sessions.git_client = mock_git_client;
    }

    /// Builds a test app with a caller-provided database, git context, and
    /// app-server boundary.
    async fn new_test_app_with_db_and_app_server(
        path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
        app_server_client: Arc<dyn app_server::AppServerClient>,
    ) -> App {
        let clients =
            crate::app::core::AppClients::new().with_app_server_client_override(app_server_client);
        let mut app = App::new_with_clients(path, working_dir.clone(), git_branch, db, clients)
            .await
            .expect("failed to build app");
        let mock_git_client = create_default_mock_git_client(working_dir);
        install_mock_git_client(&mut app, mock_git_client);

        app
    }

    /// Builds a test app with a caller-provided database and git context.
    async fn new_test_app_with_db(
        path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
    ) -> App {
        new_test_app_with_db_and_app_server(path, working_dir, git_branch, db, mock_app_server())
            .await
    }

    /// Builds a test app rooted at `path` with no branch-specific git context.
    async fn new_test_app(path: PathBuf) -> App {
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        new_test_app_with_db(path, working_dir, None, db).await
    }

    /// Builds a test app rooted at `path` with mock git branch context.
    async fn new_test_app_with_git(path: &Path) -> App {
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        new_test_app_with_git_and_db(path, db).await
    }

    /// Builds a test app rooted at `path` with mock git branch context and a
    /// caller-provided database handle.
    async fn new_test_app_with_git_and_db(path: &Path, db: Database) -> App {
        new_test_app_with_db(
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
            follow_up_tasks: Vec::new(),
            id: id.to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: String::new(),
            prompt: prompt.to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
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
                Arc::new(start_backend),
                AgentModel::ClaudeOpus46,
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
        let app = new_test_app_with_db(
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
        let activity_timestamps = app
            .services
            .db()
            .load_session_activity_timestamps()
            .await
            .expect("failed to load session activity timestamps");
        assert_eq!(db_sessions.len(), 1);
        assert_eq!(db_sessions[0].base_branch, "main");
        assert_eq!(
            db_sessions[0].model,
            AgentKind::Gemini.default_model().as_str()
        );
        assert_eq!(db_sessions[0].status, "New");
        assert_eq!(activity_timestamps.len(), 1);
    }

    #[tokio::test]
    async fn test_create_session_keeps_default_smart_model_setting_when_session_model_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let first_session_id = app
            .create_session()
            .await
            .expect("failed to create first session");
        app.set_session_model(&first_session_id, AgentModel::Gpt53Codex)
            .await
            .expect("failed to set session model");
        let active_project_id = app.active_project_id();
        let default_smart_model_setting = app
            .services
            .db()
            .get_project_setting(active_project_id, SettingName::DefaultSmartModel.as_str())
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
        assert_eq!(second_session.model, AgentKind::Gemini.default_model());
        assert_eq!(default_smart_model_setting, None);

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
        assert_eq!(
            db_second_session.model,
            AgentKind::Gemini.default_model().as_str()
        );
    }

    #[tokio::test]
    async fn test_create_session_persists_default_smart_model_setting_when_last_used_model_is_enabled()
     {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
        let active_project_id = app.active_project_id();
        app.services
            .db()
            .upsert_project_setting(
                active_project_id,
                SettingName::LastUsedModelAsDefault.as_str(),
                "true",
            )
            .await
            .expect("failed to upsert last-used-model setting");
        let first_session_id = app
            .create_session()
            .await
            .expect("failed to create first session");

        // Act
        app.set_session_model(&first_session_id, AgentModel::Gpt53Codex)
            .await
            .expect("failed to set session model");
        let default_smart_model_setting = app
            .services
            .db()
            .get_project_setting(active_project_id, SettingName::DefaultSmartModel.as_str())
            .await
            .expect("failed to load setting");
        drop(app);
        let mut restarted_app = new_test_app_with_git_and_db(dir.path(), db).await;
        let second_session_id = restarted_app
            .create_session()
            .await
            .expect("failed to create second session");

        // Assert
        assert_eq!(
            default_smart_model_setting,
            Some(AgentModel::Gpt53Codex.as_str().to_string())
        );
        let second_session = restarted_app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == second_session_id)
            .expect("missing second session");
        assert_eq!(second_session.model, AgentModel::Gpt53Codex);
    }

    #[tokio::test]
    async fn test_create_session_reads_default_smart_model_from_db_setting() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let active_project_id = app.active_project_id();
        app.services
            .db()
            .upsert_project_setting(
                active_project_id,
                SettingName::DefaultSmartModel.as_str(),
                AgentModel::ClaudeHaiku4520251001.as_str(),
            )
            .await
            .expect("failed to upsert default smart model setting");

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
    async fn test_create_session_reads_legacy_default_model_setting_when_default_smart_key_is_missing()
     {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.services
            .db()
            .upsert_setting("DefaultModel", AgentModel::ClaudeHaiku4520251001.as_str())
            .await
            .expect("failed to upsert legacy default model setting");

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
        let activity_timestamps = app
            .services
            .db()
            .load_session_activity_timestamps()
            .await
            .expect("failed to load session activity timestamps");
        assert_eq!(db_sessions[0].prompt, "Hello");
        assert_eq!(db_sessions[0].output, " › Hello\n\n");
        assert_eq!(activity_timestamps.len(), 1);
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
                Arc::new(backend),
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
        wait_for_status(&mut app, &session_id, Status::Review).await;

        // Act
        app.reply(&session_id, "Reply").await;

        // Assert
        app.sessions.sync_from_handles();
        let session = &app.sessions.sessions[0];
        let output = &session.output;
        let activity_timestamps = app
            .services
            .db()
            .load_session_activity_timestamps()
            .await
            .expect("failed to load session activity timestamps");
        assert!(output.contains("Reply"));
        assert_eq!(activity_timestamps.len(), 1);
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
        let app = new_test_app_with_db(
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
    async fn test_create_session_uses_default_smart_model_setting_and_most_recent_permission_mode()
    {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
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
        db.upsert_project_setting(
            project_id,
            SettingName::DefaultSmartModel.as_str(),
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to upsert default smart model setting");
        db.update_session_updated_at("alpha0001", 1_i64)
            .await
            .expect("failed to update alpha0001 timestamp");
        db.update_session_updated_at("beta00002", 2_i64)
            .await
            .expect("failed to update beta00002 timestamp");
        for session_id in ["alpha0001", "beta00002"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create session data dir");
        }
        let mut app = new_test_app_with_git_and_db(dir.path(), db).await;

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

        db.update_session_updated_at("alpha000", 1_i64)
            .await
            .expect("failed to update alpha000 timestamp");
        db.update_session_updated_at("beta0000", 2_i64)
            .await
            .expect("failed to update beta0000 timestamp");

        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }

        // Act
        let app = new_test_app_with_db(
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

        db.update_session_created_at("alpha000", day_key_one * seconds_per_day + 10)
            .await
            .expect("failed to update alpha000 created_at");
        db.update_session_created_at("beta0000", day_key_one * seconds_per_day + 600)
            .await
            .expect("failed to update beta0000 created_at");
        db.update_session_created_at("gamma000", day_key_two * seconds_per_day + 50)
            .await
            .expect("failed to update gamma000 created_at");
        db.clear_session_activity()
            .await
            .expect("failed to clear session activity");
        db.backfill_session_activity_from_sessions()
            .await
            .expect("failed to backfill session activity from session rows");
        let working_dir = PathBuf::from("/tmp/test");
        let mut handles: HashMap<String, SessionHandles> = HashMap::new();
        let mut expected_activity_by_day: BTreeMap<i64, u32> = BTreeMap::new();
        for timestamp_seconds in [
            day_key_one * seconds_per_day + 10,
            day_key_one * seconds_per_day + 600,
            day_key_two * seconds_per_day + 50,
        ] {
            let day_key = util::activity_day_key_local(timestamp_seconds);
            let day_count = expected_activity_by_day.entry(day_key).or_insert(0);
            *day_count = day_count.saturating_add(1);
        }
        let expected_activity: Vec<DailyActivity> = expected_activity_by_day
            .into_iter()
            .map(|(day_key, session_count)| DailyActivity {
                day_key,
                session_count,
            })
            .collect();

        // Act
        let (sessions, stats_activity) =
            SessionManager::load_sessions(dir.path(), &db, project_id, &working_dir, &mut handles)
                .await;

        // Assert
        assert_eq!(sessions.len(), 3);
        assert_eq!(stats_activity, expected_activity);
    }

    #[tokio::test]
    async fn test_load_sessions_keeps_daily_activity_after_session_deletion() {
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
        db.insert_session_creation_activity_at("alpha000", 10)
            .await
            .expect("failed to persist first activity event");
        db.insert_session_creation_activity_at("beta0000", 20)
            .await
            .expect("failed to persist second activity event");
        db.delete_session("alpha000")
            .await
            .expect("failed to delete alpha000");
        let working_dir = PathBuf::from("/tmp/test");
        let mut handles: HashMap<String, SessionHandles> = HashMap::new();

        // Act
        let (sessions, stats_activity) =
            SessionManager::load_sessions(dir.path(), &db, project_id, &working_dir, &mut handles)
                .await;

        // Assert
        assert_eq!(sessions.len(), 1);
        let total_activity_count: u32 = stats_activity
            .iter()
            .map(|daily_activity| daily_activity.session_count)
            .sum();
        assert_eq!(total_activity_count, 2);
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
        db.update_session_updated_at("alpha000", 1)
            .await
            .expect("failed to set alpha000 timestamp");
        db.update_session_updated_at("beta0000", 2)
            .await
            .expect("failed to set beta0000 timestamp");
        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }
        let mut app = new_test_app_with_db(
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
        db.update_session_updated_at("alpha000", 1)
            .await
            .expect("failed to set alpha000 timestamp");
        db.update_session_updated_at("beta0000", 2)
            .await
            .expect("failed to set beta0000 timestamp");
        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }
        let mut app = new_test_app_with_db(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;
        let selected_session_id = app.sessions.sessions[1].id.clone();
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
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
        let app = new_test_app_with_db(
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
        let app = new_test_app_with_db(
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
    async fn test_load_sessions_uses_persisted_size_for_non_terminal_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.services
            .db()
            .update_session_size(&session_id, "S")
            .await
            .expect("failed to update size");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing created session");
        let session_folder = app.sessions.sessions[session_index].folder.clone();
        let changed_lines = "line\n".repeat(700);
        std::fs::write(session_folder.join("size-test.txt"), changed_lines)
            .expect("failed to write test file");

        // Act
        let (reloaded_sessions, _) = SessionManager::load_sessions(
            app.services.base_path(),
            app.services.db(),
            app.projects.active_project_id(),
            app.projects.working_dir(),
            &mut app.sessions.handles,
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
    async fn test_reply_turn_completion_persists_session_size() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|request| {
            let mut command = Command::new("sh");
            command
                .args([
                    "-lc",
                    "yes line | head -n 20 > turn-size-test.txt; echo turn-complete",
                ])
                .current_dir(request.folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());

            Ok(command)
        });

        // Act
        app.sessions
            .reply_with_backend(
                &app.services,
                &session_id,
                "compute size after turn",
                Arc::new(backend),
                AgentModel::ClaudeOpus46,
            )
            .await;
        wait_for_status(&mut app, &session_id, Status::Review).await;
        app.process_pending_app_events().await;
        let db_sessions = app
            .services
            .db()
            .load_sessions()
            .await
            .expect("failed to load");

        // Assert
        let session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing in-memory session");
        let db_session = db_sessions
            .iter()
            .find(|db_session| db_session.id == session_id)
            .expect("missing persisted session");
        assert_eq!(session.size, SessionSize::S);
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
            app.projects.active_project_id(),
            app.projects.working_dir(),
            &mut app.sessions.handles,
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
    /// Verifies end-to-end session execution for start and resume turns using
    /// a single `MockAgentChannel`. The first turn must use
    /// `AgentRequestKind::SessionStart` and produce output without
    /// `--resume`; the second must use `AgentRequestKind::SessionResume` and
    /// produce output with `--resume latest`.
    async fn test_spawn_integration() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = new_test_app_with_git_and_db(dir.path(), db).await;

        // One channel handles both turns; a counter distinguishes them so the
        // correct final response text is returned and mode assertions are made
        // per turn.
        let turn_count = Arc::new(Mutex::new(0usize));
        let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let mut mock_channel = MockAgentChannel::new();
        let turn_count_capture = Arc::clone(&turn_count);
        let done_capture = done_tx.clone();
        mock_channel
            .expect_run_turn()
            .returning(move |_, req, _event_tx| {
                let turn_index = {
                    let mut count = turn_count_capture.lock().expect("lock poisoned");
                    let current = *count;
                    *count += 1;
                    current
                };
                let delta_text = if turn_index == 0 {
                    assert!(
                        matches!(req.request_kind, AgentRequestKind::SessionStart),
                        "expected AgentRequestKind::SessionStart on first turn"
                    );
                    format!("--prompt {}\n", req.prompt)
                } else {
                    assert!(
                        matches!(req.request_kind, AgentRequestKind::SessionResume { .. }),
                        "expected AgentRequestKind::SessionResume on second turn"
                    );
                    format!("--prompt {} --resume latest\n", req.prompt)
                };
                let done = done_capture.clone();
                Box::pin(async move {
                    let _ = done.send(());
                    Ok(TurnResult {
                        assistant_message: AgentResponse::plain(&delta_text),
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    })
                })
            });
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        // Act — create and start session (start command)
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions
            .worker_service
            .test_agent_channels
            .insert(session_id.clone(), Arc::new(mock_channel));
        app.sessions
            .reply(&app.services, &session_id, "SpawnInit")
            .await;
        done_rx.recv().await.expect("first turn completion signal");
        wait_for_status(&mut app, &session_id, Status::Review).await;
        wait_for_output_contains(&mut app, &session_id, "SpawnInit", 200).await;

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
        let session_id = app.sessions.sessions[0].id.clone();
        app.sessions
            .reply(&app.services, &session_id, "SpawnReply")
            .await;
        done_rx.recv().await.expect("second turn completion signal");
        wait_for_output_contains(&mut app, &session_id, "--resume", 200).await;
        wait_for_status(&mut app, &session_id, Status::Review).await;

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
    /// Verifies that the first reply after a model switch replays the full
    /// session transcript
    /// (`AgentRequestKind::SessionResume { session_output: Some(...) }`) and
    /// subsequent replies omit it (`session_output: None`).
    ///
    /// A completion channel (`done_tx`/`done_rx`) is used to signal from
    /// inside the mock's async block so that `wait_for_status` always sees the
    /// worker in `InProgress` and correctly polls until `Review`. Without this,
    /// `wait_for_status` would return immediately because the initial status
    /// is already `Review` before the worker runs.
    async fn test_reply_with_backend_replays_history_once_after_model_switch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = new_test_app_with_git_and_db(dir.path(), db).await;

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

        // Persist the prompt so that `RefreshSessions` reloads from DB with the
        // correct value. `update_status(Review)` emits `RefreshSessions`, which
        // reloads sessions from DB; without persisting here, `session.prompt`
        // would be reset to `""` causing the second reply to use
        // `AgentRequestKind::SessionStart`.
        app.services
            .db()
            .update_session_prompt(&session_id, "Initial prompt")
            .await
            .expect("failed to persist initial prompt");

        app.set_session_model(&session_id, AgentModel::ClaudeSonnet46)
            .await
            .expect("failed to switch model");

        // Shared state to capture session_output from each turn's TurnRequest.
        let captured_session_outputs: Arc<Mutex<Vec<Option<String>>>> =
            Arc::new(Mutex::new(Vec::new()));

        // The done channel signals from inside the mock future so the test
        // can wait on each turn completing before calling `wait_for_status`.
        // This prevents `wait_for_status` from returning immediately when the
        // session is already in `Review` before the worker processes the turn.
        let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

        // Register a MockAgentChannel that collects session_output values from
        // Resume turns so they can be asserted synchronously after the test.
        let mut mock_channel = MockAgentChannel::new();
        let captured = Arc::clone(&captured_session_outputs);
        let done_capture = done_tx.clone();
        mock_channel.expect_run_turn().returning(move |_, req, _| {
            if let AgentRequestKind::SessionResume { session_output } = req.request_kind {
                captured.lock().expect("lock poisoned").push(session_output);
            }
            let done = done_capture.clone();
            Box::pin(async move {
                let _ = done.send(());
                Ok(TurnResult {
                    assistant_message: AgentResponse::plain(""),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));
        app.sessions
            .worker_service
            .test_agent_channels
            .insert(session_id.clone(), Arc::new(mock_channel));

        // Act — first reply after model switch: history should be replayed.
        app.sessions
            .reply(&app.services, &session_id, "Switch reply")
            .await;
        done_rx.recv().await.expect("first turn completion signal");
        wait_for_status(&mut app, &session_id, Status::Review).await;

        // Act — second reply: no history replay expected.
        app.sessions
            .reply(&app.services, &session_id, "Second reply")
            .await;
        done_rx.recv().await.expect("second turn completion signal");
        wait_for_status(&mut app, &session_id, Status::Review).await;

        // Assert
        let outputs = captured_session_outputs
            .lock()
            .expect("lock poisoned")
            .clone();
        assert_eq!(outputs.len(), 2, "expected exactly two Resume turns");
        let first_session_output = outputs[0]
            .as_deref()
            .expect("first reply should include session_output");
        assert!(
            first_session_output.contains("Initial prompt"),
            "first reply should replay history containing 'Initial prompt'"
        );
        assert!(
            outputs[1].is_none(),
            "second reply should not replay history"
        );
    }

    /// Ensures resumed review sessions replay persisted transcript output on
    /// the first reply after app restart.
    #[tokio::test]
    async fn test_reply_with_backend_replays_history_after_app_restart_for_review_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        let mut first_app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
        let session_id = first_app
            .create_session()
            .await
            .expect("failed to create session");
        let start_backend = create_mock_backend();
        first_app
            .sessions
            .reply_with_backend(
                &first_app.services,
                &session_id,
                "Initial prompt",
                Arc::new(start_backend),
                AgentModel::ClaudeSonnet46,
            )
            .await;
        wait_for_status(&mut first_app, &session_id, Status::Review).await;
        first_app.sessions.sync_from_handles();
        let first_run_output = first_app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.output.clone())
            .expect("missing persisted session");
        assert!(first_run_output.contains("Initial prompt"));
        assert!(first_run_output.contains("mock-start"));
        drop(first_app);

        let mut resumed_app = new_test_app_with_git_and_db(dir.path(), db).await;
        let resumed_session = resumed_app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing resumed session");
        assert_eq!(resumed_session.status, Status::Review);

        // Act
        let mut resume_backend = MockAgentBackend::new();
        resume_backend.expect_build_command().returning(|request| {
            assert!(request.request_kind.is_resume());

            let session_output = request
                .request_kind
                .session_output()
                .expect("expected replayed session output after restart");
            assert!(session_output.contains("Initial prompt"));
            assert!(session_output.contains("mock-start"));

            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(
                    "printf '{\"answer\":\"replayed-after-restart\",\"questions\":[],\"summary\":\
                     null}'",
                )
                .current_dir(request.folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            Ok(cmd)
        });
        resumed_app
            .sessions
            .reply_with_backend(
                &resumed_app.services,
                &session_id,
                "Restart reply",
                Arc::new(resume_backend),
                AgentModel::ClaudeSonnet46,
            )
            .await;

        // Assert
        wait_for_output_contains(
            &mut resumed_app,
            &session_id,
            "replayed-after-restart",
            2000,
        )
        .await;
        wait_for_status(&mut resumed_app, &session_id, Status::Review).await;
    }

    #[tokio::test]
    async fn test_spawn_session_task_auto_commits_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = new_test_app_with_git_and_db(dir.path(), db).await;
        let repo_root = dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(0..)
            .returning(move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Some(repo_root) })
            });
        mock_git_client
            .expect_create_worktree()
            .times(1)
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_has_commits_since()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .returning(|_| Box::pin(async { Ok(Some("Existing session commit".to_string())) }));
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .returning(|_, _, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        mock_git_client
            .expect_diff()
            .times(2)
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Create a session that writes a file so commit_all has something to commit
        let mut mock = MockAgentBackend::new();
        mock.expect_build_command().returning(|request| {
            let mut cmd = Command::new("bash");
            cmd.arg("-c")
                .arg(
                    "echo auto-content > auto-committed.txt; printf '{\"answer\":\"Auto commit \
                     done\",\"questions\":[],\"summary\":null}'",
                )
                .current_dir(request.folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            Ok(cmd)
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
                Arc::new(mock),
                AgentModel::ClaudeSonnet46,
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
    async fn test_commit_changes_reuses_existing_session_commit_message_in_tests() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = mockall::Sequence::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_has_commits_since()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(Some("Refine session work".to_string())) }));
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .withf(|_, base_branch, commit_message, strategy, no_verify| {
                base_branch == "main"
                    && commit_message == "Refine session work"
                    && *strategy == git::SingleCommitMessageStrategy::Replace
                    && !*no_verify
            })
            .in_sequence(&mut sequence)
            .returning(|_, _, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok("def5678".to_string()) }));

        // Act
        let outcome = SessionTaskService::commit_session_changes(
            &mock_git_client,
            &session_folder,
            "main",
            AgentModel::ClaudeSonnet46,
            false,
            false,
        )
        .await
        .expect("failed to commit existing session message");

        // Assert
        assert_eq!(outcome.commit_hash, "def5678");
        assert_eq!(outcome.commit_message, "Refine session work");
    }

    #[tokio::test]
    async fn test_spawn_session_task_skips_commit_when_nothing_to_commit() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let repo_root = dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(0..)
            .returning(move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Some(repo_root) })
            });
        mock_git_client
            .expect_create_worktree()
            .times(1)
            .returning(|_, worktree_path, _, _| {
                Box::pin(async move {
                    let fs_client = create_passthrough_mock_fs_client();
                    fs_client
                        .create_dir_all(worktree_path.clone())
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock worktree: {error}"
                            ))
                        })?;
                    fs_client
                        .create_dir_all(worktree_path.join(SESSION_DATA_DIR))
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock worktree data dir: {error}"
                            ))
                        })?;

                    Ok(())
                })
            });
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_diff()
            .times(0..)
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Agent that produces no file changes
        let mut mock = MockAgentBackend::new();
        mock.expect_build_command().returning(|request| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg("printf '{\"answer\":\"no-changes\",\"questions\":[],\"summary\":null}'")
                .current_dir(request.folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            Ok(cmd)
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
                Arc::new(mock),
                AgentModel::ClaudeOpus46,
            )
            .await;

        // Act — wait for agent to finish
        wait_for_status(&mut app, &session_id, Status::Review).await;

        // Assert — no-op commit output is visible
        let session = &app.sessions.sessions[0];
        let output = session.output.clone();
        assert!(
            output.contains("[Commit] No changes to commit."),
            "should contain no-op commit output when nothing to commit"
        );
        assert!(
            !output.contains("[Commit Error]"),
            "should not contain commit error when nothing to commit"
        );
    }

    #[tokio::test]
    async fn test_next_tab() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert_eq!(app.tabs.current(), Tab::Projects);
        app.tabs.next();
        assert_eq!(app.tabs.current(), Tab::Sessions);
        app.tabs.next();
        assert_eq!(app.tabs.current(), Tab::Stats);
        app.tabs.next();
        assert_eq!(app.tabs.current(), Tab::Settings);
        app.tabs.next();
        assert_eq!(app.tabs.current(), Tab::Projects);
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
                .to_string()
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
        let mut app = new_test_app_with_db(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            db,
        )
        .await;
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(|_| Box::pin(async { None }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        let result = app.create_session().await;

        // Assert - should fail because git repo doesn't actually exist
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .to_string()
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
        let mut app = new_test_app_with_db(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
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
            .returning(|_, _, _, _| {
                Box::pin(async {
                    Err(git::GitError::OutputParse(
                        "mock create_worktree failed".to_string(),
                    ))
                })
            });
        install_mock_git_client(&mut app, mock_git_client);

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
                .to_string()
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
                .to_string()
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
                .to_string()
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
                .to_string()
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
                .to_string()
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_rebase_session_updates_session_worktree_to_base_head() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let repo_root = dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(0..)
            .returning(move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Some(repo_root) })
            });
        mock_git_client
            .expect_create_worktree()
            .times(1)
            .returning(|_, worktree_path, _, _| {
                Box::pin(async move {
                    let fs_client = create_passthrough_mock_fs_client();
                    fs_client
                        .create_dir_all(worktree_path.clone())
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock worktree: {error}"
                            ))
                        })?;
                    fs_client
                        .create_dir_all(worktree_path.join(SESSION_DATA_DIR))
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock worktree data dir: {error}"
                            ))
                        })?;

                    Ok(())
                })
            });
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        install_mock_git_client(&mut app, mock_git_client);

        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.sessions[0].status = Status::Review;
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut session_status) = handles.status.lock()
        {
            *session_status = Status::Review;
        }

        // Act
        let result = app.rebase_session(&session_id).await;

        // Assert
        assert!(result.is_ok(), "rebase should succeed: {:?}", result.err());
        wait_for_output_contains(&mut app, &session_id, "[Rebase] Successfully rebased", 200).await;
    }

    #[tokio::test]
    async fn test_rebase_session_auto_commits_uncommitted_changes() {
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
            .returning(|_, worktree_path, _, _| {
                Box::pin(async move {
                    let fs_client = create_passthrough_mock_fs_client();
                    fs_client
                        .create_dir_all(worktree_path.clone())
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock worktree: {error}"
                            ))
                        })?;
                    fs_client
                        .create_dir_all(worktree_path.join(SESSION_DATA_DIR))
                        .await
                        .map_err(|error| {
                            git::GitError::OutputParse(format!(
                                "Failed to create mock worktree data dir: {error}"
                            ))
                        })?;

                    Ok(())
                })
            });
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_has_commits_since()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .returning(|_| Box::pin(async { Ok(Some("Existing session commit".to_string())) }));
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .withf(|_, _, _, _, no_verify| *no_verify)
            .returning(|_, _, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .returning(|_| Box::pin(async { Ok("cafe123".to_string()) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        install_mock_git_client(&mut app, mock_git_client);

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
        // The commit call itself is verified by mock expectations; output can
        // be refreshed from persisted state before the commit line is observed
        // in this integration test under full-suite runtime contention.
        app.refresh_sessions_now().await;
    }

    #[tokio::test]
    async fn test_sync_main_uses_active_project_branch_from_context() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.projects.update_active_project_context(
            app.active_project_id(),
            app.projects.project_name().to_string(),
            Some("develop".to_string()),
            None,
            dir.path().to_path_buf(),
        );
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
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));

        // Act
        let result = SessionManager::sync_main_for_project(
            app.projects.git_branch().map(str::to_string),
            app.projects.working_dir().to_path_buf(),
            Arc::new(mock_git_client),
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
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));

        // Act
        let result = SessionManager::sync_main_for_project(
            app.projects.git_branch().map(str::to_string),
            app.projects.working_dir().to_path_buf(),
            Arc::new(mock_git_client),
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
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let mut ahead_behind_calls = 0_u8;
        mock_git_client
            .expect_get_ahead_behind()
            .times(2)
            .returning(move |_| {
                ahead_behind_calls = ahead_behind_calls.saturating_add(1);
                let value = if ahead_behind_calls == 1 {
                    (1, 2)
                } else {
                    (0, 0)
                };

                Box::pin(async move { Ok(value) })
            });
        mock_git_client
            .expect_list_upstream_commit_titles()
            .times(1)
            .returning(|_| Box::pin(async { Ok(vec!["remote fix".to_string()]) }));
        mock_git_client
            .expect_pull_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(git::PullRebaseResult::Completed) }));
        mock_git_client
            .expect_list_local_commit_titles()
            .times(1)
            .returning(|_| Box::pin(async { Ok(vec!["local work".to_string()]) }));
        mock_git_client
            .expect_push_current_branch()
            .times(1)
            .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));

        // Act
        let result = SessionManager::sync_main_for_project(
            Some("main".to_string()),
            dir.path().to_path_buf(),
            Arc::new(mock_git_client),
            AgentModel::Gemini3FlashPreview,
        )
        .await;

        // Assert
        let outcome = result.expect("sync should succeed");
        assert_eq!(outcome.pulled_commits, Some(2));
        assert_eq!(outcome.pushed_commits, Some(0));
        assert_eq!(outcome.pulled_commit_titles, vec!["remote fix".to_string()]);
        assert_eq!(outcome.pushed_commit_titles, vec!["local work".to_string()]);
        assert!(outcome.resolved_conflict_files.is_empty());
    }

    #[tokio::test]
    /// Ensures canceling a review session persists `Canceled` status and
    /// removes its dedicated worktree checkout.
    async fn test_cancel_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_folder = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing session")
            .folder
            .clone();
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
        assert!(
            !session_folder.exists(),
            "session worktree should be removed after cancellation"
        );
    }

    #[tokio::test]
    /// Ensures canceling a review session shuts down its app-server runtime.
    async fn test_cancel_session_triggers_app_server_shutdown() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut mock_app_server = MockAppServerClient::new();
        mock_app_server
            .expect_run_turn()
            .times(1)
            .returning(|_, _| {
                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"ready","questions":[],"summary":null}"#
                            .to_string(),
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        pid: None,
                        provider_conversation_id: None,
                    })
                })
            });
        mock_app_server
            .expect_shutdown_session()
            .times(1)
            .returning(move |session_id| {
                let shutdown_tx = shutdown_tx.clone();
                Box::pin(async move {
                    let _ = shutdown_tx.send(session_id);
                })
            });
        let app_server_client: Arc<dyn app_server::AppServerClient> = Arc::new(mock_app_server);
        let mut app = new_test_app_with_db_and_app_server(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
            app_server_client,
        )
        .await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Act
        app.sessions
            .reply(&app.services, &session_id, "Start")
            .await;
        wait_for_status(&mut app, &session_id, Status::Review).await;
        app.cancel_session(&session_id)
            .await
            .expect("failed to cancel session");
        app.process_pending_app_events().await;
        wait_for_status(&mut app, &session_id, Status::Canceled).await;
        let shutdown_session_id =
            tokio::time::timeout(std::time::Duration::from_secs(1), shutdown_rx.recv())
                .await
                .expect("timed out waiting for app-server shutdown")
                .expect("missing shutdown session id");

        // Assert
        assert_eq!(shutdown_session_id, session_id);
    }

    #[tokio::test]
    /// Ensures transitioning a session to `Done` shuts down its app-server
    /// runtime.
    async fn test_done_status_triggers_app_server_shutdown() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut mock_app_server = MockAppServerClient::new();
        mock_app_server
            .expect_run_turn()
            .times(1)
            .returning(|_, _| {
                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"ready","questions":[],"summary":null}"#
                            .to_string(),
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        pid: None,
                        provider_conversation_id: None,
                    })
                })
            });
        mock_app_server
            .expect_shutdown_session()
            .times(1)
            .returning(move |session_id| {
                let shutdown_tx = shutdown_tx.clone();
                Box::pin(async move {
                    let _ = shutdown_tx.send(session_id);
                })
            });
        let app_server_client: Arc<dyn app_server::AppServerClient> = Arc::new(mock_app_server);
        let mut app = new_test_app_with_db_and_app_server(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
            app_server_client,
        )
        .await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Act
        app.sessions
            .reply(&app.services, &session_id, "Start")
            .await;
        wait_for_status(&mut app, &session_id, Status::Review).await;
        let handles = app
            .sessions
            .session_handles_or_err(&session_id)
            .expect("missing session handles");
        let session_status = Arc::clone(&handles.status);
        let app_event_tx = app.services.event_sender();
        let transitioned_to_merging = SessionTaskService::update_status(
            &session_status,
            app.services.db(),
            &app_event_tx,
            &session_id,
            Status::Merging,
        )
        .await;
        assert!(
            transitioned_to_merging,
            "status transition to Merging should succeed"
        );
        let transitioned_to_done = SessionTaskService::update_status(
            &session_status,
            app.services.db(),
            &app_event_tx,
            &session_id,
            Status::Done,
        )
        .await;
        assert!(
            transitioned_to_done,
            "status transition to Done should succeed"
        );
        app.process_pending_app_events().await;
        wait_for_status(&mut app, &session_id, Status::Done).await;
        let shutdown_session_id =
            tokio::time::timeout(std::time::Duration::from_secs(1), shutdown_rx.recv())
                .await
                .expect("timed out waiting for app-server shutdown")
                .expect("missing shutdown session id");

        // Assert
        assert_eq!(shutdown_session_id, session_id);
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
                .to_string()
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
                .to_string()
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_cleanup_merged_session_worktree_without_repo_hint() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let worktree_folder = dir.path().join("merged-worktree");
        let branch_name = "agentty/cleanup123";
        std::fs::create_dir_all(&worktree_folder).expect("failed to create worktree folder");
        assert!(
            worktree_folder.exists(),
            "worktree should exist before cleanup"
        );
        let repo_root = dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_main_repo_root()
            .times(1)
            .returning(move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Ok(repo_root) })
            });
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .returning(|worktree_path| {
                Box::pin(async move {
                    let fs_client = create_passthrough_mock_fs_client();
                    let _ = fs_client.remove_dir_all(worktree_path).await;

                    Ok(())
                })
            });
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .withf(|_, branch| branch == "agentty/cleanup123")
            .returning(|_, _| Box::pin(async { Ok(()) }));

        // Act
        let result = SessionManager::cleanup_merged_session_worktree(
            worktree_folder.clone(),
            Arc::new(create_passthrough_mock_fs_client()),
            Arc::new(mock_git_client),
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

    #[test]
    fn test_parse_merge_commit_message_response_with_protocol_message() {
        // Arrange
        let content = r#"{"answer":"Title\n\n- Detail","questions":[],"summary":null}"#;

        // Act
        let parsed = crate::infra::agent::protocol::parse_agent_response_strict(content)
            .ok()
            .map(|response| response.to_answer_display_text())
            .filter(|answer_text| !answer_text.trim().is_empty());

        // Assert
        assert!(parsed.is_some());
        assert_eq!(parsed.as_deref(), Some("Title\n\n- Detail"));
    }

    #[test]
    fn test_parse_merge_commit_message_response_rejects_non_protocol_json() {
        // Arrange
        let content = r#"{"title":"Title","description":"- Detail"}"#;

        // Act
        let parsed = crate::infra::agent::protocol::parse_agent_response_strict(content)
            .ok()
            .map(|response| response.to_answer_display_text())
            .filter(|answer_text| !answer_text.trim().is_empty());

        // Assert
        assert!(parsed.is_none());
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
}
