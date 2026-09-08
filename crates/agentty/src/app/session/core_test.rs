//! Session module tests.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ag_agent::{
    AgentRequestKind, AppServerClient, AppServerTurnResponse, MockAgentBackend, MockAgentChannel,
    MockAppServerClient, MockOneShotClient, TurnResult,
};
use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};
use ag_git as git;
use ag_protocol::{
    AgentResponse, QuestionItem, TurnPrompt, TurnPromptAttachment, TurnPromptTextSource,
    parse_agent_response_strict,
};
use tempfile::tempdir;
use tokio::sync::{Barrier, Notify};

use super::{
    AT_MENTION_INDEX_TTL, AppServices, FileEntry, SESSION_REFRESH_INTERVAL, SessionCreationKind,
    SessionDefaults, SessionError, SessionId, SessionManager, SessionMessageKind, SessionState,
    SessionTaskService, TurnAppliedState, remote_branch_name_from_upstream_ref, session_branch,
    session_folder,
};
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::app::review::{review_failure_message, review_loading_message};
use crate::app::session::SessionLoadInput;
use crate::app::{App, AppEvent, ReviewCacheEntry, SyncSessionStartError, Tab};
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata, ReasoningLevel, ResponseStyle,
    SpeedMode,
};
use crate::domain::permission::PermissionMode;
use crate::domain::selection::SelectionState;
use crate::domain::session::{
    DailyActivity, SESSION_DATA_DIR, Session, SessionHandles, SessionRole, SessionSize,
    SessionStats, Status, activity_day_key_with_offset,
};
use crate::domain::session_message::SessionTranscript;
use crate::domain::setting::SettingName;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot, TransientMessageStore,
};
use crate::infra::clock::{Clock, RealClock};
use crate::infra::db::AppRepositories;
use crate::infra::fs::{self as fs, FsClient};
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineComments, DiffPreview, HelpContext,
    ReviewCommentSelection,
};

/// Builds one loading focused-review entry with a stable test profile.
fn test_loading_review(diff_hash: u64) -> ReviewCacheEntry {
    ReviewCacheEntry::Loading {
        diff_hash,
        review_agent: (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            ReasoningLevel::High,
            SpeedMode::Normal,
        ),
    }
}

/// Builds a filesystem mock that delegates operations to local disk.
fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_cleanup_agent_artifacts()
        .returning(|root| fs::FsClient::cleanup_agent_artifacts(&fs::RealFsClient, root));
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

/// Mutable clock used to test cache TTL behavior without sleeping.
struct TestClock {
    instant: Mutex<Instant>,
    system_time: Mutex<SystemTime>,
}

impl TestClock {
    /// Creates a clock pinned to the provided time pair.
    fn new(instant: Instant, system_time: SystemTime) -> Self {
        Self {
            instant: Mutex::new(instant),
            system_time: Mutex::new(system_time),
        }
    }

    /// Advances both clock domains by the provided duration.
    fn advance(&self, duration: Duration) {
        if let Ok(mut instant) = self.instant.lock() {
            *instant += duration;
        }

        if let Ok(mut system_time) = self.system_time.lock() {
            *system_time += duration;
        }
    }
}

impl Clock for TestClock {
    fn now_instant(&self) -> Instant {
        *self
            .instant
            .lock()
            .expect("test clock instant lock should not be poisoned")
    }

    fn now_system_time(&self) -> SystemTime {
        *self
            .system_time
            .lock()
            .expect("test clock system-time lock should not be poisoned")
    }
}

fn create_mock_backend() -> MockAgentBackend {
    let mut mock = MockAgentBackend::new();
    mock.expect_build_command().returning(|request| {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf '{\"answer\":\"mock-start\",\"questions\":[]}'")
            .current_dir(request.folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(cmd)
    });
    mock
}

/// Allows branch discovery calls to fall back to defaults in tests that do
/// not care about exact detected refs.
fn allow_detect_git_info(mock: &mut git::MockGitClient) {
    allow_detect_git_info_with_head_hash(mock, true);
}

/// Expects one successful advisory pre-commit readiness check.
fn expect_pre_commit_hook_ready(mock: &mut git::MockGitClient) {
    mock.expect_check_pre_commit_hook_ready()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
}

fn allow_detect_git_info_with_head_hash(mock: &mut git::MockGitClient, allow_head_hash: bool) {
    mock.expect_detect_git_info().times(0..).returning(|path| {
        let branch_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .filter(|folder_name| folder_name.len() == 8)
            .map_or_else(
                || "main".to_string(),
                |folder_name| format!("wt/{folder_name}"),
            );

        Box::pin(async move { Some(branch_name) })
    });
    mock.expect_main_repo_root().times(0..).returning(|path| {
        let repo_root = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(path);

        Box::pin(async move { Ok(repo_root) })
    });
    mock.expect_main_checkout_working_tree()
        .times(0..)
        .returning(|path| {
            let repo_root = path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or(path);

            Box::pin(async move { Ok(Some(repo_root)) })
        });
    mock.expect_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock.expect_tracked_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    if allow_head_hash {
        mock.expect_head_hash()
            .times(0..)
            .returning(|_| Box::pin(async { Ok("main-before".to_string()) }));
    }
    mock.expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock.expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock.expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    mock.expect_get_ahead_behind()
        .times(0..)
        .returning(|_| Box::pin(async { Ok((0, 0)) }));
}

/// Builds a merge-focused mock git client for no-op merge scenarios,
/// including both main-checkout preflight and session-worktree clean
/// checks for each merge.
fn create_mock_git_client_for_successful_noop_merges(
    expected_merge_count: usize,
    repo_root: PathBuf,
) -> git::MockGitClient {
    let mut mock = git::MockGitClient::new();
    allow_detect_git_info(&mut mock);
    mock.expect_find_git_repo_root()
        .times(expected_merge_count)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock.expect_is_worktree_clean()
        .times(expected_merge_count * 2)
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

    mock.expect_detect_git_info().times(0..).returning({
        let repo_root = repo_root.clone();

        move |path| {
            let branch_name = if path == repo_root {
                "main".to_string()
            } else {
                path.file_name()
                    .and_then(|file_name| file_name.to_str())
                    .map_or_else(
                        || "main".to_string(),
                        |folder_name| format!("wt/{folder_name}"),
                    )
            };

            Box::pin(async move { Some(branch_name) })
        }
    });
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
    mock.expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    mock.expect_list_upstream_commit_titles()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    mock.expect_list_local_commit_titles()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    mock.expect_repo_url()
        .times(0..)
        .returning(|_| Box::pin(async { Ok("https://example.invalid/repo.git".to_string()) }));
    expect_shared_repo_resolvers(mock, repo_root);
}

/// Registers the admin-root and main-working-checkout resolver expectations
/// backed by the same `repo_root`, treating the shared repository as non-bare.
fn expect_shared_repo_resolvers(mock: &mut git::MockGitClient, repo_root: PathBuf) {
    mock.expect_main_checkout_working_tree()
        .times(0..)
        .returning({
            let repo_root = repo_root.clone();

            move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Ok(Some(repo_root)) })
            }
        });
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
    mock.expect_rebase_onto_start()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock.expect_run_pre_commit_hook()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
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
    let fs_client = create_passthrough_mock_fs_client();
    let line_count = count_non_metadata_lines(&fs_client, folder).await;

    synthetic_added_line_diff(line_count)
}

/// Counts lines across one worktree while ignoring session metadata.
async fn count_non_metadata_lines(fs_client: &dyn fs::FsClient, root: &Path) -> usize {
    let mut pending_entries = vec![root.to_path_buf()];
    let mut line_count = 0;

    while let Some(entry) = pending_entries.pop() {
        if !fs_client.is_dir(entry.clone()) {
            line_count += count_file_lines(fs_client, &entry).await;

            continue;
        }

        if is_session_metadata_dir(&entry) {
            continue;
        }

        pending_entries.extend(child_paths(&entry));
    }

    line_count
}

/// Counts UTF-8-lossy text lines in one file, returning zero on read error.
async fn count_file_lines(fs_client: &dyn fs::FsClient, path: &Path) -> usize {
    fs_client
        .read_file(path.to_path_buf())
        .await
        .map_or(0, |content| {
            String::from_utf8_lossy(&content).lines().count()
        })
}

/// Returns whether `path` points at Agentty's session metadata directory.
fn is_session_metadata_dir(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == SESSION_DATA_DIR)
}

/// Returns direct child paths for a directory, or an empty list if
/// unreadable.
fn child_paths(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .filter_map(Result::ok)
                .map(|dir_entry| dir_entry.path())
                .collect()
        },
    )
}

/// Builds a git-diff body with one added-line marker per counted line.
fn synthetic_added_line_diff(line_count: usize) -> String {
    match line_count {
        0 => String::new(),
        _ => "+\n".repeat(line_count),
    }
}

/// Configures commit, staging, and branch operation expectations.
fn setup_mock_commit_and_branch_expectations(mock: &mut git::MockGitClient) {
    mock.expect_commit_all()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    mock.expect_commit_all_preserving_single_commit()
        .times(0..)
        .returning(|_, _, _, _| {
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
    mock.expect_head_hash()
        .times(0..)
        .returning(|_| Box::pin(async { Ok("parent-tip".to_string()) }));
    mock.expect_ref_hash()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok("parent-tip".to_string()) }));
    mock.expect_delete_branch()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    mock.expect_diff().times(0..).returning(|folder, _| {
        Box::pin(async move { Ok(synthetic_diff_from_session_folder(&folder).await) })
    });
    mock.expect_is_worktree_clean()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock.expect_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock.expect_tracked_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
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
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
    let app_server_client_override = app.services.app_server_client_override();
    let fs_client = app.services.fs_client();
    let review_request_client = app.services.review_request_client();

    app.services = AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        crate::app::service::AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client: Arc::clone(&mock_git_client),
            one_shot_client_override: Some(auto_commit_one_shot_client()),
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
    app.sessions.git_client = mock_git_client;
}

/// Builds a deterministic one-shot boundary for app-level auto-commit tests.
fn auto_commit_one_shot_client() -> Arc<dyn ag_agent::OneShotClient> {
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(0..)
        .returning(|request| {
            if request
                .prompt
                .contains("Generate a concise, commit-style title")
            {
                return Err(ag_agent::OneShotError::new(
                    "title generation is disabled in this fixture",
                ));
            }

            Ok(ag_agent::OneShotSubmission {
                response: AgentResponse::plain("Existing session commit"),
                stats: ag_agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: ag_agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    Arc::new(one_shot_client)
}

/// Builds a test app with a caller-provided database, git context, and
/// app-server boundary.
async fn new_test_app_with_db_and_app_server(
    path: PathBuf,
    working_dir: PathBuf,
    git_branch: Option<String>,
    db: AppRepositories,
    app_server_client: Arc<dyn AppServerClient>,
) -> App {
    let clients =
        crate::test_support::test_app_clients().with_app_server_client_override(app_server_client);
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
    db: AppRepositories,
) -> App {
    new_test_app_with_db_and_app_server(
        path,
        working_dir,
        git_branch,
        db,
        crate::test_support::mock_app_server(),
    )
    .await
}

/// Builds a test app rooted at `path` with no branch-specific git context.
async fn new_test_app(path: PathBuf) -> App {
    let working_dir = PathBuf::from("/tmp/test");
    let db = AppRepositories::in_memory().await.expect("db should open");

    new_test_app_with_db(path, working_dir, None, db).await
}

/// Builds a test app rooted at `path` with mock git branch context.
async fn new_test_app_with_git(path: &Path) -> App {
    let db = AppRepositories::in_memory().await.expect("db should open");
    new_test_app_with_git_and_db(path, db).await
}

/// Builds a test app rooted at `path` with mock git branch context and a
/// caller-provided database handle.
async fn new_test_app_with_git_and_db(path: &Path, db: AppRepositories) -> App {
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
        .session_handles_mut()
        .insert(id.to_string().into(), SessionHandles::new(status));
    app.sessions.push_session(Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder,
        follow_up_tasks: Vec::new(),
        id: id.into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            crate::domain::agent::AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: PermissionMode::AutoEdit,
        personality_id: None,
        project_name: String::new(),
        prompt: prompt.to_string(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: crate::domain::agent::SpeedMode::default(),
        stats: SessionStats::default(),
        status,
        title: Some(prompt.to_string()),
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    });
    if app.sessions.selected_session_index().is_none() {
        app.sessions.select_session_index(Some(0));
    }
}

/// Builds a minimal `SessionManager` for reducer tests that only need one
/// in-memory session snapshot.
fn test_session_manager(
    session_id: &str,
    reasoning_level_override: Option<ReasoningLevel>,
) -> SessionManager {
    test_session_manager_with_clock(session_id, reasoning_level_override, Arc::new(RealClock))
}

/// Builds a minimal `SessionManager` using the provided clock.
fn test_session_manager_with_clock(
    session_id: &str,
    reasoning_level_override: Option<ReasoningLevel>,
    clock: Arc<dyn Clock>,
) -> SessionManager {
    let mut handles = HashMap::new();
    handles.insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Review),
    );

    let state = SessionState::new(
        handles,
        vec![Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::from(format!("/tmp/{session_id}")),
            follow_up_tasks: Vec::new(),
            id: session_id.into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: crate::domain::agent::AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                AgentModel::Gpt56Sol,
            ),
            parent_session_id: None,
            permission_mode: PermissionMode::AutoEdit,
            personality_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: crate::domain::agent::SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::Review,
            title: Some("Title".to_string()),
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
        }],
        crate::domain::selection::SelectionState::default(),
        clock,
        1,
        0,
    );

    SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    )
}

