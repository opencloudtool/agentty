//! Session loading and derived snapshot attributes from persisted rows.

use std::collections::HashMap;
use std::path::Path;

use super::{draft, session_folder};
use crate::app::SessionManager;
use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::session::{
    DailyActivity, PublishedBranchSyncStatus, ReviewRequest, ReviewRequestSummary, Session,
    SessionFollowUpTask, SessionHandles, SessionSize, SessionStats, Status,
};
use crate::infra::agent::protocol::QuestionItem;
#[cfg(test)]
use crate::infra::db::Database;
use crate::infra::db::{AppRepositories, SessionRow};
use crate::infra::fs::FsClient;
use crate::infra::git::GitClient;

/// Precomputed fields needed to assemble one loaded session snapshot.
struct LoadedSessionInput {
    draft_attachments: Vec<crate::infra::channel::TurnPromptAttachment>,
    follow_up_tasks: Vec<SessionFollowUpTask>,
    folder: std::path::PathBuf,
    project_name: String,
    questions: Vec<QuestionItem>,
    reasoning_level_override: Option<ReasoningLevel>,
    review_request: Option<ReviewRequest>,
    row: SessionRow,
    session_model: AgentModel,
    session_output: String,
    session_status: Status,
    size: SessionSize,
}

impl SessionManager {
    /// Loads session models from the database using the provided filesystem
    /// boundary to decide which session folders exist.
    ///
    /// Existing handles are reused in place to preserve `Arc` identity so
    /// that background workers holding cloned references continue to work.
    ///
    /// When a handle already exists, live handle output is treated as
    /// authoritative for the returned in-memory snapshot to avoid clobbering
    /// fresh runtime output with stale persisted rows. Active statuses are also
    /// preserved from live handles, while terminal persisted statuses (`Done`,
    /// `Canceled`) override stale in-memory status.
    ///
    /// Retired persisted model ids are upgraded to their current replacement
    /// models while rows are loaded.
    ///
    /// New handles are inserted for sessions that don't have entries yet.
    ///
    /// Returns loaded sessions, local-day activity counts aggregated from
    /// persisted session-creation activity history, and cached worktree
    /// availability keyed by session id.
    pub(crate) async fn load_sessions_with_fs_client(
        base: &Path,
        db: &AppRepositories,
        active_project_id: i64,
        working_dir: &Path,
        handles: &mut HashMap<String, SessionHandles>,
        fs_client: &dyn FsClient,
    ) -> (Vec<Session>, Vec<DailyActivity>, HashMap<String, bool>) {
        let project_name = working_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let db_rows = db
            .load_sessions_for_project(active_project_id)
            .await
            .unwrap_or_default();
        let persisted_follow_up_tasks = db.load_session_follow_up_tasks().await.unwrap_or_default();
        let stats_activity = db.load_session_activity().await.unwrap_or_default();
        let mut sessions: Vec<Session> = Vec::new();
        let mut follow_up_tasks_by_session = HashMap::<String, Vec<_>>::new();
        let mut session_worktree_availability = HashMap::new();

        for persisted_follow_up_task in persisted_follow_up_tasks {
            follow_up_tasks_by_session
                .entry(persisted_follow_up_task.session_id.clone())
                .or_default()
                .push(persisted_follow_up_task.into_session_follow_up_task());
        }

        for row in db_rows {
            let folder = session_folder(base, &row.id);
            let persisted_status = row.status.parse::<Status>().unwrap_or(Status::Done);
            let persisted_size = row.size.parse::<SessionSize>().unwrap_or_default();
            let has_session_folder = fs_client.is_dir(folder.clone());
            let live_handle_status = handles
                .get(&row.id)
                .and_then(|existing| existing.status.lock().ok().map(|status| *status));

            if should_skip_missing_folder_session(
                has_session_folder,
                row.is_draft,
                persisted_status,
                live_handle_status,
            ) {
                continue;
            }
            session_worktree_availability.insert(row.id.clone(), has_session_folder);
            let session_model = AgentModel::parse_persisted(&row.model)
                .unwrap_or_else(|_| AgentKind::Gemini.default_model());

            let (session_output, session_status) = if let Some(existing) = handles.get(&row.id) {
                let output_from_handle = existing
                    .output
                    .lock()
                    .ok()
                    .map_or_else(|| row.output.clone(), |output| output.clone());
                let status_from_handle = existing
                    .status
                    .lock()
                    .ok()
                    .map_or(persisted_status, |status| *status);
                let merged_status =
                    merge_loaded_session_status(persisted_status, status_from_handle);

                if let Ok(mut handle_status) = existing.status.lock() {
                    *handle_status = merged_status;
                }

                (output_from_handle, merged_status)
            } else {
                handles.insert(
                    row.id.clone(),
                    SessionHandles::new(row.output.clone(), persisted_status),
                );

                (row.output.clone(), persisted_status)
            };

            let review_request = parse_review_request(&row);
            let draft_attachments =
                draft::load_staged_draft_attachments(fs_client, base, &row.id).await;
            let questions = row
                .questions
                .as_deref()
                .and_then(parse_questions_json)
                .unwrap_or_default();
            let reasoning_level_override = row
                .reasoning_level_override
                .as_deref()
                .and_then(|value| value.parse::<ReasoningLevel>().ok());
            let follow_up_tasks = follow_up_tasks_by_session
                .remove(&row.id)
                .unwrap_or_default();

            sessions.push(Self::build_loaded_session(LoadedSessionInput {
                draft_attachments,
                follow_up_tasks,
                folder,
                project_name: project_name.clone(),
                questions,
                reasoning_level_override,
                review_request,
                row,
                session_model,
                session_output,
                session_status,
                size: persisted_size,
            }));
        }

        (sessions, stats_activity, session_worktree_availability)
    }

