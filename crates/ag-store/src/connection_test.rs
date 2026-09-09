use ag_agent::{AgentModel, ReasoningLevel, SessionDiffState, SessionStats, SpeedMode};
use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionMessageKind,
    SettingName,
};
use sqlx::migrate::Migrator;
use tempfile::tempdir;

use super::{DB_POOL_MAX_CONNECTIONS, Database, DbError, SqlitePool, SqlitePoolOptions};
use crate::{
    NewSessionReviewCommentResolution, PersistedSessionCreation, SessionFocusedReviewRow,
    SessionOperationRow, SessionRow, SessionTurnMetadata,
};

/// Builds one deterministic persisted review-request fixture for DB tests.
fn review_request_fixture() -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 456,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("2 approvals, checks passing".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    }
}

/// Asserts that one loaded session row carries the expected review-request
/// linkage.
fn assert_review_request_row(row: &SessionRow) {
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.display_id.as_str()),
        Some("#42")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.forge_kind.as_str()),
        Some("GitHub")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.last_refreshed_at),
        Some(456)
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.source_branch.as_str()),
        Some("feature/forge")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.state.as_str()),
        Some("Open")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .and_then(|review_request| review_request.status_summary.as_deref()),
        Some("2 approvals, checks passing")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.target_branch.as_str()),
        Some("main")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.title.as_str()),
        Some("Add forge review support")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.web_url.as_str()),
        Some("https://github.com/agentty-xyz/agentty/pull/42")
    );
}

/// Inserts one session row with deterministic defaults for tests.
async fn insert_session_fixture(
    database: &Database,
    session_id: &str,
    base_branch: &str,
    status: &str,
    project_id: i64,
) {
    database
        .sessions()
        .insert_session(session_id, "gpt-5.6-sol", base_branch, status, project_id)
        .await
        .expect("failed to insert session fixture");
}

/// Inserts one raw session-message row for migration compatibility tests.
async fn insert_session_message_row(
    database: &Database,
    session_id: &str,
    position: i64,
    kind: &str,
    content: &str,
) {
    sqlx::query!(
        r"
INSERT INTO session_message (session_id, position, kind, content)
VALUES (?, ?, ?, ?)
",
        session_id,
        position,
        kind,
        content
    )
    .execute(database.pool())
    .await
    .expect("failed to insert raw session message row");
}

/// Reapplies one embedded migration under an isolated test tracking table.
async fn rerun_embedded_migration(pool: &SqlitePool, version: i64) {
    let migration = sqlx::migrate!("./migrations")
        .iter()
        .find(|migration| migration.version == version)
        .cloned()
        .expect("embedded migration should exist");
    let mut migrator = Migrator::with_migrations(vec![migration]);
    migrator.dangerous_set_table_name(format!("_sqlx_test_migrations_{version}"));

    migrator
        .run(pool)
        .await
        .expect("embedded migration should run");
}

/// Loads all project settings in deterministic name order for migration
/// assertions.
async fn load_project_setting_rows(database: &Database, project_id: i64) -> Vec<(String, String)> {
    sqlx::query_as::<_, (String, String)>(
        r"
SELECT name, value
FROM project_setting
WHERE project_id = ?
ORDER BY name
",
    )
    .bind(project_id)
    .fetch_all(database.pool())
    .await
    .expect("failed to load project settings")
}

/// Loads the legacy global reasoning value for migration cleanup assertions.
async fn load_legacy_global_reasoning_level(database: &Database) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        r"
SELECT value
FROM setting
WHERE name = 'ReasoningLevel'
",
    )
    .fetch_optional(database.pool())
    .await
    .expect("failed to load legacy global reasoning level")
}

/// Loads one session row by identifier through `load_sessions()`.
async fn load_session_row(database: &Database, session_id: &str) -> SessionRow {
    database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load all sessions")
        .into_iter()
        .find(|row| row.id == session_id)
        .expect("missing session row")
}

/// Loads one persisted session-operation row regardless of lifecycle
/// status.
async fn load_session_operation_row(
    database: &Database,
    operation_id: &str,
) -> SessionOperationRow {
    sqlx::query_as!(
        SessionOperationRow,
        r#"
SELECT id AS "id!", session_id AS "session_id!", kind AS "kind!", status AS "status!",
       queued_at, started_at, finished_at,
       heartbeat_at, last_error, cancel_requested AS "cancel_requested: _"
FROM session_operation
WHERE id = ?
"#,
        operation_id
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load session operation row")
}

/// Typed helper row used to verify nullable session references.
struct SessionUsageSessionIdRow {
    session_id: Option<String>,
}

/// Verifies `open()` creates missing parent directories before opening the
/// on-disk database.
#[tokio::test]
async fn test_open_creates_missing_parent_directory() {
    // Arrange
    let temp_dir = tempdir().expect("temp dir should be created");
    let db_path = temp_dir.path().join("nested/store.db");

    // Act
    let database = Database::open(&db_path)
        .await
        .expect("database should open with missing parent directories");

    // Assert
    assert!(db_path.parent().is_some_and(std::path::Path::is_dir));
    assert!(!database.pool().is_closed());
}

/// Verifies the current schema no longer persists agent session summaries.
#[tokio::test]
async fn test_session_schema_omits_summary_column() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");

    // Act
    let column_names =
        sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('session')")
            .fetch_all(database.pool())
            .await
            .expect("session columns should load");

    // Assert
    assert!(!column_names.iter().any(|name| name == "summary"));
}

/// Verifies `load_sessions()` maps persisted joined session fields.
#[tokio::test]
async fn test_load_sessions_maps_joined_session_fields() {
    // Arrange
    let (database, project_id) = database_with_joined_session_fields().await;

    // Act
    let session_row = load_session_row(&database, "session-a").await;

    // Assert
    assert_eq!(session_row.id, "session-a");
    assert_eq!(session_row.base_branch, "main");
    assert_eq!(session_row.created_at, 100);
    assert_eq!(session_row.updated_at, 200);
    assert_eq!(session_row.agent, "claude");
    assert_eq!(session_row.model, "claude-opus-4.1");
    assert_eq!(session_row.status, "Review");
    assert_eq!(session_row.in_progress_started_at, None);
    assert_eq!(session_row.in_progress_total_seconds, 120);
    assert_eq!(session_row.project_id, Some(project_id));
    assert_eq!(session_row.prompt, "Implement the feature");
    assert_eq!(session_row.added_lines, 14);
    assert_eq!(session_row.deleted_lines, 6);
    assert_eq!(session_row.has_diff, Some(true));
    assert_eq!(session_row.input_tokens, 11);
    assert_eq!(session_row.output_tokens, 29);
    assert_eq!(session_row.parent_session_id, None);
    assert_eq!(session_row.size, "L");
    assert_eq!(session_row.questions.as_deref(), Some("[\"Need logs?\"]"));
    assert_eq!(session_row.title.as_deref(), Some("Feature work"));
    assert_eq!(
        session_row.published_upstream_ref.as_deref(),
        Some("origin/wt/session-a")
    );
    assert_review_request_row(&session_row);
}

/// Verifies the diff-presence migration preserves ambiguous legacy rows.
#[tokio::test]
async fn test_add_session_diff_presence_backfills_legacy_rows() {
    // Arrange
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("failed to open pre-migration database");
    sqlx::query!(
        r"
CREATE TABLE IF NOT EXISTS session (
    id TEXT PRIMARY KEY NOT NULL,
    added_lines INTEGER NOT NULL DEFAULT 0,
    deleted_lines INTEGER NOT NULL DEFAULT 0
)
"
    )
    .execute(&pool)
    .await
    .expect("failed to create pre-migration session table");
    sqlx::query!(
        r"
INSERT INTO session (id, added_lines, deleted_lines)
VALUES ('legacy-clean', 0, 0),
       ('legacy-added', 3, 0),
       ('legacy-deleted', 0, 2)
"
    )
    .execute(&pool)
    .await
    .expect("failed to seed pre-migration sessions");

    // Act
    rerun_embedded_migration(&pool, 61).await;
    let rows = sqlx::query!(
        r#"
SELECT id, has_diff AS "has_diff: bool"
FROM session
ORDER BY id
"#
    )
    .fetch_all(&pool)
    .await
    .expect("failed to load migrated diff presence")
    .into_iter()
    .map(|row| (row.id, row.has_diff))
    .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        rows,
        vec![
            ("legacy-added".to_string(), Some(true)),
            ("legacy-clean".to_string(), None),
            ("legacy-deleted".to_string(), Some(true)),
        ]
    );
}