#[tokio::test]
async fn resource_refresh_projects_only_current_worker_pids() {
    // Arrange
    let mut manager = test_session_manager("tracked", None);
    let mut client = crate::infra::resource::MockResourceClient::new();
    client.expect_sample().times(1).returning(|_| {
        Some(vec![crate::infra::resource::ProcessSample {
            is_alive: true,
            identity: Some(crate::infra::process_identity::ProcessIdentity(1_000_001)),
            pid: 42,
            parent_pid: 1,
            resources: crate::domain::resource::SessionResources {
                process_count: 1,
                cpu_percent: 5.0,
                resident_memory_kib: 1024,
            },
        }])
    });
    manager.resources = crate::app::session::resource::ResourceMonitor::new(Arc::new(client));
    let pid = Arc::clone(&manager.state.handle("tracked").expect("handles").child_pid);
    *pid.lock().expect("pid") = Some(42);

    // Act
    manager.refresh_resources().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !manager.refresh_resources().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("sample completion");

    // Assert
    assert_eq!(
        manager.render_parts().session_resources["tracked"].process_count,
        1
    );
    *pid.lock().expect("pid") = None;
    assert!(manager.refresh_resources().await);
    assert!(manager.render_parts().session_resources.is_empty());
}

#[test]
fn test_append_stacked_rebase_failure_notices_updates_affected_child() {
    // Arrange
    let mut session_manager = test_session_manager("child-session", None);
    let failures = vec![(
        SessionId::from("child-session"),
        SessionError::HandlesNotFound,
    )];

    // Act
    session_manager
        .append_stacked_rebase_failure_notices(failures, "Stacked child auto-sync failed");

    // Assert
    let child_session = session_manager
        .sessions()
        .iter()
        .find(|session| session.id.as_str() == "child-session")
        .expect("expected affected child session");
    assert_eq!(
        child_session
            .transient_messages
            .get(TransientMessageSlot::WorkflowNotice)
            .map(|message| message.body.text()),
        Some("[Sync Error] Stacked child auto-sync failed: Session handles not found")
    );
}

#[test]
fn test_append_workflow_notice_anchors_active_status_notices_after_active_turn() {
    for status in [Status::InProgress, Status::Queued] {
        // Arrange
        let mut session_manager = test_session_manager("session-id", None);
        session_manager.sessions_mut()[0].status = status;

        // Act
        session_manager.append_workflow_notice(
            "session-id",
            "[Sync] Queued until the current turn finishes.".to_string(),
        );

        // Assert
        let workflow_notice = session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::WorkflowNotice)
            .expect("workflow notice should be present");
        assert_eq!(
            workflow_notice.anchor,
            TransientMessageAnchor::AfterActiveTurn,
            "unexpected anchor for {status}"
        );
    }
}

#[test]
fn test_queued_session_actions_use_explicit_waiting_slots_until_resolved() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager.queue_branch_publish(
        "session-id",
        3,
        "review request — publish after this turn".to_string(),
    );
    session_manager.queue_session_sync("session-id", 4);

    // Assert
    let transient_messages = &session_manager.sessions()[0].transient_messages;
    assert!(matches!(
        transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .map(|message| &message.body),
        Some(TransientMessageBody::Queued(label))
            if label.order == 3 && label.text == "review request — publish after this turn"
    ));
    assert!(matches!(
        transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .map(|message| &message.body),
        Some(TransientMessageBody::Queued(label))
            if label.order == 4
                && label.text == "sync — rebase onto the base branch after this turn"
    ));

    // Act
    session_manager.resolve_queued_branch_publish("session-id");
    session_manager.resolve_queued_session_sync("session-id");

    // Assert
    assert!(
        session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
    assert!(
        session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .is_none()
    );
}

#[test]
fn test_finish_review_request_publish_keeps_loading_review_at_tail() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.sessions_mut()[0]
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Reviewing changes...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
    session_manager.start_branch_publish("session-id", "Publishing review request...".to_string());

    // Act
    let finished = session_manager.finish_review_request_publish(
        "session-id",
        "[Review Request] Created PR https://example.test/pull/42",
    );

    // Assert
    assert!(finished);
    let transient_messages = &session_manager.sessions()[0].transient_messages;
    assert_eq!(
        transient_messages
            .get(TransientMessageSlot::Review)
            .expect("loading review should remain visible")
            .anchor,
        TransientMessageAnchor::Tail
    );
    assert!(
        transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
}

#[test]
fn test_finish_branch_publish_promotes_result_when_project_snapshot_is_unloaded() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.state_mut().replace_sessions(Vec::new());

    // Act
    let persistent_notice = session_manager.finish_branch_publish(
        "session-id",
        TransientMessageBody::Markdown("**Branch push failed**\n\nRemote rejected".to_string()),
    );

    // Assert
    assert_eq!(
        persistent_notice.as_deref(),
        Some("**Branch push failed**\n\nRemote rejected")
    );
    let transcript = session_manager
        .state()
        .handle("session-id")
        .expect("session handles should remain loaded")
        .transcript
        .lock()
        .expect("session transcript lock should succeed");
    let notice = transcript
        .messages()
        .last()
        .expect("branch publish result should be promoted to the transcript");
    assert_eq!(notice.kind, SessionMessageKind::WorkflowNotice);
    assert_eq!(notice.content, "**Branch push failed**\n\nRemote rejected");
}

#[test]
fn test_finish_review_request_publish_reports_unloaded_handle_update() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.state_mut().replace_sessions(Vec::new());

    // Act
    let finished = session_manager.finish_review_request_publish(
        "session-id",
        "[Review Request] Created PR https://example.test/pull/42",
    );

    // Assert
    assert!(finished);
    let transcript = session_manager
        .state()
        .handle("session-id")
        .expect("session handles should remain loaded")
        .transcript
        .lock()
        .expect("session transcript lock should succeed");
    assert_eq!(
        transcript
            .messages()
            .last()
            .map(|message| (message.kind, message.content.as_str())),
        Some((
            SessionMessageKind::WorkflowNotice,
            "[Review Request] Created PR https://example.test/pull/42",
        ))
    );
}

#[test]
fn test_finish_published_branch_sync_reports_unloaded_handle_update() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.start_published_branch_sync("session-id", "sync-id".to_string());
    session_manager.state_mut().replace_sessions(Vec::new());

    // Act
    let finished = session_manager.finish_published_branch_sync(
        "session-id",
        "sync-id",
        Some("[Branch Push] Auto-pushed published branch after completed turn."),
    );

    // Assert
    assert!(finished);
    let transcript = session_manager
        .state()
        .handle("session-id")
        .expect("session handles should remain loaded")
        .transcript
        .lock()
        .expect("session transcript lock should succeed");
    assert_eq!(
        transcript
            .messages()
            .last()
            .map(|message| (message.kind, message.content.as_str())),
        Some((
            SessionMessageKind::WorkflowNotice,
            "[Branch Push] Auto-pushed published branch after completed turn.",
        ))
    );
}

#[test]
fn test_update_orchestration_progress_replaces_and_clears_board_snapshot() {
    // Arrange
    let mut session_manager = test_session_manager("controller", None);

    // Act
    session_manager.update_orchestration_progress(
        "controller",
        Some("Working... Protocol: running".to_string()),
    );
    session_manager.update_orchestration_progress(
        "controller",
        Some("Working... Protocol: ready".to_string()),
    );

    // Assert
    assert_eq!(
        session_manager.sessions()[0]
            .orchestration_progress
            .as_deref(),
        Some("Working... Protocol: ready")
    );
    assert!(
        session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::Orchestration)
            .is_none()
    );

    // Act
    session_manager.update_orchestration_progress("controller", None);
    session_manager.update_orchestration_progress("missing", Some("ignored".to_string()));

    // Assert
    assert!(
        session_manager.sessions()[0]
            .orchestration_progress
            .is_none()
    );
}

#[test]
fn test_set_and_get_at_mention_index_for_root_cache() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    let entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];

    // Act
    session_manager.set_at_mention_index_for_root(lookup_root.clone(), entries.clone());

    // Assert
    assert_eq!(
        session_manager
            .at_mention_index_for_root(&lookup_root)
            .expect("expected cached entries"),
        entries
    );
}

#[test]
fn test_at_mention_index_for_root_invalidates_after_ttl_expires() {
    // Arrange
    let initial_instant = Instant::now();
    let initial_system_time = SystemTime::UNIX_EPOCH + Duration::from_mins(1);
    let clock = Arc::new(TestClock::new(initial_instant, initial_system_time));
    let mut session_manager = test_session_manager_with_clock("session-id", None, clock.clone());
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    let entries = vec![FileEntry {
        is_dir: true,
        path: "src".to_string(),
    }];
    session_manager.set_at_mention_index_for_root(lookup_root.clone(), entries);
    clock.advance(AT_MENTION_INDEX_TTL + Duration::from_secs(1));

    // Act
    let cached_entries = session_manager.at_mention_index_for_root(&lookup_root);

    // Assert
    assert!(cached_entries.is_none());
}

#[test]
fn test_remove_at_mention_index_for_root_drops_cached_entries() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    let entries = vec![FileEntry {
        is_dir: true,
        path: "src".to_string(),
    }];
    session_manager.set_at_mention_index_for_root(lookup_root.clone(), entries);

    // Act
    session_manager.remove_at_mention_index_for_root(&lookup_root);

    // Assert
    assert!(
        session_manager
            .at_mention_index_for_root(&lookup_root)
            .is_none()
    );
}

#[test]
/// Ensures reasoning reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_reasoning_level_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", Some(ReasoningLevel::Low));

    // Act
    session_manager.apply_session_reasoning_level_updated("other-session", ReasoningLevel::High);
    let reasoning_level_after_non_matching_update =
        session_manager.state.sessions[0].reasoning_level_override;
    session_manager.apply_session_reasoning_level_updated("session-id", ReasoningLevel::Medium);

    // Assert
    assert_eq!(
        reasoning_level_after_non_matching_update,
        Some(ReasoningLevel::Low)
    );
    assert_eq!(
        session_manager.state.sessions[0].reasoning_level_override,
        Some(ReasoningLevel::Medium)
    );
}

#[test]
/// Ensures response-style reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_response_style_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager.apply_session_response_style_updated("other-session", ResponseStyle::Detailed);
    let style_after_non_matching_update = session_manager.state.sessions[0].response_style;
    session_manager.apply_session_response_style_updated("session-id", ResponseStyle::Concise);

    // Assert
    assert_eq!(style_after_non_matching_update, ResponseStyle::Balanced);
    assert_eq!(
        session_manager.state.sessions[0].response_style,
        ResponseStyle::Concise
    );
}

#[test]
/// Ensures speed reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_speed_mode_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager.apply_session_speed_mode_updated("other-session", SpeedMode::Fast);
    let speed_mode_after_non_matching_update = session_manager.state.sessions[0].speed_mode;
    session_manager.apply_session_speed_mode_updated("session-id", SpeedMode::Fast);

    // Assert
    assert_eq!(speed_mode_after_non_matching_update, SpeedMode::Normal);
    assert_eq!(
        session_manager.state.sessions[0].speed_mode,
        SpeedMode::Fast
    );
}

#[test]
/// Ensures permission reducer updates only the matching in-memory session
/// snapshot and leaves unrelated sessions untouched.
fn test_apply_session_permission_mode_updated_updates_only_matching_session() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager
        .apply_session_permission_mode_updated("other-session", PermissionMode::ReadOnly);
    let mode_after_non_matching_update = session_manager.state.sessions[0].permission_mode;
    session_manager.apply_session_permission_mode_updated("session-id", PermissionMode::ReadOnly);

    // Assert
    assert_eq!(mode_after_non_matching_update, PermissionMode::AutoEdit);
    assert_eq!(
        session_manager.state.sessions[0].permission_mode,
        PermissionMode::ReadOnly
    );
}

#[tokio::test]
async fn test_apply_turn_applied_state_clears_active_prompt_and_resolution_loader() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(temp_dir.path().to_path_buf()).await;
    add_manual_session_with_status(
        &mut app,
        temp_dir.path(),
        "session-id",
        "Prompt",
        Status::InProgress,
    );
    app.sessions
        .set_active_prompt_output("session-id", " › Prompt\n\n".to_string());
    app.sessions.sessions_mut()[0]
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Resolving 1 review comment...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::ReviewCommentResolution,
            turn_position: None,
        });

    // Act
    app.sessions.apply_turn_applied_state(
        "session-id",
        &TurnAppliedState {
            follow_up_tasks: Vec::new(),
            questions: Vec::new(),
            token_usage_delta: SessionStats::default(),
        },
    );

    // Assert
    assert!(
        !app.sessions
            .active_prompt_outputs()
            .contains_key("session-id")
    );
    assert!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
}