    /// Computes diff-derived session size and line-count totals from one
    /// worktree folder using the injected filesystem boundary.
    pub(crate) async fn session_diff_stats_for_folder(
        fs_client: &dyn FsClient,
        git_client: &dyn GitClient,
        folder: &Path,
        base_branch: &str,
    ) -> (SessionSize, u64, u64) {
        if !fs_client.is_dir(folder.to_path_buf()) {
            return (SessionSize::Xs, 0, 0);
        }

        let folder = folder.to_path_buf();
        let base_branch = base_branch.to_string();
        let diff = git_client
            .diff(folder, base_branch)
            .await
            .ok()
            .unwrap_or_default();

        let (added_lines, deleted_lines) = SessionStats::line_change_counts(&diff);

        (SessionSize::from_diff(&diff), added_lines, deleted_lines)
    }

    /// Builds one in-memory session snapshot from a database row plus the
    /// transient fields computed during reload.
    fn build_loaded_session(input: LoadedSessionInput) -> Session {
        Session {
            base_branch: input.row.base_branch,
            created_at: input.row.created_at,
            draft_attachments: input.draft_attachments,
            folder: input.folder,
            follow_up_tasks: input.follow_up_tasks,
            id: input.row.id,
            in_progress_started_at: input.row.in_progress_started_at,
            in_progress_total_seconds: input.row.in_progress_total_seconds,
            is_draft: input.row.is_draft,
            model: input.session_model,
            output: input.session_output,
            project_name: input.project_name,
            prompt: input.row.prompt,
            reasoning_level_override: input.reasoning_level_override,
            published_upstream_ref: input.row.published_upstream_ref,
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: input.questions,
            review_request: input.review_request,
            size: input.size,
            stats: SessionStats {
                added_lines: input.row.added_lines.cast_unsigned(),
                deleted_lines: input.row.deleted_lines.cast_unsigned(),
                input_tokens: input.row.input_tokens.cast_unsigned(),
                output_tokens: input.row.output_tokens.cast_unsigned(),
            },
            status: input.session_status,
            summary: input.row.summary,
            title: input.row.title,
            updated_at: input.row.updated_at,
        }
    }
}

/// Returns whether one persisted session row should be skipped because its
/// worktree folder is missing and no merge-cleanup transition is still active.
fn should_skip_missing_folder_session(
    has_session_folder: bool,
    is_draft_session: bool,
    persisted_status: Status,
    live_handle_status: Option<Status>,
) -> bool {
    if has_session_folder {
        return false;
    }

    if matches!(persisted_status, Status::Done | Status::Canceled) {
        return false;
    }

    if is_draft_session && persisted_status == Status::New {
        return false;
    }

    !matches!(
        live_handle_status,
        Some(Status::Merging | Status::Done | Status::Canceled)
    )
}