/// Verifies message appends write ordered rows for the canonical transcript
/// store.
#[tokio::test]
async fn test_append_session_message_writes_message_rows() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;

    // Act
    database
        .sessions()
        .append_session_message("session-a", SessionMessageKind::UserPrompt, "    hi ")
        .await
        .expect("failed to append prompt message");
    database
        .sessions()
        .append_session_message(
            "session-a",
            SessionMessageKind::AssistantAnswer,
            "\nHello\n",
        )
        .await
        .expect("failed to append assistant message");
    database
        .sessions()
        .append_session_message(
            "session-a",
            SessionMessageKind::WorkflowNotice,
            "\n[Sync Error] failed\n",
        )
        .await
        .expect("failed to append workflow notice");

    // Assert
    let messages = database
        .sessions()
        .load_session_messages("session-a")
        .await
        .expect("failed to load session messages");
    let detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load session detail")
        .expect("session detail should exist");
    assert_eq!(detail.prompt, "");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].position, 0);
    assert_eq!(messages[0].kind, SessionMessageKind::UserPrompt.as_str());
    assert_eq!(messages[0].content, "    hi");
    assert_eq!(messages[1].position, 1);
    assert_eq!(
        messages[1].kind,
        SessionMessageKind::AssistantAnswer.as_str()
    );
    assert_eq!(messages[1].content, "Hello");
    assert_eq!(messages[2].position, 2);
    assert_eq!(
        messages[2].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(messages[2].content, "\n[Sync Error] failed\n");
}

/// Verifies legacy transcript checkpoints keep their old collapse semantics
/// before becoming workflow notices.
#[tokio::test]
async fn test_convert_legacy_transcript_messages_keeps_latest_checkpoint() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    insert_session_fixture(&database, "session-b", "main", "Review", project_id).await;
    insert_session_message_row(&database, "session-a", 0, "user_prompt", "old prompt").await;
    insert_session_message_row(&database, "session-a", 1, "assistant_answer", "old answer").await;
    insert_session_message_row(
        &database,
        "session-a",
        2,
        "legacy_transcript",
        "old prompt\nold answer\n",
    )
    .await;
    insert_session_message_row(&database, "session-a", 3, "assistant_answer", "new answer").await;
    insert_session_message_row(&database, "session-b", 0, "transcript_chunk", "chunk text").await;

    // Act
    rerun_embedded_migration(database.pool(), 55).await;

    // Assert
    let checkpoint_messages = database
        .sessions()
        .load_session_messages("session-a")
        .await
        .expect("failed to load session-a messages");
    assert_eq!(checkpoint_messages.len(), 2);
    assert_eq!(checkpoint_messages[0].position, 2);
    assert_eq!(
        checkpoint_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(checkpoint_messages[0].content, "old prompt\nold answer\n");
    assert_eq!(checkpoint_messages[1].position, 3);
    assert_eq!(
        checkpoint_messages[1].kind,
        SessionMessageKind::AssistantAnswer.as_str()
    );
    assert_eq!(checkpoint_messages[1].content, "new answer");

    let chunk_messages = database
        .sessions()
        .load_session_messages("session-b")
        .await
        .expect("failed to load session-b messages");
    assert_eq!(chunk_messages.len(), 1);
    assert_eq!(
        chunk_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(chunk_messages[0].content, "chunk text");
}

/// Verifies canonical transcript appends refresh session list ordering
/// metadata.
#[tokio::test]
async fn test_append_session_message_refreshes_session_updated_at() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .sessions()
        .update_session_updated_at("session-a", 10)
        .await
        .expect("failed to backdate session updated_at");

    // Act
    database
        .sessions()
        .append_session_message(
            "session-a",
            SessionMessageKind::AssistantAnswer,
            "current answer",
        )
        .await
        .expect("failed to append assistant message");

    // Assert
    let (_, updated_at) = database
        .sessions()
        .load_session_timestamps("session-a")
        .await
        .expect("failed to load session timestamps")
        .expect("session timestamps should exist");
    assert!(
        updated_at > 10,
        "expected updated_at refresh, got {updated_at}"
    );
}

/// Verifies session detail loads transcript metadata from message rows.
#[tokio::test]
async fn test_load_session_detail_reads_message_transcript() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .sessions()
        .update_session_prompt("session-a", "Do something")
        .await
        .expect("failed to update prompt");
    // Act
    let detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load session detail")
        .expect("session detail should exist");

    // Assert
    assert_eq!(detail.prompt, "Do something");
}

/// Builds an in-memory database with one session covering joined fields
/// returned by `load_sessions()`.
async fn database_with_joined_session_fields() -> (Database, i64) {
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    let review_request = review_request_fixture();

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    persist_joined_session_metadata(&database, &review_request).await;
    persist_joined_session_state(&database).await;

    (database, project_id)
}

/// Persists metadata fields asserted by the joined-session mapping test.
async fn persist_joined_session_metadata(database: &Database, review_request: &ReviewRequest) {
    database
        .sessions()
        .update_session_created_at("session-a", 100)
        .await
        .expect("failed to update session created_at");
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("failed to update session updated_at");
    database
        .sessions()
        .update_session_diff_stats(14, 6, true, "session-a", "L")
        .await
        .expect("failed to update session diff stats");
    database
        .sessions()
        .update_session_questions("session-a", "[\"Need logs?\"]")
        .await
        .expect("failed to update session questions");
    database
        .sessions()
        .update_session_prompt("session-a", "Implement the feature")
        .await
        .expect("failed to update session prompt");
    database
        .sessions()
        .update_session_title("session-a", "Feature work")
        .await
        .expect("failed to update session title");
    database
        .sessions()
        .update_session_stats(
            "session-a",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 11,
                output_tokens: 29,
            },
        )
        .await
        .expect("failed to update session stats");
    database
        .sessions()
        .update_session_model("session-a", "claude-opus-4.1")
        .await
        .expect("failed to update session model");
    database
        .sessions()
        .update_session_published_upstream_ref("session-a", Some("origin/wt/session-a".to_string()))
        .await
        .expect("failed to update published upstream ref");
    database
        .reviews()
        .update_session_review_request("session-a", Some(review_request.clone()))
        .await
        .expect("failed to update review request");
}

/// Persists timing fields asserted by the joined-session mapping test.
async fn persist_joined_session_state(database: &Database) {
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "InProgress", 50)
        .await
        .expect("failed to open in-progress timing window");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "Review", 170)
        .await
        .expect("failed to close in-progress timing window");
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("failed to update session updated_at");
}

/// Verifies title candidates are ordered by accepted result rather than
/// request completion time.
#[tokio::test]
async fn test_session_title_candidate_order_preserves_usable_results() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;
    database
        .sessions()
        .update_session_provisional_title("session-a", "First draft")
        .await
        .expect("failed to persist provisional title");
    let older_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to claim older title generation")
        .expect("provisional title should permit an older candidate");
    let newer_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to claim newer title generation")
        .expect("provisional title should permit a newer candidate");

    // Act
    let older_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", older_generation, "Earlier usable title")
        .await
        .expect("failed to apply older usable title");
    let newer_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", newer_generation, "Newer usable title")
        .await
        .expect("failed to apply newer usable title");
    let repeated_older_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", older_generation, "Repeated older title")
        .await
        .expect("failed to reject older title after newer candidate");

    // Assert
    let session_row = load_session_row(&database, "session-a").await;
    assert!(older_update_applied);
    assert!(newer_update_applied);
    assert!(!repeated_older_update_applied);
    assert_eq!(session_row.title.as_deref(), Some("Newer usable title"));
}

/// Verifies provisional fallbacks and authoritative titles invalidate every
/// outstanding title candidate.
#[tokio::test]
async fn test_session_title_authority_invalidates_outstanding_candidates() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;
    database
        .sessions()
        .update_session_provisional_title("session-a", "First draft")
        .await
        .expect("failed to persist provisional title");
    let invalidated_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to claim provisional title generation")
        .expect("provisional title should permit generation");

    // Act
    database
        .sessions()
        .update_session_provisional_title("session-a", "New fallback")
        .await
        .expect("failed to replace provisional title");
    let invalidated_update_applied = database
        .sessions()
        .update_session_title_for_generation(
            "session-a",
            invalidated_generation,
            "Invalidated generated title",
        )
        .await
        .expect("failed to reject title invalidated by a newer fallback");
    let stale_generation = database
        .sessions()
        .begin_session_title_generation("session-a", false)
        .await
        .expect("failed to claim title generation before authoritative title")
        .expect("forced title generation should be claimed");
    database
        .sessions()
        .update_session_title("session-a", "Authoritative commit title")
        .await
        .expect("failed to persist authoritative title");
    let stale_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", stale_generation, "Stale generated title")
        .await
        .expect("failed to reject stale title generation");
    let provisional_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to inspect provisional title state");
    let forced_generation = database
        .sessions()
        .begin_session_title_generation("session-a", false)
        .await
        .expect("failed to claim forced title generation")
        .expect("forced title generation should be claimed");
    let forced_update_applied = database
        .sessions()
        .update_session_title_for_generation(
            "session-a",
            forced_generation,
            "Refine draft workflow title",
        )
        .await
        .expect("failed to apply current title generation");

    // Assert
    let session_row = load_session_row(&database, "session-a").await;
    assert!(!invalidated_update_applied);
    assert!(!stale_update_applied);
    assert_eq!(provisional_generation, None);
    assert!(forced_update_applied);
    assert_eq!(
        session_row.title.as_deref(),
        Some("Refine draft workflow title")
    );
}