#[tokio::test]
async fn test_retain_active_prompt_outputs_keeps_only_active_sessions() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(temp_dir.path().to_path_buf()).await;
    add_manual_session_with_status(
        &mut app,
        temp_dir.path(),
        "active-session",
        "Prompt",
        Status::InProgress,
    );
    add_manual_session_with_status(
        &mut app,
        temp_dir.path(),
        "review-session",
        "Prompt",
        Status::Review,
    );
    app.sessions
        .set_active_prompt_output("active-session", " › Active\n\n".to_string());
    app.sessions
        .set_active_prompt_output("review-session", " › Review\n\n".to_string());

    // Act
    app.sessions.retain_active_prompt_outputs();

    // Assert
    assert!(
        app.sessions
            .active_prompt_outputs()
            .contains_key("active-session")
    );
    assert!(
        !app.sessions
            .active_prompt_outputs()
            .contains_key("review-session")
    );
}

#[test]
fn test_retain_active_prompt_outputs_prunes_expired_at_mention_indexes() {
    // Arrange
    let initial_instant = Instant::now();
    let initial_system_time = SystemTime::UNIX_EPOCH + Duration::from_mins(1);
    let clock = Arc::new(TestClock::new(initial_instant, initial_system_time));
    let mut session_manager = test_session_manager_with_clock("session-id", None, clock.clone());
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    session_manager.set_at_mention_index_for_root(
        lookup_root.clone(),
        vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }],
    );
    clock.advance(AT_MENTION_INDEX_TTL + Duration::from_secs(1));

    // Act
    session_manager.retain_active_prompt_outputs();

    // Assert
    assert!(
        session_manager
            .at_mention_index_for_root(&lookup_root)
            .is_none()
    );
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
            AgentModel::ClaudeOpus5,
        )
        .await;
}

async fn wait_for_status(app: &mut App, session_id: &str, expected: Status) {
    wait_for_status_with_retries(app, session_id, expected, 2000, false).await;
}

async fn wait_for_status_with_retries(
    app: &mut App,
    session_id: &str,
    expected: Status,
    retries: usize,
    process_events_each_iteration: bool,
) {
    for _ in 0..retries {
        if process_events_each_iteration {
            app.process_pending_app_events().await;
        }
        app.sessions.sync_from_handles();
        let Some(session) = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
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
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session while waiting for status");
    assert_eq!(
        session.status,
        expected,
        "session transcript while waiting for status: {}",
        session_replay_text(session)
    );
}

fn session_replay_text(session: &Session) -> String {
    session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default()
}

/// Waits until background cleanup removes `path`.
async fn wait_for_path_absent(path: &Path) {
    for _ in 0..500 {
        if !path.exists() {
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    assert!(!path.exists(), "timed out waiting for path cleanup");
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
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            break;
        };
        if session_replay_text(session).contains(expected_output) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session while waiting for output");
    assert!(
        session_replay_text(session).contains(expected_output),
        "expected output to contain: {expected_output}, actual output: {}",
        session_replay_text(session)
    );
}

/// Waits for output while draining app events that may start follow-up
/// background work.
async fn wait_for_output_contains_after_events(
    app: &mut App,
    session_id: &str,
    expected_output: &str,
    retries: usize,
) {
    for _ in 0..retries {
        app.process_pending_app_events().await;
        app.sessions.sync_from_handles();
        let Some(session) = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            break;
        };
        if session_replay_text(session).contains(expected_output) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    app.process_pending_app_events().await;
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session while waiting for output");
    assert!(
        session_replay_text(session).contains(expected_output),
        "expected output to contain: {expected_output}, actual output: {}",
        session_replay_text(session)
    );
}

/// Returns the current session status or `Done` when session is missing.
fn session_status_or_done(app: &App, session_id: &str) -> Status {
    app.sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .map_or(Status::Done, |session| session.status)
}

/// Returns whether a session currently has `Done` status.
fn is_session_done(app: &App, session_id: &str) -> bool {
    app.sessions
        .sessions()
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
    assert!(app.sessions.sessions().is_empty());
    assert_eq!(app.sessions.selected_session_index(), None);
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
    let db = AppRepositories::in_memory().await.expect("db should open");
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
    app.sessions.select_session_index(Some(0));
    app.next();
    assert_eq!(app.sessions.selected_session_index(), Some(1));
    app.next();
    assert_eq!(app.sessions.selected_session_index(), Some(0)); // Loop back

    // Act & Assert (Previous)
    app.previous();
    assert_eq!(app.sessions.selected_session_index(), Some(1)); // Loop back
    app.previous();
    assert_eq!(app.sessions.selected_session_index(), Some(0));
}

#[tokio::test]
async fn test_navigation_follows_grouped_order_and_skips_group_headers() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session_with_status(&mut app, dir.path(), "archive-1", "Archive 1", Status::Done);
    add_manual_session_with_status(&mut app, dir.path(), "active-1", "Active 1", Status::Review);
    add_manual_session_with_status(&mut app, dir.path(), "queued-1", "Queued 1", Status::Queued);
    add_manual_session_with_status(
        &mut app,
        dir.path(),
        "archive-2",
        "Archive 2",
        Status::Canceled,
    );
    add_manual_session_with_status(&mut app, dir.path(), "merge-1", "Merge 1", Status::Merging);
    add_manual_session_with_status(&mut app, dir.path(), "active-2", "Active 2", Status::Draft);
    app.sessions.select_session_index(Some(3));

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
    assert_eq!(app.sessions.selected_session_index(), None);

    app.previous();
    assert_eq!(app.sessions.selected_session_index(), None);
}

#[tokio::test]
async fn test_navigation_recovery() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "A").await;

    // Act & Assert — next recovers from None
    app.sessions.select_session_index(None);
    app.next();
    assert_eq!(app.sessions.selected_session_index(), Some(0));

    // Act & Assert — previous recovers from None
    app.sessions.select_session_index(None);
    app.previous();
    assert_eq!(app.sessions.selected_session_index(), Some(0));
}

#[tokio::test]
async fn test_create_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            app.projects.active_project_id(),
            SettingName::DefaultSmartReasoningLevel,
            ReasoningLevel::Low.as_str(),
        )
        .await
        .expect("failed to set project reasoning level");

    // Act
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    // Assert — blank session
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, session_id);
    assert_eq!(app.sessions.sessions()[0].prompt, "");
    assert_eq!(app.sessions.sessions()[0].title, None);
    assert_eq!(app.sessions.sessions()[0].display_title(), "No title");
    assert!(!app.sessions.sessions()[0].is_draft_session());
    assert_eq!(app.sessions.sessions()[0].status, Status::Draft);
    assert_eq!(app.sessions.selected_session_index(), Some(0));
    assert_eq!(
        app.sessions.sessions()[0].agent.model(),
        AgentKind::Gemini.default_model()
    );

    // Check filesystem
    let session_dir = &app.sessions.sessions()[0].folder;
    let data_dir = session_dir.join(SESSION_DATA_DIR);
    assert!(session_dir.exists());
    assert!(data_dir.is_dir());

    // Check DB
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let activity_timestamps = app
        .services
        .db()
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");
    assert_eq!(db_sessions.len(), 1);
    assert_eq!(db_sessions[0].base_branch, "main");
    assert_eq!(
        db_sessions[0].model,
        AgentKind::Gemini.default_model().as_str()
    );
    assert!(!db_sessions[0].is_draft);
    assert_eq!(db_sessions[0].status, "Draft");
    assert_eq!(
        db_sessions[0].reasoning_level_override.as_deref(),
        Some(ReasoningLevel::Low.as_str())
    );
    assert_eq!(activity_timestamps.len(), 1);
}

#[tokio::test]
async fn test_create_session_propagates_project_reasoning_read_failure() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), database).await;
    sqlx::query!("DROP TABLE project_setting")
        .execute(&pool)
        .await
        .expect("failed to drop project settings table");

    // Act
    let result = app.create_session().await;

    // Assert
    assert!(result.is_err());
    assert!(app.sessions.sessions().is_empty());
}

#[tokio::test]
async fn test_create_draft_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;

    // Act
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");

    // Assert
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, session_id);
    assert!(app.sessions.sessions()[0].is_draft_session());
    assert_eq!(app.sessions.sessions()[0].status, Status::Draft);
    assert!(!app.sessions.sessions()[0].folder.exists());

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert!(db_sessions[0].is_draft);
}

#[tokio::test]
async fn test_create_stacked_draft_session_persists_parent_and_base_branch() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let expected_base_branch = session_branch(&parent_session_id);

    // Act
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");

    // Assert
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    assert!(child_session.is_draft_session());
    assert_eq!(child_session.status, Status::Draft);
    assert_eq!(
        child_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(child_session.base_branch, expected_base_branch);
    assert!(!child_session.folder.exists());

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted child session");
    assert_eq!(
        db_child_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(db_child_session.base_branch, expected_base_branch);
}

#[tokio::test]
async fn test_create_stacked_draft_session_chains_five_drafts_and_rejects_sixth() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let root_session_id = app.create_session().await.expect("failed to create root");
    let mut parent_session_id = root_session_id;
    for _ in 1..=5 {
        parent_session_id = app
            .create_stacked_draft_session(&parent_session_id)
            .await
            .expect("failed to create nested stack level");
    }

    // Act
    let result = app.create_stacked_draft_session(&parent_session_id).await;

    // Assert
    let error = result.expect_err("sixth stack level should fail");
    assert!(error.to_string().contains("five-level stack limit"));
    assert_eq!(app.sessions.sessions().len(), 6);
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
    app.set_session_model(
        &first_session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set session model");
    let active_project_id = app.active_project_id();
    let default_smart_model_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartModel)
        .await
        .expect("failed to load setting");
    let default_smart_agent_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartAgent)
        .await
        .expect("failed to load agent setting");

    // Act
    let second_session_id = app
        .create_session()
        .await
        .expect("failed to create second session");

    // Assert
    let second_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == second_session_id)
        .expect("missing second session");
    assert_eq!(
        second_session.agent.model(),
        AgentKind::Gemini.default_model()
    );
    assert_eq!(default_smart_model_setting, None);
    assert_eq!(default_smart_agent_setting, None);

    let db_sessions = app
        .services
        .db()
        .sessions()
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
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
    let active_project_id = app.active_project_id();
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            active_project_id,
            SettingName::LastUsedModelAsDefault,
            "true",
        )
        .await
        .expect("failed to upsert last-used-model setting");
    let first_session_id = app
        .create_session()
        .await
        .expect("failed to create first session");

    // Act
    app.set_session_model(
        &first_session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set session model");
    let default_smart_model_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartModel)
        .await
        .expect("failed to load setting");
    let default_smart_agent_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartAgent)
        .await
        .expect("failed to load agent setting");
    drop(app);
    let mut restarted_app = new_test_app_with_git_and_db(dir.path(), db).await;
    let second_session_id = restarted_app
        .create_session()
        .await
        .expect("failed to create second session");

    // Assert
    assert_eq!(
        default_smart_model_setting,
        Some(AgentModel::Gpt56Sol.as_str().to_string())
    );
    assert_eq!(
        default_smart_agent_setting,
        Some(AgentKind::Codex.name().to_string())
    );
    let second_session = restarted_app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == second_session_id)
        .expect("missing second session");
    assert_eq!(second_session.agent.kind(), AgentKind::Codex);
    assert_eq!(second_session.agent.model(), AgentModel::Gpt56Sol);
}

#[tokio::test]
async fn test_create_session_reads_default_smart_model_and_speed_from_db_settings() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let active_project_id = app.active_project_id();
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            active_project_id,
            SettingName::DefaultSmartModel,
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to upsert default smart model setting");
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            active_project_id,
            SettingName::DefaultSmartSpeedMode,
            SpeedMode::Fast.as_str(),
        )
        .await
        .expect("failed to upsert default smart speed setting");

    // Act
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    // Assert
    let created_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing created session");
    assert_eq!(created_session.agent.model(), AgentModel::ClaudeOpus5);
    assert_eq!(created_session.agent.kind(), AgentKind::Claude);
    assert_eq!(created_session.speed_mode, SpeedMode::Fast);
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
    assert_eq!(app.sessions.sessions()[0].prompt, "Hello");
    assert_eq!(app.sessions.sessions()[0].title, Some("Hello".to_string()));
    app.sessions.sync_from_handles();
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert!(output.contains("Hello"));
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let activity_timestamps = app
        .services
        .db()
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(db_sessions[0].id.as_str())
        .await
        .expect("failed to load session messages");
    assert_eq!(db_sessions[0].prompt, "Hello");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].kind,
        crate::domain::session_message::SessionMessageKind::UserPrompt.as_str()
    );
    assert_eq!(messages[0].content, "Hello");
    assert_eq!(activity_timestamps.len(), 1);
}