/// Merges one loaded status with the existing live-handle status.
///
/// Existing handle status is kept for active transitions to prevent stale DB
/// snapshots from clobbering in-memory updates. Persisted terminal statuses
/// (`Done`, `Canceled`) take precedence so explicit DB transitions still appear
/// after refresh.
fn merge_loaded_session_status(status_from_db: Status, status_from_handle: Status) -> Status {
    if matches!(status_from_db, Status::Done | Status::Canceled) {
        return status_from_db;
    }

    status_from_handle
}

/// Parses normalized review-request metadata from one loaded database row.
///
/// Incomplete or invalid persisted metadata is ignored so stale partial rows do
/// not block session loading.
fn parse_review_request(row: &SessionRow) -> Option<ReviewRequest> {
    let review_request_row = row.review_request.as_ref()?;
    let forge_kind = parse_optional_enum(Some(review_request_row.forge_kind.as_str())).ok()?;
    let state = parse_optional_enum(Some(review_request_row.state.as_str())).ok()?;

    Some(ReviewRequest {
        last_refreshed_at: review_request_row.last_refreshed_at,
        summary: ReviewRequestSummary {
            display_id: review_request_row.display_id.clone(),
            forge_kind,
            source_branch: review_request_row.source_branch.clone(),
            state,
            status_summary: review_request_row.status_summary.clone(),
            target_branch: review_request_row.target_branch.clone(),
            title: review_request_row.title.clone(),
            web_url: review_request_row.web_url.clone(),
        },
    })
}

/// Converts one optional persisted string into a parsed enum value.
fn parse_optional_enum<T>(value: Option<&str>) -> Result<T, ()>
where
    T: std::str::FromStr,
{
    value.ok_or(())?.parse().map_err(|_| ())
}