/// Verifies timing-aware status transitions accumulate repeated
/// `InProgress` intervals.
#[tokio::test]
async fn test_update_session_status_with_timing_at_accumulates_repeated_intervals() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;

    // Act
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "InProgress", 10)
        .await
        .expect("failed to enter in-progress the first time");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "Review", 70)
        .await
        .expect("failed to leave in-progress the first time");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "InProgress", 100)
        .await
        .expect("failed to enter in-progress the second time");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "Question", 190)
        .await
        .expect("failed to leave in-progress the second time");
    let session_row = load_session_row(&database, "session-a").await;

    // Assert
    assert_eq!(session_row.status, "Question");
    assert_eq!(session_row.in_progress_started_at, None);
    assert_eq!(session_row.in_progress_total_seconds, 150);
}

/// Verifies `load_sessions_for_project()` filters rows by project id.
#[tokio::test]
async fn test_load_sessions_for_project_filters_to_project_rows() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let first_project_id = database
        .projects()
        .upsert_project("/tmp/project-a", Some("main".to_string()))
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project("/tmp/project-b", Some("develop".to_string()))
        .await
        .expect("failed to insert second project");

    insert_session_fixture(&database, "session-a", "main", "Review", first_project_id).await;
    insert_session_fixture(&database, "session-b", "main", "Done", first_project_id).await;
    insert_session_fixture(&database, "session-c", "develop", "Done", second_project_id).await;
    database
        .sessions()
        .update_session_updated_at("session-a", 300)
        .await
        .expect("failed to update session-a updated_at");
    database
        .sessions()
        .update_session_updated_at("session-b", 200)
        .await
        .expect("failed to update session-b updated_at");
    database
        .sessions()
        .update_session_updated_at("session-c", 100)
        .await
        .expect("failed to update session-c updated_at");

    // Act
    let session_rows = database
        .sessions()
        .load_sessions_for_project(first_project_id)
        .await
        .expect("failed to load project sessions");

    // Assert
    assert_eq!(session_rows.len(), 2);
    assert_eq!(session_rows[0].id, "session-a");
    assert_eq!(session_rows[1].id, "session-b");
    assert!(
        session_rows
            .iter()
            .all(|row| row.project_id == Some(first_project_id))
    );
}

/// Verifies stacked draft inserts persist their parent session link.
#[tokio::test]
async fn test_insert_stacked_draft_session_persists_parent_session_id() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;

    // Act
    database
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Draft",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert stacked draft session");
    let child_session = load_session_row(&database, "child-session").await;

    // Assert
    assert_eq!(child_session.base_branch, "wt/parent-session");
    assert!(child_session.is_draft);
    assert_eq!(
        child_session.parent_session_id.as_deref(),
        Some("parent-session")
    );
}

/// Verifies restacking clears active child parent links after parent merge.
#[tokio::test]
async fn test_restack_child_sessions_after_parent_merge_clears_active_children() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    database
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Draft",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert active stacked child");
    database
        .sessions()
        .insert_stacked_draft_session(
            "review-child",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Review",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert review stacked child");
    database
        .sessions()
        .insert_stacked_draft_session(
            "canceled-child",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Canceled",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert canceled stacked child");

    // Act
    let restacked_child_session_ids = database
        .sessions()
        .restack_child_sessions_after_parent_merge(
            "parent-session",
            "main",
            Some("parent-tip".to_string()),
        )
        .await
        .expect("failed to restack child sessions");
    let child_session = load_session_row(&database, "child-session").await;
    let review_child = load_session_row(&database, "review-child").await;
    let review_child_stack_base = database
        .sessions()
        .get_session_stack_base_commit_hash("review-child")
        .await
        .expect("failed to load review child stack base");
    let canceled_child = load_session_row(&database, "canceled-child").await;

    // Assert
    assert_eq!(
        restacked_child_session_ids,
        vec!["review-child".to_string()]
    );
    assert_eq!(child_session.parent_session_id, None);
    assert_eq!(child_session.base_branch, "main");
    assert_eq!(review_child.parent_session_id, None);
    assert_eq!(review_child.base_branch, "main");
    assert_eq!(review_child_stack_base.as_deref(), Some("parent-tip"));
    assert_eq!(
        canceled_child.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(canceled_child.base_branch, "wt/parent-session");
}

/// Verifies deleting a parent retargets surviving children onto the
/// parent's base branch instead of leaving them on the orphaned worktree
/// branch.
#[tokio::test]
async fn test_delete_session_retargets_children_base_branch() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    database
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Draft",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert active stacked child");
    database
        .sessions()
        .insert_stacked_draft_session(
            "canceled-child",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Canceled",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert canceled stacked child");

    // Act
    database
        .sessions()
        .delete_session("parent-session")
        .await
        .expect("failed to delete parent session");
    let child_session = load_session_row(&database, "child-session").await;
    let canceled_child = load_session_row(&database, "canceled-child").await;

    // Assert
    assert_eq!(child_session.parent_session_id, None);
    assert_eq!(child_session.base_branch, "main");
    assert_eq!(canceled_child.parent_session_id, None);
    assert_eq!(canceled_child.base_branch, "wt/parent-session");
}

#[tokio::test]
async fn test_load_pending_stack_restack_session_ids_returns_only_review_ready_parentless_rows() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "ready-child", "main", "Review", project_id).await;
    insert_session_fixture(&database, "draft-child", "main", "Draft", project_id).await;
    insert_session_fixture(&database, "plain-review", "main", "Review", project_id).await;
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    database
        .sessions()
        .insert_stacked_draft_session(
            "still-stacked",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Review",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert stacked child");
    for session_id in ["ready-child", "draft-child", "still-stacked"] {
        database
            .sessions()
            .update_session_stack_base_commit_hash(session_id, Some("parent-tip".to_string()))
            .await
            .expect("failed to set stack base hash");
    }

    // Act
    let pending_session_ids = database
        .sessions()
        .load_pending_stack_restack_session_ids(project_id)
        .await
        .expect("failed to load pending restacks");

    // Assert
    assert_eq!(pending_session_ids, vec!["ready-child".to_string()]);
}

#[tokio::test]
async fn test_update_session_stack_membership_updates_and_clears_linkage_atomically() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    insert_session_fixture(&database, "child-session", "main", "Review", project_id).await;

    // Act
    database
        .sessions()
        .update_session_stack_membership(
            "child-session",
            Some("parent-session"),
            "wt/parent-session",
            Some("old-parent-tip".to_string()),
        )
        .await
        .expect("failed to attach child");
    let attached = database
        .sessions()
        .load_session("child-session")
        .await
        .expect("failed to load attached child")
        .expect("attached child should exist");
    let attached_stack_base = database
        .sessions()
        .get_session_stack_base_commit_hash("child-session")
        .await
        .expect("failed to load attached stack base");
    database
        .sessions()
        .update_session_stack_membership("child-session", None, "main", None)
        .await
        .expect("failed to clear child membership");
    let detached = database
        .sessions()
        .load_session("child-session")
        .await
        .expect("failed to load detached child")
        .expect("detached child should exist");
    let detached_stack_base = database
        .sessions()
        .get_session_stack_base_commit_hash("child-session")
        .await
        .expect("failed to load detached stack base");

    // Assert
    assert_eq!(
        attached.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(attached.base_branch, "wt/parent-session");
    assert_eq!(attached_stack_base.as_deref(), Some("old-parent-tip"));
    assert_eq!(detached.parent_session_id, None);
    assert_eq!(detached.base_branch, "main");
    assert_eq!(detached_stack_base, None);
}

/// Verifies `load_sessions_metadata()` returns session count and max
/// `updated_at`.
#[tokio::test]
async fn test_load_sessions_metadata_returns_count_and_latest_timestamp() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    insert_session_fixture(&database, "session-b", "main", "Done", project_id).await;
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("failed to update session-a updated_at");
    database
        .sessions()
        .update_session_updated_at("session-b", 300)
        .await
        .expect("failed to update session-b updated_at");

    // Act
    let session_metadata = database
        .sessions()
        .load_sessions_metadata()
        .await
        .expect("failed to load session metadata");

    // Assert
    assert_eq!(session_metadata, (2, 300));
}

/// Verifies `load_session_timestamps()` returns the persisted timestamps.
#[tokio::test]
async fn test_load_session_timestamps_returns_created_and_updated_values() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
    database
        .sessions()
        .update_session_created_at("session-a", 111)
        .await
        .expect("failed to update session created_at");
    database
        .sessions()
        .update_session_updated_at("session-a", 222)
        .await
        .expect("failed to update session updated_at");

    // Act
    let session_timestamps = database
        .sessions()
        .load_session_timestamps("session-a")
        .await
        .expect("failed to load session timestamps");

    // Assert
    assert_eq!(session_timestamps, Some((111, 222)));
}