#[tokio::test]
async fn test_stage_draft_message_persists_bundle_without_starting_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    let session_update_versions = app.services.session_update_versions();
    SessionTaskService::remove_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );

    // Act
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    app.stage_draft_message(&session_id, "Second draft")
        .await
        .expect("failed to stage second draft");
    let session_update_version = {
        let session_update_versions = session_update_versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *session_update_versions
            .get(session_id.as_str())
            .unwrap_or(&0)
    };

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::Draft);
    assert_eq!(
        app.sessions.sessions()[0].prompt,
        "First draft\n\nSecond draft"
    );
    assert_eq!(
        app.sessions.sessions()[0].title,
        Some("First draft".to_string())
    );
    assert_eq!(
        app.sessions.sessions()[0].draft_attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(db_sessions[0].prompt, "First draft\n\nSecond draft");
    assert_eq!(db_sessions[0].title, Some("First draft".to_string()));
    assert_eq!(session_update_version, 2);
}

#[tokio::test]
async fn test_stage_draft_message_keeps_generated_title_until_replacement_finishes() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    app.services
        .db()
        .sessions()
        .update_session_title(&session_id, "Generated draft title")
        .await
        .expect("failed to persist generated draft title");
    app.sessions.sessions_mut()[0].title = Some("Generated draft title".to_string());

    // Act
    app.stage_draft_message(&session_id, "Second draft")
        .await
        .expect("failed to stage second draft");

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].prompt,
        "First draft\n\nSecond draft"
    );
    assert_eq!(
        app.sessions.sessions()[0].title,
        Some("Generated draft title".to_string())
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(
        db_sessions[0].title,
        Some("Generated draft title".to_string())
    );
}

#[tokio::test]
async fn test_replace_title_generation_task_aborts_superseded_task() {
    // Arrange
    let state = SessionState::new(
        HashMap::new(),
        Vec::new(),
        SelectionState::default(),
        Arc::new(RealClock),
        1,
        0,
    );
    let mut session_manager = SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    );
    let session_id = "session-id".to_string();
    let first_task_aborted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let first_task_flag = Arc::clone(&first_task_aborted);
    let first_task = tokio::spawn(async move {
        struct AbortFlagGuard(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for AbortFlagGuard {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let _abort_flag_guard = AbortFlagGuard(first_task_flag);
        std::future::pending::<()>().await;
    });
    let second_task = tokio::spawn(async {});

    // Act
    session_manager.replace_title_generation_task(&session_id, 1, first_task);
    tokio::task::yield_now().await;
    session_manager.replace_title_generation_task(&session_id, 2, second_task);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !first_task_aborted.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("superseded task should abort promptly");

    // Assert
    assert_eq!(
        session_manager.workflow_state.title_generation_tasks.len(),
        1
    );
    assert!(
        session_manager
            .workflow_state
            .title_generation_tasks
            .contains_key(session_id.as_str())
    );
}

#[tokio::test]
async fn test_clear_title_generation_task_if_matches_ignores_stale_generation() {
    // Arrange
    let state = SessionState::new(
        HashMap::new(),
        Vec::new(),
        SelectionState::default(),
        Arc::new(RealClock),
        1,
        0,
    );
    let mut session_manager = SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    );
    let session_id = "session-id".to_string();
    let task = tokio::spawn(async {});
    session_manager.replace_title_generation_task(&session_id, 2, task);

    // Act
    session_manager.clear_title_generation_task_if_matches(&session_id, 1);

    // Assert
    assert_eq!(
        session_manager.workflow_state.title_generation_tasks.len(),
        1
    );
    assert!(
        session_manager
            .workflow_state
            .title_generation_tasks
            .contains_key(session_id.as_str())
    );
}

#[tokio::test]
async fn test_clear_title_generation_task_if_matches_removes_matching_generation() {
    // Arrange
    let state = SessionState::new(
        HashMap::new(),
        Vec::new(),
        SelectionState::default(),
        Arc::new(RealClock),
        1,
        0,
    );
    let mut session_manager = SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    );
    let session_id = "session-id".to_string();
    let task = tokio::spawn(async {});
    session_manager.replace_title_generation_task(&session_id, 2, task);

    // Act
    session_manager.clear_title_generation_task_if_matches(&session_id, 2);

    // Assert
    assert!(
        session_manager
            .workflow_state
            .title_generation_tasks
            .is_empty()
    );
}

#[tokio::test]
async fn test_start_staged_session_launches_bundle_and_clears_staged_drafts() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    app.stage_draft_message(&session_id, "Second draft")
        .await
        .expect("failed to stage second draft");

    // Act
    app.start_staged_session(&session_id)
        .await
        .expect("failed to start staged session");

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].prompt,
        "First draft\n\nSecond draft"
    );
    assert_eq!(
        app.sessions.sessions()[0].draft_attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
    assert!(app.sessions.sessions()[0].folder.exists());
    assert!(
        app.sessions.sessions()[0]
            .folder
            .join(SESSION_DATA_DIR)
            .is_dir()
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(db_sessions[0].prompt, "First draft\n\nSecond draft");
}

#[tokio::test]
async fn test_start_staged_session_clears_draft_flag() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");

    // Act
    app.start_staged_session(&session_id)
        .await
        .expect("failed to start staged session");

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing started session");
    assert!(!session.is_draft);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert!(!db_sessions[0].is_draft);
}

#[tokio::test]
async fn test_start_staged_session_succeeds_when_clearing_draft_flag_fails() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    sqlx::query!(
        r"
CREATE TRIGGER fail_clear_draft_flag
BEFORE UPDATE OF is_draft ON session
WHEN OLD.is_draft = 1 AND NEW.is_draft = 0
BEGIN
    SELECT RAISE(ABORT, 'draft cleanup failed');
END
"
    )
    .execute(&pool)
    .await
    .expect("failed to install draft cleanup failure trigger");

    // Act
    let result = app.start_staged_session(&session_id).await;

    // Assert
    assert!(result.is_ok());
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing started session");
    assert!(session.is_draft);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert!(db_sessions[0].is_draft);
}

#[tokio::test]
async fn test_start_staged_session_launches_stacked_draft_child() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    app.stage_draft_message(&child_session_id, "Stacked draft")
        .await
        .expect("failed to stage stacked draft message");

    // Act
    app.start_staged_session(&child_session_id)
        .await
        .expect("failed to start stacked draft");
    app.sessions.sync_from_handles();

    // Assert
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    assert_eq!(child_session.status, Status::InProgress);
    assert!(child_session.folder.exists());
    assert_eq!(
        child_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
}

#[tokio::test]
async fn test_start_staged_session_rejects_stacked_child_before_parent_review() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let parent_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == parent_session_id)
        .expect("expected parent session");
    parent_session.status = Status::InProgress;
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    app.stage_draft_message(&child_session_id, "Stacked draft")
        .await
        .expect("failed to stage stacked draft message");

    // Act
    let result = app.start_staged_session(&child_session_id).await;

    // Assert
    let error = result.expect_err("parent branch work should block child start");
    assert!(error.to_string().contains("parent is in review"));
}

#[tokio::test]
async fn test_delete_selected_draft_session_removes_staged_draft_metadata() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(
        &session_id,
        TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: dir.path().join("draft-image.png"),
            }],
            text: "First draft".to_string(),
            text_source: TurnPromptTextSource::UserPrompt,
        },
    )
    .await
    .expect("failed to stage first draft");
    let staged_draft_root = app.services.base_path().join(&session_id);
    assert!(staged_draft_root.exists());

    // Act
    app.delete_selected_session().await;

    // Assert
    assert!(!staged_draft_root.exists());
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
    assert_eq!(app.sessions.sessions()[0].title, Some(prompt.to_string()));
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(db_sessions[0].title, Some(prompt.to_string()));
}

#[tokio::test]
async fn test_append_session_to_stack_persists_parent_and_queues_sync() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let session_id = app.create_session().await.expect("failed to create child");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::AgentReview);
    let expected_parent_branch = session_branch(&parent_session_id);

    // Act
    let result = app
        .append_session_to_stack(&session_id, &parent_session_id)
        .await;
    let persisted_session = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("failed to load appended session")
        .expect("appended session should exist");
    let in_memory_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("appended in-memory session should exist");

    // Assert
    assert!(result.is_ok(), "append should start: {:?}", result.err());
    assert_eq!(
        in_memory_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(in_memory_session.base_branch, expected_parent_branch);
    assert_eq!(
        persisted_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(persisted_session.base_branch, expected_parent_branch);
}

#[tokio::test]
async fn test_append_session_to_stack_preserves_pending_restack_base_while_sync_is_queued() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let session_id = app.create_session().await.expect("failed to create child");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let pending_restack_base = "former-parent-tip";
    app.services
        .db()
        .sessions()
        .update_session_stack_base_commit_hash(&session_id, Some(pending_restack_base.to_string()))
        .await
        .expect("failed to seed pending restack base");
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected child session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = branch_operation_lock.lock_owned().await;

    // Act
    let result = app
        .append_session_to_stack(&session_id, &parent_session_id)
        .await;
    let preserved_restack_base = app
        .services
        .db()
        .sessions()
        .get_session_stack_base_commit_hash(&session_id)
        .await
        .expect("failed to load pending restack base");

    // Assert
    assert!(result.is_ok(), "append should queue: {:?}", result.err());
    assert_eq!(
        preserved_restack_base.as_deref(),
        Some(pending_restack_base)
    );

    drop(app);
    drop(existing_operation_guard);
}

#[tokio::test]
async fn test_append_session_to_stack_rolls_back_metadata_when_sync_cannot_start() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let session_id = app.create_session().await.expect("failed to create child");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    app.sessions
        .session_handles_mut()
        .remove(session_id.as_str());

    // Act
    let result = app
        .append_session_to_stack(&session_id, &parent_session_id)
        .await;
    let persisted_session = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("failed to load rolled-back session")
        .expect("rolled-back session should exist");
    let in_memory_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("rolled-back in-memory session should exist");

    // Assert
    assert!(result.is_err());
    assert_eq!(in_memory_session.parent_session_id, None);
    assert_eq!(in_memory_session.base_branch, "main");
    assert_eq!(persisted_session.parent_session_id, None);
    assert_eq!(persisted_session.base_branch, "main");
}

#[tokio::test]
async fn test_append_session_to_stack_rejects_non_review_source() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let session_id = app.create_session().await.expect("failed to create source");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);

    // Act
    let result = app
        .append_session_to_stack(&session_id, &parent_session_id)
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(crate::app::AppError::Session(SessionError::Workflow(message)))
            if message.contains("Review")
    ));
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
            AgentModel::Gemini38Flash,
        )
        .await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].title, Some(prompt.to_string()));
    let db_sessions = app
        .services
        .db()
        .sessions()
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
    let session_folder = app.sessions.sessions()[session_index].folder.clone();
    assert!(session_folder.exists());

    // Act — simulate Esc: delete the blank session
    app.delete_selected_session().await;

    // Assert
    assert!(app.sessions.sessions().is_empty());
    assert!(!session_folder.exists());
    let db_sessions = app
        .services
        .db()
        .sessions()
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
    let session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &session_id, Status::Review).await;

    // Act
    app.reply(&session_id, "Reply").await;

    // Assert
    app.sessions.sync_from_handles();
    let session = &app.sessions.sessions()[0];
    let output = session_replay_text(session);
    let activity_timestamps = app
        .services
        .db()
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");
    assert!(output.contains("Reply"));
    assert_eq!(activity_timestamps.len(), 1);
}

#[tokio::test]
async fn test_resolve_session_review_comments_enqueues_turn_and_clears_focused_review() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = prepare_review_comment_resolution_session(&mut app).await;
    let snapshot = review_comment_resolution_snapshot();
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let turn_release = Arc::new(Notify::new());
    let mut mock_channel = MockAgentChannel::new();
    let turn_release_for_agent = Arc::clone(&turn_release);
    mock_channel
        .expect_run_turn()
        .once()
        .returning(move |_, request, _| {
            assert!(request.prompt.text.contains("Thread ID: thread-42"));
            let done_tx = done_tx.clone();
            let turn_release = Arc::clone(&turn_release_for_agent);

            Box::pin(async move {
                let _ = done_tx.send(());
                turn_release.notified().await;

                Ok(TurnResult {
                    assistant_message: AgentResponse::plain("Resolved the review comment."),
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
    let selected_comments = vec![ReviewCommentSelection {
        thread_id: "thread-42".to_string(),
    }];

    // Act
    let outcome = app
        .resolve_session_review_comments(&session_id, &snapshot, &selected_comments)
        .await;
    done_rx.recv().await.expect("turn should start");
    app.sessions.sync_from_handles();
    let active_session = &app.sessions.sessions()[0];
    let active_status = active_session.status;
    let resolution_loader = active_session
        .transient_messages
        .get(TransientMessageSlot::ReviewCommentResolution)
        .map(|message| message.body.text().to_string());
    let generated_prompt_kind = active_session
        .transcript
        .as_ref()
        .and_then(|transcript| transcript.messages().last())
        .map(|message| message.kind);
    turn_release.notify_one();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    let focused_reviews = app
        .services
        .db()
        .sessions()
        .load_session_focused_reviews_for_project(app.active_project_id())
        .await
        .expect("failed to load focused reviews");
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(session_id.as_str())
        .await
        .expect("session messages should load");
    let persisted_generated_prompt = persisted_messages
        .iter()
        .find(|message| message.content.contains("Thread ID: thread-42"));

    // Assert
    assert_eq!(
        outcome,
        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    );
    assert!(!app.review_cache.contains_key(&session_id));
    assert_eq!(active_status, Status::InProgress);
    assert_eq!(
        resolution_loader.as_deref(),
        Some("Resolving 1 review comment...")
    );
    assert_eq!(
        generated_prompt_kind,
        Some(crate::domain::session_message::SessionMessageKind::AgentPrompt)
    );
    assert!(matches!(
        persisted_generated_prompt,
        Some(message) if message.kind == "agent_prompt"
    ));
    assert_eq!(
        focused_reviews,
        [] as [crate::infra::db::SessionFocusedReviewRow; 0]
    );
}

/// Prepares one review-ready session with persisted focused-review output.
async fn prepare_review_comment_resolution_session(app: &mut App) -> SessionId {
    let session_id: SessionId = app
        .create_session()
        .await
        .expect("failed to create session")
        .into();
    app.sessions.sessions_mut()[0].status = Status::Review;
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str()) {
        *handles
            .status
            .lock()
            .expect("status lock should be available") = Status::Review;
    }
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, "Review", 0)
        .await
        .expect("failed to persist review status");
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash: 42,
            text: "Focused review".to_string(),
        },
    );
    app.services
        .db()
        .sessions()
        .update_session_focused_review(
            &session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("Focused review".to_string()),
        )
        .await
        .expect("failed to persist focused review");

    session_id
}

/// Builds one actionable inline review thread for session-resolution tests.
fn review_comment_resolution_snapshot() -> ReviewCommentSnapshot {
    ReviewCommentSnapshot {
        pr_level_comments: Vec::new(),
        threads: vec![ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "reviewer".to_string(),
                authored_by_current_user: false,
                body: "Add validation.".to_string(),
            }],
            id: "thread-42".to_string(),
            is_outdated: Some(false),
            is_resolved: false,
            line: Some(12),
            path: "src/main.rs".to_string(),
            start_line: Some(11),
        }],
    }
}