/// Parses persisted question JSON with backward compatibility.
///
/// Attempts to deserialize as `Vec<QuestionItem>` first (new format). Falls
/// back to `Vec<String>` (legacy format) and converts each entry into a
/// `QuestionItem` without predefined options.
fn parse_questions_json(raw_json: &str) -> Option<Vec<QuestionItem>> {
    if raw_json.is_empty() {
        return None;
    }

    if let Ok(items) = serde_json::from_str::<Vec<QuestionItem>>(raw_json) {
        return Some(items);
    }

    serde_json::from_str::<Vec<String>>(raw_json)
        .ok()
        .map(|texts| {
            texts
                .into_iter()
                .map(|text| QuestionItem {
                    options: Vec::new(),
                    text,
                })
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::domain::session::{ForgeKind, ReviewRequestState, ReviewRequestSummary};
    use crate::infra::db::SessionReviewRequestRow;
    use crate::infra::fs;

    /// Returns a filesystem mock that reports the supplied directories as
    /// existing and treats missing staged-draft metadata files as absent.
    fn create_folder_lookup_mock(existing_folders: Vec<PathBuf>) -> fs::MockFsClient {
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_is_dir()
            .times(0..)
            .returning(move |path| existing_folders.contains(&path));
        mock_fs_client.expect_read_file().times(0..).returning(|_| {
            Box::pin(async {
                Err(fs::FsError::Io(std::io::Error::from(
                    std::io::ErrorKind::NotFound,
                )))
            })
        });

        mock_fs_client
    }

    /// Ensures reload keeps live handle output and active status when
    /// persisted row data is stale.
    #[tokio::test]
    async fn test_load_sessions_preserves_live_handle_output_and_status() {
        // Arrange
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.append_session_output(session_id, "DB Output")
            .await
            .expect("failed to append persisted output");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

        let mut handles = HashMap::new();
        let live_output = "Live Output".to_string();
        let live_status = Status::Review;
        handles.insert(
            session_id.to_string(),
            SessionHandles::new(live_output.clone(), live_status),
        );

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            base_path,
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
            &mock_fs_client,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.output, live_output);
        assert_eq!(session.status, live_status);

        let handle = handles
            .get(session_id)
            .expect("missing existing runtime handle");
        let handle_output = handle
            .output
            .lock()
            .expect("failed to lock handle output")
            .clone();
        let handle_status = *handle.status.lock().expect("failed to lock handle status");
        assert_eq!(handle_output, live_output);
        assert_eq!(handle_status, live_status);
    }

    /// Ensures reload caches worktree availability alongside loaded session
    /// rows.
    #[tokio::test]
    async fn test_load_sessions_reports_worktree_availability() {
        // Arrange
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let session_with_worktree_id = "worktree-available";
        let session_without_worktree_id = "draft-missing";
        db.insert_session(
            session_with_worktree_id,
            "gemini-3-flash-preview",
            "main",
            "New",
            project_id,
        )
        .await
        .expect("failed to insert session with worktree");
        db.insert_draft_session(
            session_without_worktree_id,
            "gemini-3-flash-preview",
            "main",
            "New",
            project_id,
        )
        .await
        .expect("failed to insert draft session");

        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client =
            create_folder_lookup_mock(vec![session_folder(base_path, session_with_worktree_id)]);
        let mut handles = HashMap::new();

        // Act
        let (_, _, session_worktree_availability) = SessionManager::load_sessions_with_fs_client(
            base_path,
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
            &mock_fs_client,
        )
        .await;

        // Assert
        assert_eq!(
            session_worktree_availability.get(session_with_worktree_id),
            Some(&true)
        );
        assert_eq!(
            session_worktree_availability.get(session_without_worktree_id),
            Some(&false)
        );
    }

    /// Ensures reload reads the persisted summary for active sessions.
    #[tokio::test]
    async fn test_load_sessions_reads_persisted_summary_for_active_session() {
        // Arrange
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.update_session_summary(session_id, "persisted summary")
            .await
            .expect("failed to update session summary");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

        let mut handles = HashMap::new();
        handles.insert(
            session_id.to_string(),
            SessionHandles::new("Live Output".to_string(), Status::Review),
        );

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            base_path,
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
            &mock_fs_client,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.summary.as_deref(), Some("persisted summary"));
    }

    /// Ensures terminal persisted statuses replace stale active handle status
    /// during reload.
    #[tokio::test]
    async fn test_load_sessions_terminal_db_status_overrides_handle_status() {
        // Arrange
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert session");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

        let mut handles = HashMap::new();
        handles.insert(
            session_id.to_string(),
            SessionHandles::new("output".to_string(), Status::Review),
        );

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            base_path,
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
            &mock_fs_client,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.status, Status::Done);

        let handle = handles
            .get(session_id)
            .expect("missing existing runtime handle");
        let handle_status = *handle.status.lock().expect("failed to lock handle status");
        assert_eq!(handle_status, Status::Done);
    }

    /// Ensures persisted review-request metadata is mapped onto loaded session
    /// snapshots.
    #[tokio::test]
    async fn test_load_sessions_maps_review_request_metadata() {
        // Arrange
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let review_request = ReviewRequest {
            last_refreshed_at: 999,
            summary: ReviewRequestSummary {
                display_id: "#17".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Closed,
                status_summary: Some("closed by maintainer".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/team/project/pull/17".to_string(),
            },
        };

        let session_id = "test-session";
        db.insert_session(
            session_id,
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.update_session_review_request(session_id, Some(&review_request))
            .await
            .expect("failed to persist review request metadata");

        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client = create_folder_lookup_mock(Vec::new());
        let mut handles = HashMap::new();

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            base_path,
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
            &mock_fs_client,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.review_request, Some(review_request));
    }

    #[tokio::test]
    /// Ensures reload maps persisted launched sibling-session links onto
    /// follow-up tasks.
    async fn test_load_sessions_maps_follow_up_task_launch_links() {
        // Arrange
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        db.insert_session(
            "source-session",
            "gemini-3-flash-preview",
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert source session");
        db.insert_session(
            "sibling-session",
            "gemini-3-flash-preview",
            "main",
            "New",
            project_id,
        )
        .await
        .expect("failed to insert sibling session");
        db.replace_session_follow_up_tasks(
            "source-session",
            &["Launch the sibling task.".to_string()],
        )
        .await
        .expect("failed to persist follow-up task");
        db.update_session_follow_up_task_launched_session_id(
            "source-session",
            0,
            Some("sibling-session"),
        )
        .await
        .expect("failed to persist launched sibling-session id");

        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client =
            create_folder_lookup_mock(vec![session_folder(base_path, "source-session")]);
        let mut handles = HashMap::new();

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            base_path,
            &db,
            project_id,
            Path::new("/tmp/test"),
            &mut handles,
            &mock_fs_client,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == "source-session")
            .expect("missing reloaded session");
        assert_eq!(session.follow_up_tasks.len(), 1);
        assert_eq!(
            session.follow_up_tasks[0].launched_session_id.as_deref(),
            Some("sibling-session")
        );
        assert_eq!(session.follow_up_tasks[0].position, 0);
    }

    #[test]
    /// Verifies terminal DB statuses override stale in-memory handle statuses.
    fn merge_loaded_session_status_prefers_terminal_status_from_db() {
        // Arrange
        let status_from_db = Status::Done;
        let status_from_handle = Status::New;

        // Act
        let merged_status = merge_loaded_session_status(status_from_db, status_from_handle);

        // Assert
        assert_eq!(merged_status, Status::Done);
    }

    #[test]
    /// Verifies non-terminal DB statuses do not overwrite in-memory status.
    fn merge_loaded_session_status_prefers_handle_for_non_terminal_db_status() {
        // Arrange
        let status_from_db = Status::Review;
        let status_from_handle = Status::InProgress;

        // Act
        let merged_status = merge_loaded_session_status(status_from_db, status_from_handle);

        // Assert
        assert_eq!(merged_status, Status::InProgress);
    }

    #[test]
    /// Verifies missing-folder rows stay visible while merge cleanup has
    /// removed the worktree before `Done` persistence finishes.
    fn should_skip_missing_folder_session_keeps_live_merging_session() {
        // Arrange
        let has_session_folder = false;
        let persisted_status = Status::Merging;
        let live_handle_status = Some(Status::Merging);

        // Act
        let should_skip = should_skip_missing_folder_session(
            has_session_folder,
            false,
            persisted_status,
            live_handle_status,
        );

        // Assert
        assert!(!should_skip);
    }

    #[test]
    /// Verifies missing-folder non-terminal rows are still filtered when no
    /// merge-cleanup transition is active.
    fn should_skip_missing_folder_session_skips_orphaned_active_session() {
        // Arrange
        let has_session_folder = false;
        let persisted_status = Status::Review;
        let live_handle_status = None;

        // Act
        let should_skip = should_skip_missing_folder_session(
            has_session_folder,
            false,
            persisted_status,
            live_handle_status,
        );

        // Assert
        assert!(should_skip);
    }

    #[test]
    /// Verifies missing-folder draft sessions stay visible before their
    /// deferred worktree is created.
    fn should_skip_missing_folder_session_keeps_new_draft_session() {
        // Arrange
        let has_session_folder = false;
        let persisted_status = Status::New;
        let live_handle_status = None;

        // Act
        let should_skip = should_skip_missing_folder_session(
            has_session_folder,
            true,
            persisted_status,
            live_handle_status,
        );

        // Assert
        assert!(!should_skip);
    }

    #[test]
    /// Verifies invalid review-request rows are ignored during session load.
    fn parse_review_request_returns_none_for_invalid_row() {
        // Arrange
        let row = SessionRow {
            added_lines: 0,
            base_branch: "main".to_string(),
            created_at: 0,
            deleted_lines: 0,
            id: "session-a".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            input_tokens: 0,
            is_draft: false,
            model: "gpt-5.4".to_string(),
            output: String::new(),
            output_tokens: 0,
            project_id: Some(1),
            prompt: String::new(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            questions: None,
            review_request: Some(SessionReviewRequestRow {
                display_id: "#42".to_string(),
                forge_kind: "UnknownForge".to_string(),
                last_refreshed_at: 0,
                source_branch: "feature/forge".to_string(),
                state: "Open".to_string(),
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            }),
            size: "XS".to_string(),
            status: "Review".to_string(),
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let review_request = parse_review_request(&row);

        // Assert
        assert_eq!(review_request, None);
    }

    #[test]
    fn test_parse_questions_json_new_format() {
        // Arrange
        let json = r#"[{"text":"Pick one?","options":["A","B"]}]"#;

        // Act
        let result = parse_questions_json(json);

        // Assert
        let items = result.expect("expected Some");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Pick one?");
        assert_eq!(items[0].options, vec!["A", "B"]);
    }

    #[test]
    fn test_parse_questions_json_legacy_format() {
        // Arrange
        let json = r#"["Need target?","Need tests?"]"#;

        // Act
        let result = parse_questions_json(json);

        // Assert
        let items = result.expect("expected Some");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "Need target?");
        assert!(items[0].options.is_empty());
        assert_eq!(items[1].text, "Need tests?");
        assert!(items[1].options.is_empty());
    }

    #[test]
    fn test_parse_questions_json_empty_string_returns_none() {
        // Arrange / Act
        let result = parse_questions_json("");

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_questions_json_invalid_json_returns_none() {
        // Arrange / Act
        let result = parse_questions_json("{not valid json");

        // Assert
        assert!(result.is_none());
    }
}