/// Verifies `get_session_base_branch()` returns the persisted branch name.
#[tokio::test]
async fn test_get_session_base_branch_returns_persisted_value() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "release", "Done", project_id).await;

    // Act
    let base_branch = database
        .sessions()
        .get_session_base_branch("session-a")
        .await
        .expect("failed to load session base branch");

    // Assert
    assert_eq!(base_branch.as_deref(), Some("release"));
}

/// Verifies `delete_session()` removes the session row and nulls
/// `session_usage.session_id`.
#[tokio::test]
async fn test_delete_session_removes_row_and_nulls_usage_foreign_key() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
    database
        .usage()
        .upsert_session_usage(
            "session-a",
            "claude-opus-4.1",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 11,
                output_tokens: 29,
            },
        )
        .await
        .expect("failed to insert usage row");

    // Act
    database
        .sessions()
        .delete_session("session-a")
        .await
        .expect("failed to delete session");
    let deleted_session = database
        .sessions()
        .load_session_timestamps("session-a")
        .await
        .expect("failed to load deleted session timestamps");
    let retained_usage_row = sqlx::query_as!(
        SessionUsageSessionIdRow,
        r#"
SELECT session_id AS "session_id: _"
FROM session_usage
WHERE model = ?
"#,
        "claude-opus-4.1"
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load retained usage row");

    // Assert
    assert_eq!(deleted_session, None);
    assert_eq!(retained_usage_row.session_id, None,);
}

/// Verifies `load_unfinished_session_operations()` returns only queued and
/// running rows.
#[tokio::test]
async fn test_load_unfinished_session_operations_returns_only_queued_and_running_rows() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .operations()
        .insert_session_operation("operation-queued", "session-a", "merge")
        .await
        .expect("failed to insert queued operation");
    database
        .operations()
        .insert_session_operation("operation-running", "session-a", "sync")
        .await
        .expect("failed to insert running operation");
    database
        .operations()
        .insert_session_operation("operation-done", "session-a", "review")
        .await
        .expect("failed to insert done operation");
    database
        .operations()
        .mark_session_operation_running("operation-running")
        .await
        .expect("failed to mark running operation");
    database
        .operations()
        .mark_session_operation_running("operation-done")
        .await
        .expect("failed to mark done operation running");
    database
        .operations()
        .mark_session_operation_done("operation-done")
        .await
        .expect("failed to mark done operation");

    // Act
    let unfinished_rows = database
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("failed to load unfinished operations");

    // Assert
    assert_eq!(unfinished_rows.len(), 2);
    assert_eq!(unfinished_rows[0].id, "operation-queued");
    assert_eq!(unfinished_rows[0].status, "queued");
    assert_eq!(unfinished_rows[1].id, "operation-running");
    assert_eq!(unfinished_rows[1].status, "running");
}

/// Verifies `request_cancel_for_session_operations()` marks only
/// unfinished rows.
#[tokio::test]
async fn test_request_cancel_for_session_operations_marks_only_unfinished_rows() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .operations()
        .insert_session_operation("operation-queued", "session-a", "merge")
        .await
        .expect("failed to insert queued operation");
    database
        .operations()
        .insert_session_operation("operation-done", "session-a", "review")
        .await
        .expect("failed to insert done operation");
    database
        .operations()
        .mark_session_operation_running("operation-done")
        .await
        .expect("failed to mark done operation running");
    database
        .operations()
        .mark_session_operation_done("operation-done")
        .await
        .expect("failed to mark done operation");

    // Act
    database
        .operations()
        .request_cancel_for_session_operations("session-a")
        .await
        .expect("failed to request cancel");
    let queued_row = load_session_operation_row(&database, "operation-queued").await;
    let done_row = load_session_operation_row(&database, "operation-done").await;

    // Assert
    assert!(queued_row.cancel_requested);
    assert!(!done_row.cancel_requested);
}

/// Verifies `is_session_operation_unfinished()` returns `false` for a
/// completed operation.
#[tokio::test]
async fn test_is_session_operation_unfinished_returns_false_for_done_operation() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .operations()
        .insert_session_operation("operation-a", "session-a", "merge")
        .await
        .expect("failed to insert operation");
    database
        .operations()
        .mark_session_operation_running("operation-a")
        .await
        .expect("failed to mark operation running");
    database
        .operations()
        .mark_session_operation_done("operation-a")
        .await
        .expect("failed to mark operation done");

    // Act
    let is_unfinished = database
        .operations()
        .is_session_operation_unfinished("operation-a")
        .await
        .expect("failed to check unfinished operation state");

    // Assert
    assert!(!is_unfinished);
}

/// Verifies `is_cancel_requested_for_operation()` returns `true` for a
/// cancelled operation and `false` for an unaffected one.
#[tokio::test]
async fn test_is_cancel_requested_for_operation_scoped_to_single_operation() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .operations()
        .insert_session_operation("operation-cancelled", "session-a", "reply")
        .await
        .expect("failed to insert cancelled operation");
    database
        .operations()
        .insert_session_operation("operation-new", "session-a", "reply")
        .await
        .expect("failed to insert new operation");

    // Cancel only the first operation via session-level bulk update.
    database
        .operations()
        .request_cancel_for_session_operations("session-a")
        .await
        .expect("failed to request cancel");

    // Simulate a new operation created after the cancel request by
    // resetting its flag directly (mirrors real flow where new
    // operations are inserted with cancel_requested = 0 by default).
    sqlx::query!("UPDATE session_operation SET cancel_requested = 0 WHERE id = 'operation-new'")
        .execute(&database.pool)
        .await
        .expect("failed to reset new operation flag");

    // Act
    let cancelled_flag = database
        .operations()
        .is_cancel_requested_for_operation("operation-cancelled")
        .await
        .expect("failed to check cancelled operation");
    let new_flag = database
        .operations()
        .is_cancel_requested_for_operation("operation-new")
        .await
        .expect("failed to check new operation");

    // Assert — only the cancelled operation is flagged; the new one
    // proceeds normally.
    assert!(cancelled_flag);
    assert!(!new_flag);
}

/// Verifies `mark_session_operation_running()` sets the running state and
/// timestamps.
#[tokio::test]
async fn test_mark_session_operation_running_sets_started_at_and_heartbeat() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .operations()
        .insert_session_operation("operation-a", "session-a", "merge")
        .await
        .expect("failed to insert operation");

    // Act
    database
        .operations()
        .mark_session_operation_running("operation-a")
        .await
        .expect("failed to mark operation running");
    let running_row = load_session_operation_row(&database, "operation-a").await;

    // Assert
    assert_eq!(running_row.status, "running");
    assert!(running_row.started_at.is_some());
    assert!(running_row.heartbeat_at.is_some());
    assert_eq!(running_row.last_error, None);
}

/// Verifies `mark_session_operation_done()` sets the terminal completion
/// fields.
#[tokio::test]
async fn test_mark_session_operation_done_sets_finished_state() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .operations()
        .insert_session_operation("operation-a", "session-a", "merge")
        .await
        .expect("failed to insert operation");
    database
        .operations()
        .mark_session_operation_running("operation-a")
        .await
        .expect("failed to mark operation running");

    // Act
    database
        .operations()
        .mark_session_operation_done("operation-a")
        .await
        .expect("failed to mark operation done");
    let done_row = load_session_operation_row(&database, "operation-a").await;

    // Assert
    assert_eq!(done_row.status, "done");
    assert!(done_row.finished_at.is_some());
    assert!(done_row.heartbeat_at.is_some());
    assert_eq!(done_row.last_error, None);
}

/// Verifies `upsert_session_usage()` accumulates per-model token totals and
/// invocation counts.
#[tokio::test]
async fn test_upsert_session_usage_accumulates_counts_per_model() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
    database
        .usage()
        .upsert_session_usage(
            "session-a",
            "claude-opus-4.1",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 11,
                output_tokens: 29,
            },
        )
        .await
        .expect("failed to insert first usage row");
    database
        .usage()
        .upsert_session_usage(
            "session-a",
            "claude-opus-4.1",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 3,
                output_tokens: 5,
            },
        )
        .await
        .expect("failed to update existing usage row");
    database
        .usage()
        .upsert_session_usage("session-a", "ignored-model", &SessionStats::default())
        .await
        .expect("failed to ignore zero-usage update");

    // Act
    let usage_rows = database
        .usage()
        .load_session_usage("session-a")
        .await
        .expect("failed to load session usage");

    // Assert
    assert_eq!(usage_rows.len(), 1);
    assert_eq!(usage_rows[0].model, "claude-opus-4.1");
    assert_eq!(usage_rows[0].input_tokens, 14);
    assert_eq!(usage_rows[0].invocation_count, 2);
    assert_eq!(usage_rows[0].output_tokens, 34);
    assert_eq!(usage_rows[0].session_id.as_deref(), Some("session-a"));
}