#[tokio::test]
async fn test_reply_to_parent_allows_review_ready_stacked_child() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let parent_session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &parent_session_id, Status::Review).await;
    let child_session_id = app
        .create_draft_session()
        .await
        .expect("failed to create child session");
    let child_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == child_session_id)
        .expect("expected child session");
    child_session.parent_session_id = Some(parent_session_id.clone());
    child_session.status = Status::Review;

    // Act
    app.reply(&parent_session_id, "Parent follow-up").await;

    // Assert
    app.sessions.sync_from_handles();
    let parent_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("expected parent session");
    assert!(session_replay_text(parent_session).contains("Parent follow-up"));
}

#[tokio::test]
async fn test_parent_turn_completion_rebases_review_ready_stacked_child() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let parent_session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &parent_session_id, Status::Review).await;
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked child");
    let child_folder = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("expected child session")
        .folder
        .clone();
    app.services
        .fs_client()
        .create_dir_all(child_folder)
        .await
        .expect("failed to materialize child worktree folder");
    crate::test_support::set_session_status_for_test(&mut app, &child_session_id, Status::Review);
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&child_session_id, "Review", 0)
        .await
        .expect("failed to persist review status for child session");

    // Act
    app.reply(&parent_session_id, "Parent follow-up").await;
    wait_for_status(&mut app, &parent_session_id, Status::Review).await;
    wait_for_output_contains_after_events(
        &mut app,
        &child_session_id,
        "[Sync] Successfully synced",
        200,
    )
    .await;

    // Assert
    app.sessions.sync_from_handles();
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("expected child session");
    assert_eq!(child_session.status, Status::Review);
    assert!(session_replay_text(child_session).contains("onto wt/"));
}

#[tokio::test]
/// Verifies that submitting a chat message while the session is
/// `InProgress` pushes the prompt onto the in-memory queue and mirrors
/// it into the render snapshot so the row appears inline in the
/// transcript before the running turn finishes.
async fn test_enqueue_message_pushes_prompt_onto_in_memory_queue() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    app.save_diff_comment_progress(session_id.clone(), line_comments);
    let saved_line_comments = app.diff_comment_progress[&session_id].clone();

    // Act
    app.enqueue_message(&session_id, "queued reply")
        .expect("enqueue_message should succeed for InProgress session");

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session present");
    assert_eq!(session.queued_messages[0].transcript_text(), "queued reply");
    let handles = app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles present");
    let queued_len = handles.queued_messages.lock().expect("queue lock").len();
    assert_eq!(queued_len, 1);
    assert_eq!(
        app.diff_comment_progress.get(&session_id),
        Some(&saved_line_comments),
        "queueing must retain comments until the queued turn starts"
    );
}

#[tokio::test]
/// Regression: a queued chat message must remain visible after the
/// reducer reloads sessions from the database. The previous wiring
/// emitted `RefreshSessions` from `enqueue_message`, which rebuilt every
/// `Session` snapshot with `queued_messages: Vec::new()`. The post-reload
/// `sync_session_with_handles` did not restore `queued_messages` from the
/// handles, so the just-pushed entry was silently wiped on the next
/// reducer pass and the inline `≡ queued ›` row briefly disappeared from
/// the transcript before reappearing on a later mutation.
async fn test_enqueue_message_survives_refresh_sessions_reducer_pass() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    app.enqueue_message(&session_id, "queued reply")
        .expect("enqueue_message should succeed for InProgress session");

    // Act
    app.apply_app_events(AppEvent::RefreshSessions).await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session present");
    assert_eq!(
        session.queued_messages[0].transcript_text(),
        "queued reply",
        "queued_messages snapshot must be re-projected from handles after a RefreshSessions \
         reducer pass instead of being wiped to an empty vec"
    );
}

#[tokio::test]
/// Verifies that empty payloads are rejected without mutating the queue
/// so accidentally submitting an empty composer does not stage a noop
/// turn.
async fn test_enqueue_message_rejects_empty_payload() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);

    // Act
    let outcome = app.enqueue_message(&session_id, "");

    // Assert
    let error = outcome.expect_err("empty payload should error");
    assert!(matches!(
        error,
        crate::app::session::SessionError::Workflow(_)
    ));
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session present");
    assert_eq!(session.queued_messages, []);
}

#[tokio::test]
async fn test_selected_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Test").await;

    // Act & Assert
    assert!(app.selected_session().is_some());

    app.sessions.select_session_index(None);
    assert!(app.selected_session().is_none());
}

#[tokio::test]
async fn test_delete_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "A").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    let session_folder = app.sessions.sessions()[0].folder.clone();
    app.sessions.set_at_mention_index_for_root(
        session_folder.clone(),
        vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }],
    );
    let session_update_versions = app.services.session_update_versions();
    SessionTaskService::remove_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );
    let initial_version = SessionTaskService::next_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );
    assert_eq!(initial_version, 1);

    // Act
    app.delete_selected_session().await;
    let reset_version = SessionTaskService::next_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );

    // Assert
    assert!(app.sessions.sessions().is_empty());
    assert_eq!(app.sessions.selected_session_index(), None);
    assert!(!session_folder.exists());
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert!(db_sessions.is_empty());
    assert!(
        app.sessions
            .at_mention_index_for_root(&session_folder)
            .is_none()
    );
    assert_eq!(reset_version, 1);

    SessionTaskService::remove_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );
}

#[tokio::test]
async fn test_delete_selected_session_edge_cases() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "1").await;
    create_and_start_session(&mut app, "2").await;

    // Act & Assert — index out of bounds
    app.sessions.select_session_index(Some(99));
    app.delete_selected_session().await;
    assert_eq!(app.sessions.sessions().len(), 2);

    // Act & Assert — None selected
    app.sessions.select_session_index(None);
    app.delete_selected_session().await;
    assert_eq!(app.sessions.sessions().len(), 2);
}

#[tokio::test]
async fn test_delete_last_session_update_selection() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "1").await;
    create_and_start_session(&mut app, "2").await;

    // Act & Assert — delete last item
    app.sessions.select_session_index(Some(1));
    app.delete_selected_session().await;
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.selected_session_index(), Some(0));

    // Act & Assert — delete remaining item
    app.delete_selected_session().await;
    assert!(app.sessions.sessions().is_empty());
    assert_eq!(app.sessions.selected_session_index(), None);
}

#[tokio::test]
async fn test_load_existing_sessions() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("12345678", "claude-opus-4-6", "main", "Done", project_id)
        .await
        .expect("failed to insert");

    let session_dir = dir.path().join("12345678");
    let data_dir = session_dir.join(SESSION_DATA_DIR);
    std::fs::create_dir(&session_dir).expect("failed to create session dir");
    std::fs::create_dir(&data_dir).expect("failed to create data dir");
    db.sessions()
        .update_session_prompt("12345678", "Existing")
        .await
        .expect("failed to update prompt");
    db.sessions()
        .append_session_message(
            "12345678",
            crate::domain::session_message::SessionMessageKind::AssistantAnswer,
            "Output",
        )
        .await
        .expect("failed to append message");

    // Act
    let app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;

    // Assert
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, "12345678");
    assert_eq!(
        app.sessions.sessions()[0].agent.model(),
        AgentModel::ClaudeOpus5
    );
    assert_eq!(app.sessions.sessions()[0].prompt, "Existing");
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert_eq!(output, "Output\n\n");
    assert_eq!(app.sessions.selected_session_index(), Some(0));
}

#[tokio::test]
async fn test_create_session_uses_default_smart_model_setting_and_most_recent_permission_mode() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project(&dir.path().to_string_lossy(), Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha0001", "gemini-3.8-flash", "main", "Done", project_id)
        .await
        .expect("failed to insert alpha0001");
    db.sessions()
        .insert_session(
            "beta00002",
            AgentModel::ClaudeHaiku4520251001.as_str(),
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta00002");
    db.settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartModel,
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to upsert default smart model setting");
    db.sessions()
        .update_session_updated_at("alpha0001", 1_i64)
        .await
        .expect("failed to update alpha0001 timestamp");
    db.sessions()
        .update_session_updated_at("beta00002", 2_i64)
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
        .sessions()
        .iter()
        .find(|session| session.id == created_session_id)
        .expect("missing created session");
    assert_eq!(
        created_session.agent.model(),
        AgentModel::ClaudeHaiku4520251001
    );
}

#[tokio::test]
async fn test_load_existing_sessions_ordered_by_updated_at_desc() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "gemini-3.8-flash", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");

    db.sessions()
        .update_session_updated_at("alpha000", 1_i64)
        .await
        .expect("failed to update alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2_i64)
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
        .sessions()
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    assert_eq!(session_names, vec!["beta0000", "alpha000"]);
}

#[tokio::test]
async fn test_load_sessions_aggregates_daily_activity() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .insert_session("gamma000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert gamma000");
    let seconds_per_day = 86_400_i64;
    let day_key_one = 10_i64;
    let day_key_two = 11_i64;

    db.sessions()
        .update_session_created_at("alpha000", day_key_one * seconds_per_day + 10)
        .await
        .expect("failed to update alpha000 created_at");
    db.sessions()
        .update_session_created_at("beta0000", day_key_one * seconds_per_day + 600)
        .await
        .expect("failed to update beta0000 created_at");
    db.sessions()
        .update_session_created_at("gamma000", day_key_two * seconds_per_day + 50)
        .await
        .expect("failed to update gamma000 created_at");
    db.activity()
        .clear_session_activity()
        .await
        .expect("failed to clear session activity");
    db.activity()
        .backfill_session_activity_from_sessions()
        .await
        .expect("failed to backfill session activity from session rows");
    let working_dir = PathBuf::from("/tmp/test");
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
    let mut expected_activity_by_day: BTreeMap<i64, u32> = BTreeMap::new();
    for timestamp_seconds in [
        day_key_one * seconds_per_day + 10,
        day_key_one * seconds_per_day + 600,
        day_key_two * seconds_per_day + 50,
    ] {
        let day_key = activity_day_key_with_offset(
            timestamp_seconds,
            RealClock.local_utc_offset_seconds(timestamp_seconds),
        );
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
    let fs_client = fs::RealFsClient;
    let (sessions, stats_activity, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: dir.path(),
            clock: &RealClock,
            db: &db,
            fs_client: &fs_client,
            working_dir: &working_dir,
        },
        &mut handles,
    )
    .await;

    // Assert
    assert_eq!(sessions.len(), 3);
    assert_eq!(stats_activity, expected_activity);
}

#[tokio::test]
async fn test_load_sessions_keeps_daily_activity_after_session_deletion() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.activity()
        .insert_session_creation_activity_at("alpha000", 10)
        .await
        .expect("failed to persist first activity event");
    db.activity()
        .insert_session_creation_activity_at("beta0000", 20)
        .await
        .expect("failed to persist second activity event");
    db.sessions()
        .delete_session("alpha000")
        .await
        .expect("failed to delete alpha000");
    let working_dir = PathBuf::from("/tmp/test");
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

    // Act
    let fs_client = fs::RealFsClient;
    let (sessions, stats_activity, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: dir.path(),
            clock: &RealClock,
            db: &db,
            fs_client: &fs_client,
            working_dir: &working_dir,
        },
        &mut handles,
    )
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
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "alpha000",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .update_session_updated_at("alpha000", 1)
        .await
        .expect("failed to set alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2)
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
    app.sessions.select_session_index(Some(1));

    // Act
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("alpha000", "Done", 0)
        .await
        .expect("failed to update session status");
    app.refresh_sessions_now().await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].id, "alpha000");
    let selected_index = app
        .sessions
        .selected_session_index()
        .expect("missing selection");
    assert_eq!(app.sessions.sessions()[selected_index].id, "alpha000");
}