#[tokio::test]
async fn test_setting_round_trip_supports_default_smart_fast_and_review_models() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");

    database
        .settings()
        .upsert_setting(
            SettingName::DefaultSmartModel,
            AgentModel::Gemini31Pro.as_str(),
        )
        .await
        .expect("failed to persist default smart model");
    database
        .settings()
        .upsert_setting(SettingName::DefaultFastModel, AgentModel::Gpt56Sol.as_str())
        .await
        .expect("failed to persist default fast model");
    database
        .settings()
        .upsert_setting(
            SettingName::DefaultReviewModel,
            AgentModel::ClaudeOpus5.as_str(),
        )
        .await
        .expect("failed to persist default review model");

    // Act
    let default_smart_model = database
        .settings()
        .get_setting(SettingName::DefaultSmartModel)
        .await
        .expect("failed to load default smart model");
    let default_fast_model = database
        .settings()
        .get_setting(SettingName::DefaultFastModel)
        .await
        .expect("failed to load default fast model");
    let default_review_model = database
        .settings()
        .get_setting(SettingName::DefaultReviewModel)
        .await
        .expect("failed to load default review model");

    // Assert
    assert_eq!(
        default_smart_model,
        Some(AgentModel::Gemini31Pro.as_str().to_string())
    );
    assert_eq!(
        default_fast_model,
        Some(AgentModel::Gpt56Sol.as_str().to_string())
    );
    assert_eq!(
        default_review_model,
        Some(AgentModel::ClaudeOpus5.as_str().to_string())
    );
}

#[tokio::test]
async fn test_migrate_hacker_theme_to_green_preserves_theme_selection() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    database
        .settings()
        .upsert_setting(SettingName::Theme, "hacker")
        .await
        .expect("failed to persist legacy theme setting");

    // Act
    rerun_embedded_migration(database.pool(), 60).await;
    let theme = database
        .settings()
        .get_setting(SettingName::Theme)
        .await
        .expect("failed to load migrated theme setting");

    // Assert
    assert_eq!(theme, Some("green".to_string()));
}

#[tokio::test]
async fn test_split_default_reasoning_level_migrates_each_project_role() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    sqlx::query(
        r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, 'ReasoningLevel', 'xhigh')
",
    )
    .bind(project_id)
    .execute(database.pool())
    .await
    .expect("failed to seed legacy project reasoning level");
    sqlx::query(
        r"
INSERT INTO setting (name, value)
VALUES ('ReasoningLevel', 'medium')
",
    )
    .execute(database.pool())
    .await
    .expect("failed to seed legacy global reasoning level");

    // Act
    rerun_embedded_migration(database.pool(), 70).await;
    let migrated_rows = load_project_setting_rows(&database, project_id).await;
    let legacy_global_reasoning_level = load_legacy_global_reasoning_level(&database).await;

    // Assert
    assert_eq!(
        migrated_rows,
        vec![
            (
                SettingName::DefaultFastReasoningLevel.as_str().to_string(),
                "xhigh".to_string()
            ),
            (
                SettingName::DefaultReviewReasoningLevel
                    .as_str()
                    .to_string(),
                "xhigh".to_string()
            ),
            (
                SettingName::DefaultSmartReasoningLevel.as_str().to_string(),
                "xhigh".to_string()
            ),
        ]
    );
    assert_eq!(legacy_global_reasoning_level, None);
}

#[tokio::test]
async fn test_split_default_reasoning_level_migrates_global_only_value() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/global-only-project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    sqlx::query(
        r"
INSERT INTO setting (name, value)
VALUES ('ReasoningLevel', 'medium')
",
    )
    .execute(database.pool())
    .await
    .expect("failed to seed legacy global reasoning level");

    // Act
    rerun_embedded_migration(database.pool(), 70).await;
    let migrated_rows = load_project_setting_rows(&database, project_id).await;
    let legacy_global_reasoning_level = load_legacy_global_reasoning_level(&database).await;

    // Assert
    assert_eq!(
        migrated_rows,
        vec![
            (
                SettingName::DefaultFastReasoningLevel.as_str().to_string(),
                "medium".to_string()
            ),
            (
                SettingName::DefaultReviewReasoningLevel
                    .as_str()
                    .to_string(),
                "medium".to_string()
            ),
            (
                SettingName::DefaultSmartReasoningLevel.as_str().to_string(),
                "medium".to_string()
            ),
        ]
    );
    assert_eq!(legacy_global_reasoning_level, None);
}

#[tokio::test]
async fn test_remove_session_wall_clock_triggers_drops_legacy_timestamp_policy() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    seed_legacy_wall_clock_schema(&database).await;

    // Act
    rerun_embedded_migration(database.pool(), 71).await;
    let trigger_count = sqlx::query_scalar::<_, i64>(
        r"
SELECT COUNT(*)
FROM sqlite_master
WHERE type = 'trigger'
  AND name IN ('update_session_insert_timestamps', 'update_session_updated_at')
",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to count legacy timestamp triggers");
    let usage_created_at = sqlx::query_scalar::<_, i64>(
        "SELECT created_at FROM session_usage WHERE session_id = 'session-a'",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load migrated usage row");
    let usage_created_at_default = sqlx::query_scalar::<_, Option<String>>(
        r"
SELECT dflt_value
FROM pragma_table_info('session_usage')
WHERE name = 'created_at'
",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load usage timestamp default");

    // Assert
    assert_eq!(trigger_count, 0);
    assert_eq!(usage_created_at, 123);
    assert_eq!(usage_created_at_default, None);
}

async fn seed_legacy_wall_clock_schema(database: &Database) {
    let project_id = database
        .projects()
        .upsert_project("/tmp/clock-migration", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    sqlx::query("DROP TABLE session_usage")
        .execute(database.pool())
        .await
        .expect("failed to drop current usage table");
    sqlx::query(
        r"
CREATE TABLE session_usage (
    session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    input_tokens INTEGER NOT NULL DEFAULT 0,
    invocation_count INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    UNIQUE(session_id, model)
)
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy usage table");
    sqlx::query(
        r"
CREATE INDEX session_usage_session_id_idx ON session_usage (session_id)
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy usage index");
    sqlx::query(
        r"
INSERT INTO session_usage (
    session_id, model, created_at, input_tokens, invocation_count, output_tokens
)
VALUES ('session-a', 'gpt-5.6-sol', 123, 3, 1, 5)
",
    )
    .execute(database.pool())
    .await
    .expect("failed to seed legacy usage row");
    sqlx::query(
        r"
CREATE TRIGGER update_session_insert_timestamps
AFTER INSERT ON session
BEGIN
    UPDATE session SET updated_at = unixepoch() WHERE rowid = NEW.rowid;
END
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy insert trigger");
    sqlx::query(
        r"
CREATE TRIGGER update_session_updated_at
AFTER UPDATE ON session
BEGIN
    SELECT unixepoch();
END
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy update trigger");
}

#[tokio::test]
async fn test_project_setting_round_trip_is_isolated_per_project() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let first_project_id = database
        .projects()
        .upsert_project("/tmp/project-a", Some("main".to_string()))
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project("/tmp/project-b", Some("main".to_string()))
        .await
        .expect("failed to insert second project");

    database
        .settings()
        .upsert_project_setting(
            first_project_id,
            SettingName::LaunchConfiguration,
            "npm run dev",
        )
        .await
        .expect("failed to persist first project setting");
    database
        .settings()
        .upsert_project_setting(
            second_project_id,
            SettingName::LaunchConfiguration,
            "cargo test",
        )
        .await
        .expect("failed to persist second project setting");

    // Act
    let first_project_setting = database
        .settings()
        .get_project_setting(first_project_id, SettingName::LaunchConfiguration)
        .await
        .expect("failed to load first project setting");
    let second_project_setting = database
        .settings()
        .get_project_setting(second_project_id, SettingName::LaunchConfiguration)
        .await
        .expect("failed to load second project setting");

    // Assert
    assert_eq!(first_project_setting, Some("npm run dev".to_string()));
    assert_eq!(second_project_setting, Some("cargo test".to_string()));
}

#[tokio::test]
async fn test_project_role_reasoning_levels_round_trip_with_typed_setting_helpers() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    // Act
    let role_reasoning_levels = [
        (
            SettingName::DefaultSmartReasoningLevel,
            ReasoningLevel::High,
        ),
        (SettingName::DefaultFastReasoningLevel, ReasoningLevel::Low),
        (
            SettingName::DefaultReviewReasoningLevel,
            ReasoningLevel::XHigh,
        ),
    ];
    for (name, reasoning_level) in role_reasoning_levels {
        database
            .settings()
            .upsert_project_setting(project_id, name, reasoning_level.as_str())
            .await
            .expect("failed to persist project role reasoning level");
    }
    let mut loaded_reasoning_levels = Vec::new();
    for (name, _) in role_reasoning_levels {
        loaded_reasoning_levels.push(
            database
                .settings()
                .load_project_reasoning_level(project_id, name)
                .await
                .expect("failed to load project role reasoning level"),
        );
    }

    // Assert
    assert_eq!(
        loaded_reasoning_levels,
        vec![
            ReasoningLevel::High,
            ReasoningLevel::Low,
            ReasoningLevel::XHigh,
        ]
    );
}

#[tokio::test]
async fn test_load_project_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    // Act
    let missing_setting_level = database
        .settings()
        .load_project_reasoning_level(project_id, SettingName::DefaultSmartReasoningLevel)
        .await
        .expect("failed to load default project reasoning level");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartReasoningLevel,
            "unsupported",
        )
        .await
        .expect("failed to insert unsupported project reasoning level");
    let invalid_setting_level = database
        .settings()
        .load_project_reasoning_level(project_id, SettingName::DefaultSmartReasoningLevel)
        .await
        .expect("failed to load fallback project reasoning level");

    // Assert
    assert_eq!(missing_setting_level, ReasoningLevel::High);
    assert_eq!(invalid_setting_level, ReasoningLevel::High);
}

#[tokio::test]
async fn test_load_project_speed_mode_round_trips_and_defaults_invalid_values() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    // Act
    let missing_speed_mode = database
        .settings()
        .load_project_speed_mode(project_id, SettingName::DefaultSmartSpeedMode)
        .await
        .expect("failed to load missing speed mode");
    database
        .settings()
        .upsert_project_setting(project_id, SettingName::DefaultSmartSpeedMode, "fast")
        .await
        .expect("failed to persist speed mode");
    let fast_speed_mode = database
        .settings()
        .load_project_speed_mode(project_id, SettingName::DefaultSmartSpeedMode)
        .await
        .expect("failed to load speed mode");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartSpeedMode,
            "unsupported",
        )
        .await
        .expect("failed to persist invalid speed mode");
    let invalid_speed_mode = database
        .settings()
        .load_project_speed_mode(project_id, SettingName::DefaultSmartSpeedMode)
        .await
        .expect("failed to load invalid speed mode");

    // Assert
    assert_eq!(missing_speed_mode, SpeedMode::Normal);
    assert_eq!(fast_speed_mode, SpeedMode::Fast);
    assert_eq!(invalid_speed_mode, SpeedMode::Normal);
}