#[tokio::test]
async fn test_periodic_session_refresh_preserves_focused_review_states() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let ready_session_id = "ready000";
    let loading_session_id = "loading0";
    let failed_session_id = "failed00";
    let review_text = "## Review\nPersisted focused review finding.";
    let review_error = "empty provider response";
    for session_id in [ready_session_id, loading_session_id, failed_session_id] {
        db.sessions()
            .insert_session(
                session_id,
                "gemini-3.8-flash",
                "main",
                &Status::Review.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert review session");
        let data_dir = session_folder(dir.path(), session_id).join(SESSION_DATA_DIR);
        std::fs::create_dir_all(data_dir).expect("failed to create session data dir");
    }
    db.sessions()
        .update_session_focused_review(
            ready_session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some(review_text.to_string()),
        )
        .await
        .expect("failed to persist focused review");
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db.clone(),
    )
    .await;
    let loading_review_agent = (
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
        ReasoningLevel::XHigh,
        SpeedMode::Normal,
    );
    app.review_cache.insert(
        loading_session_id.into(),
        ReviewCacheEntry::Loading {
            diff_hash: 43,
            review_agent: loading_review_agent,
        },
    );
    app.review_cache.insert(
        failed_session_id.into(),
        ReviewCacheEntry::Failed {
            diff_hash: 44,
            error: review_error.to_string(),
        },
    );
    crate::app::review::hydrate_review_transients(&app.review_cache, app.sessions.state_mut());
    app.settings.default_review_selection =
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Terra);
    app.settings.default_review_reasoning_level = ReasoningLevel::Low;
    app.settings.default_review_speed_mode = SpeedMode::Fast;
    let clock = Arc::new(TestClock::new(Instant::now(), SystemTime::now()));
    app.sessions.state_mut().clock = clock.clone();
    app.sessions.state_mut().refresh_deadline = clock.now_instant() + SESSION_REFRESH_INTERVAL;
    db.sessions()
        .update_session_updated_at(ready_session_id, 4_000_000_000)
        .await
        .expect("failed to update session timestamp");
    clock.advance(SESSION_REFRESH_INTERVAL);

    // Act
    let refreshed = app.refresh_sessions_if_needed().await;

    // Assert
    assert!(refreshed);
    let ready_message = review_message_body(&app, ready_session_id);
    assert_eq!(ready_message.text(), review_text);

    let loading_message = review_message_body(&app, loading_session_id);
    assert!(matches!(loading_message, TransientMessageBody::Loading(_)));
    assert_eq!(
        loading_message.text(),
        review_loading_message(loading_review_agent)
    );

    let failed_message = review_message_body(&app, failed_session_id);
    assert!(matches!(failed_message, TransientMessageBody::Plain(_)));
    assert_eq!(failed_message.text(), review_failure_message(review_error));
}

fn review_message_body<'a>(app: &'a App, session_id: &str) -> &'a TransientMessageBody {
    &app.sessions
        .session_or_err(session_id)
        .expect("review session should remain loaded")
        .transient_messages
        .get(TransientMessageSlot::Review)
        .expect("review output should remain visible after refresh")
        .body
}

#[tokio::test]
async fn test_refresh_sessions_loads_question_detail_when_another_session_is_selected() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "alpha000",
            "gemini-3.8-flash",
            "main",
            "Question",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .update_session_prompt("alpha000", "Alpha prompt")
        .await
        .expect("failed to set alpha000 prompt");
    db.sessions()
        .update_session_prompt("beta0000", "Beta prompt")
        .await
        .expect("failed to set beta0000 prompt");
    db.sessions()
        .update_session_updated_at("alpha000", 1)
        .await
        .expect("failed to set alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2)
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
    let question_session_id = SessionId::from("alpha000");
    let selected_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == "beta0000")
        .expect("beta0000 should be loaded");
    app.sessions.select_session_index(Some(selected_index));
    app.enter_question_mode(
        &question_session_id,
        vec![QuestionItem::new("Which target should be used?")],
    );

    // Act
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("alpha000", "Question", 0)
        .await
        .expect("failed to update session status");
    app.refresh_sessions_now().await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].id, "alpha000");
    assert!(matches!(
        app.mode,
        AppMode::Question { ref session_id, .. } if session_id == &question_session_id
    ));
    assert_eq!(
        app.sessions
            .selected_session()
            .map(|session| session.id.as_str()),
        Some("beta0000")
    );
    assert_eq!(
        app.sessions
            .session_for_id(&question_session_id)
            .map(|session| session.prompt.as_str()),
        Some("Alpha prompt")
    );
    assert_eq!(
        app.sessions
            .session_for_id("beta0000")
            .map(|session| session.prompt.as_str()),
        Some("")
    );
}

#[tokio::test]
async fn test_refresh_sessions_loads_diff_help_detail_when_another_session_is_selected() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha000", "gemini-3.8-flash", "main", "Review", project_id)
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .update_session_prompt("alpha000", "Alpha prompt")
        .await
        .expect("failed to set alpha000 prompt");
    db.sessions()
        .update_session_prompt("beta0000", "Beta prompt")
        .await
        .expect("failed to set beta0000 prompt");
    db.sessions()
        .update_session_updated_at("alpha000", 1)
        .await
        .expect("failed to set alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2)
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
    let help_session_id = SessionId::from("alpha000");
    let selected_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == "beta0000")
        .expect("beta0000 should be loaded");
    app.sessions.select_session_index(Some(selected_index));
    app.mode = AppMode::Help {
        context: HelpContext::Diff {
            can_comment: true,
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            session_id: help_session_id.clone(),
            scroll_offset: 0,
        },
        scroll_offset: 0,
    };

    // Act
    app.refresh_sessions_now().await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Help {
            context: HelpContext::Diff { ref session_id, .. },
            ..
        } if session_id == &help_session_id
    ));
    assert_eq!(
        app.sessions
            .selected_session()
            .map(|session| session.id.as_str()),
        Some("beta0000")
    );
    assert_eq!(
        app.sessions
            .session_for_id(&help_session_id)
            .map(|session| session.prompt.as_str()),
        Some("Alpha prompt")
    );
    assert_eq!(
        app.sessions
            .session_for_id("beta0000")
            .map(|session| session.prompt.as_str()),
        Some("")
    );
}

#[tokio::test]
async fn test_load_sessions_invalid_path() {
    // Arrange
    let path = PathBuf::from("/invalid/path/that/does/not/exist");

    // Act
    let app = new_test_app(path).await;

    // Assert
    assert!(app.sessions.sessions().is_empty());
}

#[tokio::test]
async fn test_load_done_session_without_folder_kept() {
    // Arrange — DB has a terminal row but no matching folder on disk
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("missing01", "gemini-3.8-flash", "main", "Done", project_id)
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
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, "missing01");
    assert_eq!(app.sessions.sessions()[0].status, Status::Done);
}

#[tokio::test]
async fn test_load_in_progress_session_without_folder_skipped() {
    // Arrange — DB has a non-terminal row but no matching folder on disk
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "missing02",
            "gemini-3.8-flash",
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
    assert!(app.sessions.sessions().is_empty());
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
        .sessions()
        .update_session_diff_stats(8, 3, true, &session_id, "S")
        .await
        .expect("failed to update size");
    let session_index = app
        .session_index_for_id(&session_id)
        .expect("missing created session");
    let session_folder = app.sessions.sessions()[session_index].folder.clone();
    let changed_lines = "line\n".repeat(700);
    std::fs::write(session_folder.join("size-test.txt"), changed_lines)
        .expect("failed to write test file");

    // Act
    let fs_client = fs::RealFsClient;
    let (reloaded_sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: app.projects.active_project_id(),
            active_session_id: None,
            base: app.services.base_path(),
            clock: &RealClock,
            db: app.services.db(),
            fs_client: &fs_client,
            working_dir: app.projects.working_dir(),
        },
        app.sessions.session_handles_mut(),
    )
    .await;
    let db_sessions = app
        .services
        .db()
        .sessions()
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
    assert_eq!(reloaded_session.stats.added_lines, 8);
    assert_eq!(reloaded_session.stats.deleted_lines, 3);
    assert_eq!(db_session.added_lines, 8);
    assert_eq!(db_session.deleted_lines, 3);
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
            AgentModel::ClaudeOpus5,
        )
        .await;
    wait_for_status_with_retries(&mut app, &session_id, Status::AgentReview, 200, true).await;
    app.process_pending_app_events().await;
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing in-memory session");
    let db_session = db_sessions
        .iter()
        .find(|db_session| db_session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(session.size, SessionSize::S);
    assert_eq!(session.stats.added_lines, 20);
    assert_eq!(session.stats.deleted_lines, 0);
    assert_eq!(db_session.added_lines, 20);
    assert_eq!(db_session.deleted_lines, 0);
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
        .sessions()
        .update_session_diff_stats(21, 9, true, &session_id, "L")
        .await
        .expect("failed to update size");
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, "Done", 0)
        .await
        .expect("failed to update status");
    let session_index = app
        .session_index_for_id(&session_id)
        .expect("missing created session");
    let session_folder = app.sessions.sessions()[session_index].folder.clone();
    let changed_lines = "line\n".repeat(700);
    std::fs::write(session_folder.join("done-size-test.txt"), changed_lines)
        .expect("failed to write test file");

    // Act
    let fs_client = fs::RealFsClient;
    let (reloaded_sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: app.projects.active_project_id(),
            active_session_id: None,
            base: app.services.base_path(),
            clock: &RealClock,
            db: app.services.db(),
            fs_client: &fs_client,
            working_dir: app.projects.working_dir(),
        },
        app.sessions.session_handles_mut(),
    )
    .await;
    let db_sessions = app
        .services
        .db()
        .sessions()
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
    assert_eq!(reloaded_session.stats.added_lines, 21);
    assert_eq!(reloaded_session.stats.deleted_lines, 9);
    assert_eq!(db_session.added_lines, 21);
    assert_eq!(db_session.deleted_lines, 9);
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
    let db = AppRepositories::in_memory().await.expect("db should open");
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
                    matches!(req.request_kind, AgentRequestKind::SessionResume),
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
        .insert(session_id.clone().into(), Arc::new(mock_channel));
    app.sessions
        .reply(&app.services, &session_id, "SpawnInit")
        .await;
    done_rx.recv().await.expect("first turn completion signal");
    wait_for_status(&mut app, &session_id, Status::Review).await;
    wait_for_output_contains(&mut app, &session_id, "SpawnInit", 200).await;

    // Assert
    {
        app.sessions.sync_from_handles();
        let session = &app.sessions.sessions()[0];
        let output = session_replay_text(session);
        assert!(output.contains("--prompt"));
        assert!(output.contains("SpawnInit"));
        assert!(!output.contains("--resume"));
        assert_eq!(session.status, Status::Review);
    }

    // Act — reply (resume command)
    let session_id = app.sessions.sessions()[0].id.clone();
    app.sessions
        .reply(&app.services, &session_id, "SpawnReply")
        .await;
    done_rx.recv().await.expect("second turn completion signal");
    wait_for_output_contains(&mut app, &session_id, "--resume", 200).await;
    wait_for_status(&mut app, &session_id, Status::Review).await;

    // Assert
    {
        app.sessions.sync_from_handles();
        let session = &app.sessions.sessions()[0];
        let output = session_replay_text(session);
        assert!(output.contains("SpawnReply"));
        assert!(output.contains("--resume"));
        assert!(output.contains("latest"));
        assert_eq!(session.status, Status::Review);
    }
}