#[tokio::test]
async fn test_session_provider_conversation_id_round_trip_and_clear() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .sessions()
        .update_session_provider_conversation_id("session-a", Some("thread-123".to_string()))
        .await
        .expect("failed to set provider conversation id");
    let stored_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");
    database
        .sessions()
        .update_session_provider_conversation_id("session-a", None)
        .await
        .expect("failed to clear provider conversation id");
    let cleared_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load cleared provider conversation id");

    // Assert
    assert_eq!(stored_id, Some("thread-123".to_string()));
    assert_eq!(cleared_id, None);
}

#[tokio::test]
async fn test_session_instruction_conversation_id_round_trip_and_clear() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");
    let instruction_conversation_id = Some("thread-123");

    // Act
    database
        .sessions()
        .update_session_instruction_conversation_id(
            "session-a",
            instruction_conversation_id.map(str::to_string),
        )
        .await
        .expect("failed to set instruction conversation id");
    let stored_conversation_id = database
        .sessions()
        .get_session_instruction_conversation_id("session-a")
        .await
        .expect("failed to load instruction conversation id");
    database
        .sessions()
        .update_session_instruction_conversation_id("session-a", None)
        .await
        .expect("failed to clear instruction conversation id");
    let cleared_conversation_id = database
        .sessions()
        .get_session_instruction_conversation_id("session-a")
        .await
        .expect("failed to load cleared instruction conversation id");

    // Assert
    assert_eq!(stored_conversation_id, Some("thread-123".to_string()));
    assert_eq!(cleared_conversation_id, None);
}

#[tokio::test]
async fn test_session_published_upstream_ref_round_trip_and_clear() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .sessions()
        .update_session_published_upstream_ref("session-a", Some("origin/wt/session-a".to_string()))
        .await
        .expect("failed to persist session published upstream ref");
    let persisted_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing persisted session row");
    database
        .sessions()
        .update_session_published_upstream_ref("session-a", None)
        .await
        .expect("failed to clear session published upstream ref");
    let cleared_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions after clearing")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing cleared session row");

    // Assert
    assert_eq!(
        persisted_row.published_upstream_ref.as_deref(),
        Some("origin/wt/session-a")
    );
    assert_eq!(cleared_row.published_upstream_ref, None);
}

#[tokio::test]
async fn test_load_session_published_upstream_ref_returns_stored_value() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-load", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_published_upstream_ref(
            "session-load",
            Some("origin/wt/session-load".to_string()),
        )
        .await
        .expect("failed to set published upstream ref");

    // Act
    let loaded_ref = database
        .sessions()
        .load_session_published_upstream_ref("session-load")
        .await
        .expect("failed to load published upstream ref");

    // Assert
    assert_eq!(loaded_ref.as_deref(), Some("origin/wt/session-load"));
}

#[tokio::test]
async fn test_load_session_published_upstream_ref_returns_none_when_unset() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-unset", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");

    // Act
    let loaded_ref = database
        .sessions()
        .load_session_published_upstream_ref("session-unset")
        .await
        .expect("failed to load published upstream ref");

    // Assert
    assert_eq!(loaded_ref, None);
}

#[tokio::test]
async fn test_load_session_published_upstream_ref_returns_none_for_missing_session() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");

    // Act
    let loaded_ref = database
        .sessions()
        .load_session_published_upstream_ref("nonexistent")
        .await
        .expect("failed to load published upstream ref");

    // Assert
    assert_eq!(loaded_ref, None);
}

#[tokio::test]
async fn test_session_merged_commit_hash_round_trip_and_clear() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .sessions()
        .update_session_merged_commit_hash("session-a", Some("abc1234".to_string()))
        .await
        .expect("failed to store merged commit hash");
    let stored_hash = database
        .sessions()
        .load_session_merged_commit_hash("session-a")
        .await
        .expect("failed to load stored merged commit hash");
    database
        .sessions()
        .update_session_merged_commit_hash("session-a", None)
        .await
        .expect("failed to clear merged commit hash");
    let cleared_hash = database
        .sessions()
        .load_session_merged_commit_hash("session-a")
        .await
        .expect("failed to load cleared merged commit hash");

    // Assert
    assert_eq!(stored_hash.as_deref(), Some("abc1234"));
    assert_eq!(cleared_hash, None);
}

#[tokio::test]
async fn session_archived_diff_round_trips_empty_and_nonempty_values() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .sessions()
        .update_session_archived_diff("session-a", Some("diff --git a/a b/a".to_string()))
        .await
        .expect("failed to store archived diff");
    let stored_diff = database
        .sessions()
        .load_session_archived_diff("session-a")
        .await
        .expect("failed to load archived diff");
    database
        .sessions()
        .update_session_archived_diff("session-a", Some(String::new()))
        .await
        .expect("failed to store empty archived diff");
    let empty_diff = database
        .sessions()
        .load_session_archived_diff("session-a")
        .await
        .expect("failed to load empty archived diff");

    // Assert
    assert_eq!(stored_diff.as_deref(), Some("diff --git a/a b/a"));
    assert_eq!(empty_diff.as_deref(), Some(""));
}

#[tokio::test]
async fn test_session_review_request_round_trip_and_clear() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let review_request = review_request_fixture();

    // Act
    database
        .reviews()
        .update_session_review_request("session-a", Some(review_request.clone()))
        .await
        .expect("failed to persist session review request");
    let persisted_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing persisted session row");
    database
        .reviews()
        .update_session_review_request("session-a", None)
        .await
        .expect("failed to clear session review request");
    let cleared_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions after clearing")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing cleared session row");

    // Assert
    assert_review_request_row(&persisted_row);
    assert_eq!(cleared_row.review_request, None);
}

#[tokio::test]
async fn test_insert_session_creation_activity_at_persists_timestamp() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 123)
        .await
        .expect("failed to persist activity event");
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load activity timestamps");

    // Assert
    assert_eq!(activity_timestamps, vec![123]);
}

#[tokio::test]
async fn test_insert_session_creation_activity_at_ignores_duplicates_per_session() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 100)
        .await
        .expect("failed to persist first activity event");
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 200)
        .await
        .expect("failed to persist duplicate activity event");
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load activity timestamps");

    // Assert
    assert_eq!(activity_timestamps, vec![100]);
}

#[tokio::test]
async fn test_load_session_activity_timestamps_keeps_deleted_session_history() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert first session");
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 100)
        .await
        .expect("failed to persist first activity event");
    database
        .sessions()
        .insert_session("session-b", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert second session");
    database
        .activity()
        .insert_session_creation_activity_at("session-b", 200)
        .await
        .expect("failed to persist second activity event");
    database
        .sessions()
        .delete_session("session-a")
        .await
        .expect("failed to delete first session");

    // Act
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load activity timestamps");

    // Assert
    assert_eq!(activity_timestamps, vec![100, 200]);
}