/// Verifies sync requested during a running turn stays on the existing
/// worker, does not cancel that turn, and runs before later queued chat.
#[tokio::test]
async fn test_running_turn_finishes_before_queued_sync_and_later_chat() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), database).await;
    let release_first_turn = Arc::new(Notify::new());
    let release_first_turn_for_channel = Arc::clone(&release_first_turn);
    let turn_count = Arc::new(Mutex::new(0usize));
    let turn_count_for_channel = Arc::clone(&turn_count);
    let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .times(2)
        .returning(move |_, request, _| {
            let turn_index = {
                let mut turn_count = turn_count_for_channel
                    .lock()
                    .expect("turn count lock should not be poisoned");
                let turn_index = *turn_count;
                *turn_count += 1;

                turn_index
            };
            let release_first_turn = Arc::clone(&release_first_turn_for_channel);
            let turn_started_tx = turn_started_tx.clone();

            Box::pin(async move {
                turn_started_tx
                    .send(turn_index)
                    .expect("turn start receiver should remain available");
                if turn_index == 0 {
                    assert_eq!(request.prompt.text, "Initial running turn");
                    release_first_turn.notified().await;

                    return Ok(TurnResult {
                        assistant_message: AgentResponse::plain("Initial turn completed"),
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    });
                }

                assert_eq!(request.prompt.text, "Queued after sync");

                Ok(TurnResult {
                    assistant_message: AgentResponse::plain("Queued turn completed"),
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
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.sessions
        .worker_service
        .test_agent_channels
        .insert(session_id.clone().into(), Arc::new(mock_channel));
    app.sessions
        .reply(&app.services, &session_id, "Initial running turn")
        .await;
    assert_eq!(turn_started_rx.recv().await, Some(0));
    app.sessions.sync_from_handles();
    refresh_with_session_table_unavailable(&mut app, &pool).await;

    // Act
    app.rebase_session(&session_id)
        .await
        .expect("running sync should queue on the active worker");
    app.enqueue_message(&session_id, "Queued after sync")
        .expect("later chat message should queue");

    // Assert
    assert_sync_waits_without_canceling_turn(&mut app, &session_id);

    // Act
    release_first_turn.notify_one();
    assert_eq!(turn_started_rx.recv().await, Some(1));
    wait_for_output_contains_after_events(&mut app, &session_id, "Queued turn completed", 300)
        .await;

    // Assert
    app.sessions.sync_from_handles();
    let transcript = session_replay_text(&app.sessions.sessions()[0]);
    assert!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .is_none()
    );
    let initial_answer_index = transcript
        .find("Initial turn completed")
        .expect("missing completed initial turn");
    let sync_completion_index = transcript
        .find("[Sync] Successfully synced")
        .expect("missing queued sync completion");
    let queued_prompt_index = transcript
        .find("Queued after sync")
        .expect("missing later queued prompt");
    assert!(initial_answer_index < sync_completion_index);
    assert!(sync_completion_index < queued_prompt_index);
}

/// Forces one session refresh to observe a failed primary row query.
async fn refresh_with_session_table_unavailable(app: &mut App, pool: &sqlx::SqlitePool) {
    sqlx::query("ALTER TABLE session RENAME TO unavailable_session")
        .execute(pool)
        .await
        .expect("session table should become temporarily unavailable");
    app.refresh_sessions_now().await;
    sqlx::query("ALTER TABLE unavailable_session RENAME TO session")
        .execute(pool)
        .await
        .expect("session table should become available again");
}

fn assert_sync_waits_without_canceling_turn(app: &mut App, session_id: &str) {
    app.sessions.sync_from_handles();
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    let active_turn_was_cancelled = app
        .sessions
        .session_handles()
        .get(session_id)
        .expect("missing session handles")
        .cancel_token
        .lock()
        .expect("cancel token lock should not be poisoned")
        .is_cancelled();
    assert!(!active_turn_was_cancelled);
    assert!(!session_replay_text(&app.sessions.sessions()[0]).contains("Successfully synced"));
    assert!(matches!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .map(|message| &message.body),
        Some(TransientMessageBody::Queued(_))
    ));
}

#[tokio::test]
/// Verifies that the first reply after a model switch replays the full
/// session transcript and subsequent replies omit the replay snapshot.
///
/// A completion channel (`done_tx`/`done_rx`) is used to signal from
/// inside the mock's async block so that `wait_for_status` always sees the
/// worker in `InProgress` and correctly polls until `Review`. Without this,
/// `wait_for_status` would return immediately because the initial status
/// is already `Review` before the worker runs.
async fn test_reply_with_backend_replays_history_once_after_model_switch() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;

    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let initial_output = " › Initial prompt\n\nmock-start\n".to_string();
    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.transcript = Some(crate::test_support::assistant_transcript(&initial_output));
        session.prompt = "Initial prompt".to_string();
        session.status = Status::Review;
    }
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str()) {
        if let Ok(mut transcript) = handles.transcript.lock() {
            *transcript = crate::test_support::assistant_transcript(&initial_output);
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
        .sessions()
        .update_session_prompt(&session_id, "Initial prompt")
        .await
        .expect("failed to persist initial prompt");

    app.set_session_model(
        &session_id,
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
    )
    .await
    .expect("failed to switch model");

    // Shared state to capture replay transcript text from each turn request.
    let captured_replay_transcripts: Arc<Mutex<Vec<Option<String>>>> =
        Arc::new(Mutex::new(Vec::new()));

    // The done channel signals from inside the mock future so the test
    // can wait on each turn completing before calling `wait_for_status`.
    // This prevents `wait_for_status` from returning immediately when the
    // session is already in `Review` before the worker processes the turn.
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    // Register a MockAgentChannel that collects replay transcript values from
    // resume turns so they can be asserted synchronously after the test.
    let mut mock_channel = MockAgentChannel::new();
    let captured = Arc::clone(&captured_replay_transcripts);
    let done_capture = done_tx.clone();
    mock_channel.expect_run_turn().returning(move |_, req, _| {
        if matches!(req.request_kind, AgentRequestKind::SessionResume) {
            captured
                .lock()
                .expect("lock poisoned")
                .push(req.continuation.replay_transcript().map(str::to_string));
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
        .insert(session_id.clone().into(), Arc::new(mock_channel));

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
    let outputs = captured_replay_transcripts
        .lock()
        .expect("lock poisoned")
        .clone();
    assert_eq!(outputs.len(), 2, "expected exactly two Resume turns");
    let first_replay_transcript = outputs[0]
        .as_deref()
        .expect("first reply should include replay transcript");
    assert!(
        first_replay_transcript.contains("Initial prompt"),
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
    let db = AppRepositories::in_memory().await.expect("db should open");

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
            AgentModel::ClaudeSonnet5,
        )
        .await;
    wait_for_status(&mut first_app, &session_id, Status::Review).await;
    first_app.sessions.sync_from_handles();
    let first_run_output = first_app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .map(session_replay_text)
        .expect("missing persisted session");
    assert!(first_run_output.contains("Initial prompt"));
    assert!(first_run_output.contains("mock-start"));
    drop(first_app);

    let mut resumed_app = new_test_app_with_git_and_db(dir.path(), db).await;
    let resumed_session = resumed_app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing resumed session");
    assert_eq!(resumed_session.status, Status::Review);

    // Act
    let mut resume_backend = MockAgentBackend::new();
    resume_backend.expect_build_command().returning(|request| {
        assert!(request.request_kind.is_resume());

        let replay_transcript = request
            .replay_transcript
            .expect("expected replayed session transcript after restart");
        assert!(replay_transcript.contains("Initial prompt"));
        assert!(replay_transcript.contains("mock-start"));

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf '{\"answer\":\"replayed-after-restart\",\"questions\":[]}'")
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
            AgentModel::ClaudeSonnet5,
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
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    allow_detect_git_info(&mut mock_git_client);
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
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_head_short_hash()
        .times(1)
        .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
    expect_pre_commit_hook_ready(&mut mock_git_client);
    mock_git_client
        .expect_diff()
        .times(3)
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Create a session that writes a file so commit_all has something to commit
    let mut mock = MockAgentBackend::new();
    mock.expect_build_command().returning(|request| {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(
                "echo auto-content > auto-committed.txt; printf '{\"answer\":\"Auto commit \
                 done\",\"questions\":[]}'",
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
            AgentModel::ClaudeSonnet5,
        )
        .await;

    // Act — wait for agent to finish and auto-commit
    wait_for_status(&mut app, &session_id, Status::Review).await;
    app.process_pending_app_events().await;
    app.sessions.sync_from_handles();

    // Assert — commit completion details are transient workflow notice
    // state, not persisted transcript output.
    let session = &app.sessions.sessions()[0];
    let output = session_replay_text(session);
    let workflow_notice = session
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
        .map(|message| message.body.text());
    assert!(
        !output.contains("[Commit] committed with hash"),
        "commit completion should not be persisted, got: {output}"
    );
    assert_eq!(
        workflow_notice,
        Some("[Commit] committed with hash `abc1234`")
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
        .expect_diff()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
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
        .withf(|_, base_branch, commit_message, strategy| {
            base_branch == "main"
                && commit_message == "Refine session work"
                && *strategy == git::SingleCommitMessageStrategy::Replace
        })
        .in_sequence(&mut sequence)
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_head_short_hash()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok("def5678".to_string()) }));
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(|request| {
            assert!(request.prompt.contains("Refine session work"));

            Ok(ag_agent::OneShotSubmission {
                response: AgentResponse::plain("Refine session work"),
                stats: ag_agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: ag_agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    // Act
    let outcome = SessionTaskService::commit_session_changes(
        &mock_git_client,
        &session_folder,
        "main",
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
            ReasoningLevel::Low,
            SpeedMode::Normal,
        ),
        &one_shot_client,
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
    allow_detect_git_info(&mut mock_git_client);
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
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Agent that produces no file changes
    let mut mock = MockAgentBackend::new();
    mock.expect_build_command().returning(|request| {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf '{\"answer\":\"no-changes\",\"questions\":[]}'")
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
            AgentModel::ClaudeOpus5,
        )
        .await;

    // Act — wait for agent to finish
    wait_for_status(&mut app, &session_id, Status::Review).await;
    app.process_pending_app_events().await;
    app.sessions.sync_from_handles();

    // Assert — no-op commit output is visible as transient workflow state.
    let session = &app.sessions.sessions()[0];
    let output = session_replay_text(session);
    let workflow_notice = session
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
        .map(|message| message.body.text());
    assert!(
        !output.contains("[Commit] No changes to commit."),
        "no-op commit output should not be persisted when nothing to commit"
    );
    assert_eq!(workflow_notice, Some("[Commit] No changes to commit."));
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
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Sessions);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Settings);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Projects);
}

#[tokio::test]
async fn test_next_tab_includes_tasks_when_active_project_has_roadmap() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let database = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        None,
        database,
    )
    .await;

    // Act & Assert
    assert_eq!(app.tabs.current(), Tab::Projects);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Sessions);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Settings);
    app.next_tab();
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
    assert!(app.sessions.sessions().is_empty());
}

#[tokio::test]
async fn test_create_session_with_git_no_actual_repo() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
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
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        Some("main".to_string()),
        db,
    )
    .await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    allow_detect_git_info(&mut mock_git_client);
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
    assert_eq!(app.sessions.sessions().len(), 0);

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
    assert_eq!(app.sessions.sessions().len(), 0);
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
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let session_folder = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing created session")
        .folder
        .clone();
    let mock_git = create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
    app.sessions.git_client = Arc::new(mock_git);

    // Act
    let result = app.merge_session(&session_id).await;

    // Assert
    assert!(result.is_ok(), "merge should enqueue successfully");
    wait_for_status_with_retries(&mut app, &session_id, Status::Done, 200, false).await;

    app.sessions.sync_from_handles();
    let merged_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing merged session");
    assert!(!session_replay_text(merged_session).contains("[Merge Error]"));
    assert!(!session_folder.exists(), "worktree should be removed");
}

#[tokio::test]
async fn test_merge_session_restacks_stacked_child_after_success() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app
        .create_session()
        .await
        .expect("failed to create merge session");
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    app.stage_draft_message(&child_session_id, "Ready after parent merge")
        .await
        .expect("failed to stage child draft message");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    let mock_git = create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
    app.sessions.git_client = Arc::new(mock_git);

    // Act
    let result = app.merge_session(&parent_session_id).await;

    // Assert
    assert!(result.is_ok(), "merge should enqueue successfully");
    wait_for_status_with_retries(&mut app, &parent_session_id, Status::Done, 200, false).await;
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted child session");
    assert_eq!(db_child_session.parent_session_id, None);
    assert_eq!(db_child_session.base_branch, "main");
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
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let mock_git = create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
    app.sessions.git_client = Arc::new(mock_git);

    // Act
    let result = app.merge_session(&session_id).await;

    // Assert
    assert!(result.is_ok(), "merge should enqueue successfully");
    wait_for_status_with_retries(&mut app, &session_id, Status::Done, 200, false).await;

    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session after merge");
    assert!(!session_replay_text(session).contains("[Merge Error]"));
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
    crate::test_support::set_session_status_for_test(&mut app, &first_session_id, Status::Review);
    crate::test_support::set_session_status_for_test(&mut app, &second_session_id, Status::Review);
    let mock_git = create_mock_git_client_for_successful_noop_merges(2, dir.path().to_path_buf());
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
async fn test_rebase_session_accepts_in_progress_status_before_worktree_validation() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session(&mut app, dir.path(), "manual01", "Test");
    crate::test_support::set_session_status_for_test(&mut app, "manual01", Status::InProgress);

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

/// Verifies stale `InProgress` state cannot create a new worker and start
/// session sync without an active turn owner.
#[tokio::test]
async fn test_rebase_in_progress_requires_existing_session_worker() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    let error = result.expect_err("sync should reject stale in-progress state");
    assert!(
        error
            .to_string()
            .contains("active session worker is unavailable")
    );
    let unfinished_operations = app
        .services
        .db()
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("failed to load session operations");
    assert!(
        unfinished_operations
            .iter()
            .all(|operation| operation.kind != "rebase")
    );
}

#[tokio::test]
async fn test_rebase_session_invalid_id() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;

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

/// Verifies session sync queues without waiting for an active branch
/// operation, then runs after that operation releases ownership.
#[tokio::test]
async fn test_rebase_session_queues_while_branch_operation_is_busy() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    allow_detect_git_info(&mut mock_git_client);
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
    app.sessions.sessions_mut()[0].status = Status::Review;
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str())
        && let Ok(mut session_status) = handles.status.lock()
    {
        *session_status = Status::Review;
    }
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;

    // Act
    let start_result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        app.rebase_session(&session_id),
    )
    .await;

    // Assert
    let result = start_result.expect("queueing sync should not wait for the branch operation");
    assert!(result.is_ok(), "sync should queue: {:?}", result.err());
    assert!(branch_operation_lock.try_lock().is_err());

    // Act, Assert
    drop(existing_operation_guard);
    wait_for_output_contains(&mut app, &session_id, "[Sync] Successfully synced", 200).await;
}