#[tokio::test]
async fn test_load_session_activity_timestamps_preserves_event_order() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert first session");
    database
        .sessions()
        .insert_session("session-b", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert second session");
    database
        .sessions()
        .insert_session("session-c", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert third session");

    let first_day_timestamp = 10 * 86_400 + 10;
    let second_timestamp_same_day = 10 * 86_400 + 600;
    let second_day_timestamp = 11 * 86_400 + 50;

    database
        .activity()
        .clear_session_activity()
        .await
        .expect("failed to clear session activity");
    database
        .activity()
        .insert_session_creation_activity_at("session-a", first_day_timestamp)
        .await
        .expect("failed to persist first activity event");
    database
        .activity()
        .insert_session_creation_activity_at("session-b", second_timestamp_same_day)
        .await
        .expect("failed to persist second activity event");
    database
        .activity()
        .insert_session_creation_activity_at("session-c", second_day_timestamp)
        .await
        .expect("failed to persist third activity event");

    // Act
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");

    // Assert
    assert_eq!(
        activity_timestamps,
        vec![
            first_day_timestamp,
            second_timestamp_same_day,
            second_day_timestamp,
        ]
    );
}

#[tokio::test]
async fn test_load_projects_with_stats_returns_session_counts_tokens_and_last_update() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session-a");
    database
        .sessions()
        .persist_session_turn_metadata(
            "session-a",
            &SessionTurnMetadata {
                applied_personality_id: None,
                applied_personality_prompt_hash: None,
                instruction_conversation_id: None,
                model: AgentModel::Gpt56Sol.as_str().to_string(),
                provider_conversation_id: None,
                questions_json: "[]".to_string(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 1_200,
                    output_tokens: 650,
                },
            },
        )
        .await
        .expect("failed to persist session-a token metadata");
    database
        .sessions()
        .insert_session("session-b", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session-b");
    database
        .sessions()
        .persist_session_turn_metadata(
            "session-b",
            &SessionTurnMetadata {
                applied_personality_id: None,
                applied_personality_prompt_hash: None,
                instruction_conversation_id: None,
                model: AgentModel::Gpt56Sol.as_str().to_string(),
                provider_conversation_id: None,
                questions_json: "[]".to_string(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 3,
                    output_tokens: 5,
                },
            },
        )
        .await
        .expect("failed to persist session-b token metadata");

    // Act
    let projects = database
        .projects()
        .load_projects_with_stats()
        .await
        .expect("failed to load projects with stats");

    // Assert
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].session_count, 2);
    assert_eq!(projects[0].input_tokens, 1_203);
    assert_eq!(projects[0].output_tokens, 655);
    assert!(projects[0].last_session_updated_at.is_some());
}

#[tokio::test]
async fn test_set_and_load_active_project_id_round_trip() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");

    // Act
    database
        .settings()
        .set_active_project_id(project_id)
        .await
        .expect("failed to persist active project id");
    let active_project_id = database
        .settings()
        .load_active_project_id()
        .await
        .expect("failed to load active project id");

    // Assert
    assert_eq!(active_project_id, Some(project_id));
}

#[tokio::test]
async fn test_load_session_project_id_returns_associated_project() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    let loaded_project_id = database
        .sessions()
        .load_session_project_id("session-a")
        .await
        .expect("failed to load session project id");

    // Assert
    assert_eq!(loaded_project_id, Some(project_id));
}

#[tokio::test]
async fn test_load_session_focused_reviews_for_project_returns_persisted_review() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_focused_review(
            "session-a",
            Some(ag_session::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("## Review\nPersisted".to_string()),
        )
        .await
        .expect("failed to update focused review");

    // Act
    let focused_reviews = database
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load focused reviews");

    // Assert
    assert_eq!(
        focused_reviews,
        vec![SessionFocusedReviewRow {
            diff_hash: "42".to_string(),
            session_id: "session-a".to_string(),
            text: "## Review\nPersisted".to_string(),
        }]
    );
}

#[tokio::test]
async fn review_diff_baseline_survives_output_clear_and_database_reopen() {
    // Arrange
    let directory = tempdir().expect("temporary directory should exist");
    let path = directory.path().join("review.db");
    let database = Database::open(&path).await.expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("review-project", None)
        .await
        .expect("project should persist");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("session should persist");

    // Act
    let first = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("empty baseline should load");
    database
        .sessions()
        .update_session_review_diff_hash("session-a", "42", false)
        .await
        .expect("baseline should persist");
    database
        .sessions()
        .update_session_focused_review("session-a", None, None, None)
        .await
        .expect("output should clear");
    database.pool().close().await;
    let database = Database::open(&path).await.expect("database should reopen");
    let recovered = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("baseline should survive restart");
    database
        .sessions()
        .update_session_review_diff_hash("session-a", "43", true)
        .await
        .expect("baseline and review claim should persist");
    database.pool().close().await;
    let database = Database::open(&path)
        .await
        .expect("claimed review should reopen");
    let pending = database
        .sessions()
        .load_pending_focused_review_session_ids(project_id)
        .await
        .expect("review claim should survive restart");
    let missing = database
        .sessions()
        .load_session_review_diff_hash("missing")
        .await
        .expect("missing session should be harmless");
    let unfinished = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("unfinished review should remain recoverable");
    database
        .sessions()
        .defer_session_focused_review("session-a")
        .await
        .expect("deferred review should persist");
    let deferred = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("deferred turn should retain its baseline");

    // Assert
    assert_eq!(first, None);
    assert_eq!(recovered.as_deref(), Some("42"));
    assert_eq!(pending, vec!["session-a".to_string()]);
    assert_eq!(missing, None);
    assert_eq!(unfinished, None);
    assert_eq!(deferred.as_deref(), Some("43"));
}

#[tokio::test]
async fn review_diff_claim_failure_rolls_back_baseline() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("review-project", None)
        .await
        .expect("project should persist");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("session should persist");
    database
        .sessions()
        .update_session_review_diff_hash("session-a", "42", false)
        .await
        .expect("old baseline should persist");
    sqlx::raw_sql(
        "CREATE TRIGGER reject_review_claim BEFORE UPDATE OF focused_review_status ON session
         WHEN NEW.focused_review_status = 'Pending' BEGIN SELECT RAISE(ABORT, 'claim failed'); END;",
    )
    .execute(database.pool())
    .await
    .expect("claim failure should be injected");

    // Act
    let result = database
        .sessions()
        .update_session_review_diff_hash("session-a", "43", true)
        .await;
    let baseline = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("baseline should load after rollback");
    let pending = database
        .sessions()
        .load_pending_focused_review_session_ids(project_id)
        .await
        .expect("pending reviews should load");

    // Assert
    assert!(result.is_err());
    assert_eq!(baseline.as_deref(), Some("42"));
    assert_eq!(pending, [] as [String; 0]);
}

#[tokio::test]
async fn review_diff_baseline_migration_preserves_existing_review_hashes() {
    // Arrange
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("database should open");
    sqlx::raw_sql(
        "CREATE TABLE session (id TEXT, focused_review_diff_hash TEXT); INSERT INTO session \
         VALUES ('reviewed', '42'), ('empty', NULL);",
    )
    .execute(&pool)
    .await
    .expect("legacy rows should exist");

    // Act
    rerun_embedded_migration(&pool, 80).await;
    let rows = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, review_diff_hash FROM session ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("migrated baselines should load");

    // Assert
    assert_eq!(
        rows,
        vec![
            ("empty".to_string(), None),
            ("reviewed".to_string(), Some("42".to_string()))
        ]
    );
}

#[tokio::test]
async fn test_update_session_focused_review_clears_persisted_review() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_focused_review(
            "session-a",
            Some(ag_session::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("## Review\nPersisted".to_string()),
        )
        .await
        .expect("failed to update focused review");

    // Act
    database
        .sessions()
        .update_session_focused_review("session-a", None, None, None)
        .await
        .expect("failed to clear focused review");
    let focused_reviews = database
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load focused reviews");

    // Assert
    assert_eq!(focused_reviews, [] as [crate::SessionFocusedReviewRow; 0]);
}

#[tokio::test]
async fn test_defer_session_focused_review_requires_eligible_existing_session() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_focused_review(
            "session-a",
            Some(ag_session::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("## Review\nOutdated".to_string()),
        )
        .await
        .expect("failed to seed focused review");
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "orchestrator",
            is_draft: false,
            model: "gpt-5.6-sol",
            orchestration_task_id: None,
            parent_session_id: None,
            permission_mode: ag_session::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("Orchestrator"),
            speed_mode: SpeedMode::Normal,
            status: "Review",
        })
        .await
        .expect("failed to insert orchestrator session");

    // Act
    let deferred = database
        .sessions()
        .defer_session_focused_review("session-a")
        .await
        .expect("failed to defer focused review");
    let missing_deferred = database
        .sessions()
        .defer_session_focused_review("missing-session")
        .await
        .expect("failed to check missing session");
    let orchestrator_deferred = database
        .sessions()
        .defer_session_focused_review("orchestrator")
        .await
        .expect("failed to check orchestrator session");
    let pending_session_ids = database
        .sessions()
        .load_pending_focused_review_session_ids(project_id)
        .await
        .expect("failed to load pending focused reviews");
    let focused_reviews = database
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load focused reviews");

    // Assert
    assert!(deferred);
    assert!(!missing_deferred);
    assert!(!orchestrator_deferred);
    assert_eq!(pending_session_ids, ["session-a"]);
    assert_eq!(focused_reviews, [] as [crate::SessionFocusedReviewRow; 0]);
}

#[tokio::test]
/// Verifies transactional turn-metadata persistence rolls back partial
/// writes when any statement in the transaction fails.
async fn test_persist_session_turn_metadata_rolls_back_on_failure() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    sqlx::query!("DROP TABLE session_usage")
        .execute(database.pool())
        .await
        .expect("failed to drop session-usage table");

    // Act
    let result = database
        .sessions()
        .persist_session_turn_metadata(
            "session-a",
            &SessionTurnMetadata {
                applied_personality_id: None,
                applied_personality_prompt_hash: None,
                instruction_conversation_id: Some("instruction-thread".to_string()),
                model: AgentModel::Gpt56Sol.as_str().to_string(),
                provider_conversation_id: Some("thread-123".to_string()),
                questions_json: r#"[{"text":"Need tests?"}]"#.to_string(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 3,
                    output_tokens: 5,
                },
            },
        )
        .await;
    let session = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to reload sessions")
        .into_iter()
        .find(|session| session.id == "session-a")
        .expect("expected seeded session");
    let provider_conversation_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");

    // Assert
    assert!(matches!(result, Err(DbError::Query(_))));
    assert_eq!(session.questions.as_deref(), None);
    assert_eq!(session.input_tokens, 0);
    assert_eq!(session.output_tokens, 0);
    assert_eq!(provider_conversation_id.as_deref(), None);
}

#[tokio::test]
/// Verifies a failed review-operation insert cannot leave a completed turn
/// behind after the database is reopened.
async fn test_persist_session_turn_metadata_and_review_operation_are_restart_atomic() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp directory");
    let db_path = temp_dir.path().join("agentty.db");
    let database = Database::open(&db_path)
        .await
        .expect("failed to open database");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let turn_metadata = |resolution: &str| SessionTurnMetadata {
        applied_personality_id: None,
        applied_personality_prompt_hash: None,
        instruction_conversation_id: None,
        model: AgentModel::Gpt56Sol.as_str().to_string(),
        provider_conversation_id: Some("thread-123".to_string()),
        questions_json: r#"[{"text":"Need tests?"}]"#.to_string(),
        review_comment_resolutions: vec![NewSessionReviewCommentResolution {
            commit_hash: None,
            reply: "Applied the validation.".to_string(),
            reply_token: "token-1".to_string(),
            resolution: resolution.to_string(),
            review_request_display_id: "#42".to_string(),
            thread_id: "thread-1".to_string(),
        }],
        token_usage_delta: SessionStats::default(),
    };

    // Act
    let result = database
        .sessions()
        .persist_session_turn_metadata("session-a", &turn_metadata("invalid"))
        .await;
    database.pool().close().await;
    let database = Database::open(&db_path)
        .await
        .expect("failed to reopen database");
    let session = load_session_row(&database, "session-a").await;
    let operations = database
        .reviews()
        .load_session_review_comment_resolutions("session-a")
        .await
        .expect("failed to load review operations");
    let provider_conversation_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");

    // Assert
    assert!(matches!(result, Err(DbError::Query(_))));
    assert_eq!(session.questions, None);
    assert_eq!(provider_conversation_id, None);
    assert_eq!(operations, Vec::new());

    // Act
    database
        .sessions()
        .persist_session_turn_metadata("session-a", &turn_metadata("fixed"))
        .await
        .expect("failed to persist completed turn and review operation");
    database.pool().close().await;
    let database = Database::open(&db_path)
        .await
        .expect("failed to reopen database after retry");
    let session = load_session_row(&database, "session-a").await;
    let operations = database
        .reviews()
        .load_session_review_comment_resolutions("session-a")
        .await
        .expect("failed to load persisted review operations");
    let provider_conversation_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");

    // Assert
    assert_eq!(
        session.questions.as_deref(),
        Some(r#"[{"text":"Need tests?"}]"#)
    );
    assert_eq!(provider_conversation_id.as_deref(), Some("thread-123"));
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].reply, "Applied the validation.");
    assert_eq!(operations[0].resolution, "fixed");
}

#[tokio::test]
async fn test_set_project_favorite_updates_project_state() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");

    // Act
    database
        .projects()
        .set_project_favorite(project_id, true)
        .await
        .expect("failed to set project favorite");
    let project = database
        .projects()
        .get_project(project_id)
        .await
        .expect("failed to load project")
        .expect("expected existing project");

    // Assert
    assert!(project.is_favorite);
}

#[tokio::test]
async fn query_on_dropped_table_returns_db_error_query() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open database");
    sqlx::query!("DROP TABLE session")
        .execute(database.pool())
        .await
        .expect("failed to drop table");

    // Act
    let result = database.sessions().load_sessions_metadata().await;

    // Assert
    assert!(
        matches!(result, Err(DbError::Query(_))),
        "expected DbError::Query variant"
    );
}

#[tokio::test]
async fn db_error_display_includes_underlying_message() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open database");
    sqlx::query!("DROP TABLE session")
        .execute(database.pool())
        .await
        .expect("failed to drop table");

    // Act
    let result = database.sessions().load_sessions_metadata().await;

    // Assert
    let error = result.expect_err("expected query on dropped table to fail");
    let display_text = error.to_string();
    assert!(
        !display_text.is_empty(),
        "DbError Display should produce a non-empty message"
    );
}

#[tokio::test]
async fn open_with_unwritable_parent_returns_db_error_io() {
    // Arrange — place the database path under a regular file so
    // `create_dir_all` fails with an I/O error.
    let temp = tempdir().expect("failed to create temp directory");
    let blocking_file = temp.path().join("not_a_dir");
    std::fs::write(&blocking_file, b"").expect("failed to create blocking file");
    let db_path = blocking_file.join("nested").join("db.sqlite");

    // Act
    let result = Database::open(&db_path).await;

    // Assert
    assert!(
        matches!(result, Err(DbError::Io(_))),
        "expected DbError::Io variant"
    );
}

#[tokio::test]
async fn open_configures_small_wal_pool_normal_synchronous_mode_and_busy_timeout() {
    // Arrange
    let temp = tempdir().expect("failed to create temp directory");
    let db_path = temp.path().join("agentty.db");

    // Act
    let database = Database::open(&db_path)
        .await
        .expect("failed to open database");
    let journal_mode = sqlx::query_scalar!(
        r#"
SELECT journal_mode || '' AS "journal_mode!: String"
FROM pragma_journal_mode
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load journal mode pragma");
    let synchronous = sqlx::query_scalar!(
        r#"
SELECT synchronous + 0 AS "synchronous!: i64"
FROM pragma_synchronous
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load synchronous pragma");
    let busy_timeout = sqlx::query_scalar!(
        r#"
SELECT timeout + 0 AS "timeout!: i64"
FROM pragma_busy_timeout
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load busy-timeout pragma");

    // Assert
    // `SqliteConnectOptions` are reused for every pooled connection, so
    // checking one pooled connection here is sufficient to prove the
    // configured busy timeout propagates across the on-disk pool.
    assert_eq!(
        database.pool().options().get_max_connections(),
        DB_POOL_MAX_CONNECTIONS
    );
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
    assert_eq!(busy_timeout, 2_000, "expected PRAGMA busy_timeout = 2000");
}

#[tokio::test]
async fn open_in_memory_uses_single_connection_normal_synchronous_mode_and_busy_timeout() {
    // Arrange, Act
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory database");
    let synchronous = sqlx::query_scalar!(
        r#"
SELECT synchronous + 0 AS "synchronous!: i64"
FROM pragma_synchronous
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load synchronous pragma");
    let busy_timeout = sqlx::query_scalar!(
        r#"
SELECT timeout + 0 AS "timeout!: i64"
FROM pragma_busy_timeout
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load busy-timeout pragma");

    // Assert
    assert_eq!(database.pool().options().get_max_connections(), 1);
    assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
    assert_eq!(busy_timeout, 2_000, "expected PRAGMA busy_timeout = 2000");
}

// NOTE: `DbError::Migration` is not directly tested because
// `Database::open` and `Database::open_in_memory` run migrations
// atomically after connecting — there is no injection point to
// pre-corrupt the schema before migrations execute. The `#[from]`
// derive mapping from `sqlx::migrate::MigrateError` is validated
// at compile time by `thiserror`.