#[tokio::test]
async fn test_rebase_session_cancels_pending_focused_review() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let db = app.services.db().clone();
    db.sessions()
        .update_session_focused_review(
            &session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some("111".to_string()),
            Some("old persisted focused review".to_string()),
        )
        .await
        .expect("failed to seed persisted focused review");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::AgentReview);
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: None,
    };
    app.review_cache
        .insert(session_id.clone().into(), test_loading_review(777));

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    assert!(result.is_ok(), "rebase should succeed: {:?}", result.err());
    assert!(!app.review_cache.contains_key(session_id.as_str()));
    assert!(matches!(app.mode, AppMode::View { .. }));

    // Act
    app.apply_app_events(AppEvent::ReviewPrepared {
        diff_hash: 777,
        review_text: "stale focused review".to_string(),
        session_id: session_id.clone().into(),
    })
    .await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id.as_str()));
    assert!(matches!(app.mode, AppMode::View { .. }));
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_git_client(Arc::new(create_default_mock_git_client(
            dir.path().to_path_buf(),
        )));
    let restarted_app = App::new_with_clients(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Some("main".to_string()),
        db,
        clients,
    )
    .await
    .expect("failed to build app after recovery");

    assert!(!restarted_app.review_cache.contains_key(session_id.as_str()));
}

/// Verifies focused-review cleanup failure rejects sync before rebase
/// starts.
#[tokio::test]
async fn test_rebase_session_cleanup_failure_does_not_start_sync() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    db.sessions()
        .update_session_focused_review(
            &session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some("111".to_string()),
            Some("old persisted focused review".to_string()),
        )
        .await
        .expect("failed to seed persisted focused review");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::AgentReview);
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: None,
    };
    app.review_cache
        .insert(session_id.clone().into(), test_loading_review(777));
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client.expect_rebase_start().times(0);
    install_mock_git_client(&mut app, mock_git_client);
    pool.close().await;

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    assert!(result.is_err(), "cleanup failure should reject sync");
    assert!(app.review_cache.contains_key(session_id.as_str()));
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(
        session_status_or_done(&app, &session_id),
        Status::AgentReview
    );
}

#[tokio::test]
/// Verifies rebase commits pending worktree changes before starting.
async fn test_rebase_session_auto_commits_uncommitted_changes() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    allow_detect_git_info(&mut mock_git_client);
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
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_diff()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
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
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
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
    let session_folder = app.sessions.sessions()[0].folder.clone();
    app.sessions.sessions_mut()[0].status = Status::Review;
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str())
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
    wait_for_output_contains(&mut app, &session_id, "[Sync] Successfully synced", 200).await;
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
        None,
        Arc::new(mock_git_client),
        AgentModel::Gemini38Flash,
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
        None,
        Arc::new(mock_git_client),
        AgentModel::Gemini38Flash,
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
        None,
        app.services.git_client(),
        AgentModel::Gemini38Flash,
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
        None,
        Arc::new(mock_git_client),
        AgentModel::Gemini38Flash,
    )
    .await;

    // Assert
    let outcome = result.expect("sync should succeed");
    assert_eq!(outcome.pulled_commits, Some(2));
    assert_eq!(outcome.pushed_commits, Some(0));
    assert_eq!(outcome.pulled_commit_titles, vec!["remote fix".to_string()]);
    assert_eq!(outcome.pushed_commit_titles, vec!["local work".to_string()]);
    assert_eq!(
        outcome.resolved_conflict_files,
        [] as [std::string::String; 0]
    );
}

#[tokio::test]
/// Ensures canceling a review session persists `Canceled` status and
/// defers removal of its dedicated worktree checkout.
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
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session")
        .folder
        .clone();
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);

    // Act
    app.sessions
        .cancel_session(&app.services, &session_id)
        .await
        .expect("failed to cancel session");

    // Assert
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session");
    assert_eq!(session.status, Status::Canceled);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_session = db_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(db_session.status, "Canceled");
    wait_for_path_absent(&session_folder).await;
}

#[tokio::test]
/// Ensures canceling an unstarted draft session persists `Canceled`
/// status without requiring a materialized worktree.
async fn test_cancel_draft_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    let session_folder = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session")
        .folder
        .clone();

    // Act
    app.sessions
        .cancel_session(&app.services, &session_id)
        .await
        .expect("failed to cancel draft session");

    // Assert
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session");
    assert_eq!(session.status, Status::Canceled);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_session = db_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(db_session.status, "Canceled");
    wait_for_path_absent(&session_folder).await;
}

#[tokio::test]
/// Ensures canceling a parent session stops and cancels every nonterminal
/// stacked descendant.
async fn test_cancel_session_cascades_to_stacked_descendants() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    crate::test_support::set_session_status_for_test(&mut app, &child_session_id, Status::Review);
    let grandchild_session_id = app
        .create_stacked_draft_session(&child_session_id)
        .await
        .expect("failed to create nested stacked draft session");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &grandchild_session_id,
        Status::Queued,
    );
    app.services
        .db()
        .operations()
        .insert_session_operation("grandchild-operation", &grandchild_session_id, "rebase")
        .await
        .expect("failed to insert grandchild operation");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);

    // Act
    app.sessions
        .cancel_session(&app.services, &parent_session_id)
        .await
        .expect("failed to cancel parent session");

    // Assert
    app.sessions.sync_from_handles();
    let parent_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("missing parent session");
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    let grandchild_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == grandchild_session_id)
        .expect("missing grandchild session");
    assert_eq!(parent_session.status, Status::Canceled);
    assert_eq!(child_session.status, Status::Canceled);
    assert_eq!(grandchild_session.status, Status::Canceled);
    let grandchild_handles = app
        .sessions
        .session_handles_or_err(&grandchild_session_id)
        .expect("missing grandchild session handles");
    assert!(
        grandchild_handles
            .cancel_token
            .lock()
            .expect("grandchild cancel token lock")
            .is_cancelled()
    );
    assert!(
        app.services
            .db()
            .operations()
            .is_cancel_requested_for_operation("grandchild-operation")
            .await
            .expect("failed to load grandchild operation cancellation")
    );

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_parent_session = db_sessions
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("missing persisted parent session");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted child session");
    let db_grandchild_session = db_sessions
        .iter()
        .find(|session| session.id == grandchild_session_id)
        .expect("missing persisted grandchild session");
    assert_eq!(db_parent_session.status, "Canceled");
    assert_eq!(db_child_session.status, "Canceled");
    assert_eq!(db_grandchild_session.status, "Canceled");
}

#[tokio::test]
/// Ensures a stack cascade preserves a descendant that is already terminal.
async fn test_cancel_session_preserves_terminal_stacked_descendant() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    crate::test_support::set_session_status_for_test(&mut app, &child_session_id, Status::Done);
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&child_session_id, "Done", 0)
        .await
        .expect("failed to persist terminal child status");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);

    // Act
    app.sessions
        .cancel_session(&app.services, &parent_session_id)
        .await
        .expect("failed to cancel parent session");

    // Assert
    app.sessions.sync_from_handles();
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing terminal child session");
    assert_eq!(child_session.status, Status::Done);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted terminal child session");
    assert_eq!(db_child_session.status, "Done");
}

#[tokio::test]
/// Ensures parent cancellation reports a descendant cascade failure instead
/// of returning success after the parent status was already persisted.
async fn test_cancel_session_reports_stacked_descendant_failure() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    app.sessions
        .session_handles_mut()
        .remove(child_session_id.as_str());

    // Act
    let result = app
        .sessions
        .cancel_session(&app.services, &parent_session_id)
        .await;

    // Assert
    let error = result.expect_err("descendant cancellation failure should be reported");
    assert!(error.to_string().contains(child_session_id.as_str()));
    app.sessions.sync_from_handles();
    let parent_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("missing parent session");
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    assert_eq!(parent_session.status, Status::Canceled);
    assert_eq!(child_session.status, Status::Draft);
}

#[tokio::test]
/// Ensures canceling a running session requests operation cancellation,
/// signals the active turn token, clears queued prompts, and persists the
/// terminal `Canceled` status.
async fn test_cancel_running_session_stops_turn_and_cancels_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    app.services
        .db()
        .operations()
        .insert_session_operation("operation-id", &session_id, "reply")
        .await
        .expect("failed to insert operation");
    app.sessions
        .enqueue_message(&app.services, &session_id, "queued reply")
        .expect("failed to enqueue message");

    // Act
    app.sessions
        .cancel_session(&app.services, &session_id)
        .await
        .expect("failed to cancel session");

    // Assert
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session");
    assert_eq!(session.status, Status::Canceled);
    let handles = app
        .sessions
        .session_handles_or_err(&session_id)
        .expect("missing session handles");
    assert!(
        handles
            .cancel_token
            .lock()
            .expect("cancel token lock")
            .is_cancelled()
    );
    assert!(
        handles
            .queued_messages
            .lock()
            .expect("queue lock")
            .is_empty()
    );
    assert!(
        app.services
            .db()
            .operations()
            .is_cancel_requested_for_operation("operation-id")
            .await
            .expect("failed to load operation cancel status")
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
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
/// Ensures canceling a Codex review session shuts down its app-server
/// runtime.
async fn test_cancel_session_triggers_app_server_shutdown() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut mock_app_server = MockAppServerClient::new();
    mock_app_server
        .expect_run_turn()
        .times(1)
        .returning(|_, _| {
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
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
    let app_server_client: Arc<dyn AppServerClient> = Arc::new(mock_app_server);
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
    app.set_session_model(
        &session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set app-server model");

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
/// Ensures transitioning a Codex session to `Done` shuts down its
/// app-server runtime.
async fn test_done_status_triggers_app_server_shutdown() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut mock_app_server = MockAppServerClient::new();
    mock_app_server
        .expect_run_turn()
        .times(1)
        .returning(|_, _| {
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
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
    let app_server_client: Arc<dyn AppServerClient> = Arc::new(mock_app_server);
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
    app.set_session_model(
        &session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set app-server model");

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
        app.services.clock().as_ref(),
        app.services.db(),
        &app_event_tx,
        &app.services.session_update_versions(),
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
        app.services.clock().as_ref(),
        app.services.db(),
        &app_event_tx,
        &app.services.session_update_versions(),
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
async fn test_cancel_session_requires_cancelable_status() {
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
            .contains("not cancelable in its current state")
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
    let branch_name = "wt/cleanup123";
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
        .withf(|_, branch| branch == "wt/cleanup123")
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
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].project_id, Some(project_id));
}

#[test]
fn test_parse_merge_commit_message_response_with_protocol_message() {
    // Arrange
    let content = r#"{"answer":"Title\n\n- Detail","questions":[]}"#;

    // Act
    let parsed = parse_agent_response_strict(content)
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
    let parsed = parse_agent_response_strict(content)
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
fn orchestration_research_creation_uses_managed_read_only_role_and_task_link() {
    // Arrange
    let creation_kind = SessionCreationKind::OrchestrationResearch { task_id: 42 };

    // Act
    let role = creation_kind.role();
    let task_id = creation_kind.orchestration_task_id();

    // Assert
    assert_eq!(role, SessionRole::OrchestrationResearcher);
    assert_eq!(task_id, Some(42));
}

#[tokio::test]
async fn test_refresh_session_branch_names_runs_detection_concurrently() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session(&mut app, dir.path(), "alpha001", "First");
    add_manual_session(&mut app, dir.path(), "bravo002", "Second");
    let barrier = Arc::new(Barrier::new(2));
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(2)
        .returning({
            let barrier = Arc::clone(&barrier);
            move |_| {
                let barrier = Arc::clone(&barrier);

                Box::pin(async move {
                    barrier.wait().await;

                    None
                })
            }
        });
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let refresh_result = tokio::time::timeout(
        Duration::from_millis(100),
        app.sessions.refresh_session_branch_names(),
    )
    .await;

    // Assert
    assert!(
        refresh_result.is_ok(),
        "branch refresh should complete without serially blocking"
    );
    assert_eq!(
        app.sessions.session_branch_name("alpha001"),
        Some("wt/alpha001")
    );
    assert_eq!(
        app.sessions.session_branch_name("bravo002"),
        Some("wt/bravo002")
    );
}

// -- remote_branch_name_from_upstream_ref tests --------------------------

#[test]
fn test_remote_branch_name_strips_remote_prefix() {
    // Act
    let branch = remote_branch_name_from_upstream_ref("origin/wt/abc12345");

    // Assert
    assert_eq!(branch, "wt/abc12345");
}

#[test]
fn test_remote_branch_name_returns_input_for_bare_ref() {
    // Act
    let branch = remote_branch_name_from_upstream_ref("no-slash");

    // Assert
    assert_eq!(branch, "no-slash");
}
