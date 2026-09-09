use std::io;

use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::file_system::{LocalFileSystem, MockFileSystem};
use crate::session::{Database, NewSession, TurnGuard};
use crate::tool::WriteArguments;
use crate::{ModelError, OutputSchema, TurnError, WriteError};

async fn fixture() -> (Database, TurnGuard) {
    let database = Database::open_in_memory().await.expect("database");
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    database
        .create_session(&NewSession::new("session", schema), None, 4096)
        .await
        .expect("session");
    let acquired = database.begin_turn("session", "write").await.expect("turn");

    (database, acquired.guard)
}

async fn fail(database: &Database, guard: &mut TurnGuard) {
    database
        .fail_turn("session", 0, &TurnError::Model(ModelError::InvalidResponse))
        .await
        .expect("fail turn");
    guard.disarm();
}

fn arguments() -> WriteArguments {
    serde_json::from_value(json!({"path": "file.txt", "patch": "--- /dev/null\n+++ b/file.txt\n@@ -0,0 +1 @@\n+new\n"}))
        .expect("write arguments")
}

#[tokio::test]
async fn pending_writes_are_reconciled_without_reapplying_or_sealing_observations() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let directory = tempdir().expect("repository");
    let root = directory.path().canonicalize().expect("root");
    let tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
    let journal = guard.write_journal();
    journal
        .intent("create", &root, "file.txt", None, b"new\n")
        .await
        .expect("intent");

    // Act and Assert
    let active = WriteRecordRow::load(database.pool(), "session", true, Some(&tool))
        .await
        .expect("records");
    assert_eq!(active[0].recovery, None);
    fail(&database, &mut guard).await;
    for (content, recovery) in [
        (None, WriteRecovery::ExpectedMatches),
        (Some(b"new\n".as_slice()), WriteRecovery::ResultMatches),
        (Some(b"other\n".as_slice()), WriteRecovery::Conflict),
    ] {
        if let Some(content) = content {
            tokio::fs::write(root.join("file.txt"), content)
                .await
                .expect("change target");
        }
        let records = WriteRecordRow::load(database.pool(), "session", true, Some(&tool))
            .await
            .expect("records");
        assert_eq!(records[0].status, WriteStatus::Pending);
        assert_eq!(records[0].recovery, Some(recovery));
    }
    let records = WriteRecordRow::load(database.pool(), "session", true, None)
        .await
        .expect("records");
    assert_eq!(records[0].recovery, Some(WriteRecovery::Unavailable));
    let other = tempdir().expect("other repository");
    let other_tool = WriteTool::new(Arc::new(LocalFileSystem), other.path().to_path_buf());
    let records = WriteRecordRow::load(database.pool(), "session", true, Some(&other_tool))
        .await
        .expect("records");
    assert_eq!(records[0].recovery, Some(WriteRecovery::Unavailable));
}

#[tokio::test]
async fn applied_and_failed_outcomes_survive_failed_turns() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let directory = tempdir().expect("repository");
    let root = directory.path().canonicalize().expect("root");
    let mut tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
    tool.journal = Some(guard.write_journal());

    // Act
    tool.execute(&arguments(), "create").await.expect("write");
    let mut file_system = MockFileSystem::new();
    let canonical_root = root.clone();
    file_system
        .expect_canonicalize()
        .returning(move |_| Ok(canonical_root.clone()));
    file_system
        .expect_open_beneath()
        .returning(|_, _| Err(io::ErrorKind::NotFound.into()));
    file_system
        .expect_replace_beneath()
        .returning(|_, _, _, _| Err(io::Error::other("replacement failed")));
    let mut failing_tool = WriteTool::new(Arc::new(file_system), root.clone());
    failing_tool.journal = Some(guard.write_journal());
    let error = failing_tool
        .execute(&arguments(), "failed")
        .await
        .expect_err("filesystem failure");
    fail(&database, &mut guard).await;
    let records = WriteRecordRow::load(database.pool(), "session", true, Some(&tool))
        .await
        .expect("records");

    // Assert
    assert!(matches!(error, WriteError::WriteTarget { .. }));
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(records[0].recovery, None);
    assert_eq!(records[1].status, WriteStatus::Failed);
    assert_eq!(records[1].recovery, Some(WriteRecovery::ResultMatches));
    assert_eq!(
        content_hash(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[tokio::test]
async fn intent_failure_prevents_mutation_and_outcome_failure_keeps_recoverable_intent() {
    for phase in ["INSERT", "UPDATE"] {
        // Arrange
        let (database, mut guard) = fixture().await;
        let directory = tempdir().expect("repository");
        let root = directory.path().canonicalize().expect("root");
        let mut tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
        tool.journal = Some(guard.write_journal());
        let trigger = if phase == "INSERT" {
            "CREATE TRIGGER fail_journal BEFORE INSERT ON session_write BEGIN SELECT RAISE(FAIL, \
             'journal unavailable'); END"
        } else {
            "CREATE TRIGGER fail_journal BEFORE UPDATE ON session_write BEGIN SELECT RAISE(FAIL, \
             'journal unavailable'); END"
        };
        sqlx::query(trigger)
            .execute(database.pool())
            .await
            .expect("trigger");

        // Act
        let error = tool
            .execute(&arguments(), "create")
            .await
            .expect_err("journal failure");
        fail(&database, &mut guard).await;
        let records = WriteRecordRow::load(database.pool(), "session", true, Some(&tool))
            .await
            .expect("records");

        // Assert
        assert!(matches!(error, WriteError::Journal(_)));
        assert!(!error.is_model_correctable());
        if phase == "INSERT" {
            assert!(!root.join("file.txt").exists());
            assert_eq!(records, Vec::<WriteRecord>::new());
        } else {
            assert_eq!(
                tokio::fs::read(root.join("file.txt"))
                    .await
                    .expect("applied file"),
                b"new\n"
            );
            assert_eq!(records[0].status, WriteStatus::Pending);
            assert_eq!(records[0].recovery, Some(WriteRecovery::ResultMatches));
        }
    }
}

#[tokio::test]
async fn journal_checks_ownership_with_native_roots() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let journal = guard.write_journal();
    let native_root = PathBuf::from(OsString::from_vec(vec![b'/', 0xff]));
    let mut file_system = MockFileSystem::new();
    let canonical_root = native_root.clone();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(move |_| Ok(canonical_root.clone()));
    file_system
        .expect_open_beneath()
        .times(1)
        .withf(|root, path| {
            root.as_os_str().as_bytes() == [47, 255] && path == Path::new("file.txt")
        })
        .returning(|_, _| Ok(Box::new(io::Cursor::new(b"new"))));
    let tool = WriteTool::new(Arc::new(file_system), native_root.clone());

    // Act
    let id = journal
        .intent("call", &native_root, "file.txt", None, b"new")
        .await
        .expect("native root");
    fail(&database, &mut guard).await;
    let lost = journal
        .intent("call", Path::new("repo"), "file.txt", Some(b"old"), b"new")
        .await
        .expect_err("lost ownership");
    let records = WriteRecordRow::load(database.pool(), "session", false, Some(&tool))
        .await
        .expect("native records");

    // Assert
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].repository_root, native_root);
    assert_eq!(records[0].recovery, Some(WriteRecovery::ResultMatches));
    assert_eq!(
        serde_json::to_value(&records[0]).expect("lossless serialization")["repository_root"],
        json!([47, 255])
    );
    assert!(matches!(
        lost,
        SessionError::OwnershipLost {
            turn_position: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn recovery_handles_update_fingerprints_unreadable_targets_and_invalid_storage() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let directory = tempdir().expect("repository");
    let root = directory.path().canonicalize().expect("root");
    let tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
    let journal = guard.write_journal();
    journal
        .intent("update", &root, "file.txt", Some(b"old"), b"new")
        .await
        .expect("intent");
    tokio::fs::write(root.join("file.txt"), b"old")
        .await
        .expect("expected file");
    fail(&database, &mut guard).await;

    // Act and Assert
    let records = WriteRecordRow::load(database.pool(), "session", true, Some(&tool))
        .await
        .expect("records");
    assert_eq!(records[0].expected_hash, Some(content_hash(b"old")));
    assert_eq!(records[0].recovery, Some(WriteRecovery::ExpectedMatches));
    tokio::fs::remove_file(root.join("file.txt"))
        .await
        .expect("remove target");
    tokio::fs::create_dir(root.join("file.txt"))
        .await
        .expect("invalid target");
    let records = WriteRecordRow::load(database.pool(), "session", true, Some(&tool))
        .await
        .expect("records");
    assert_eq!(records[0].recovery, Some(WriteRecovery::Unavailable));
    let missing = WriteTool::new(Arc::new(LocalFileSystem), root.join("missing"));
    assert_eq!(missing.current_hash(&root, "file.txt").await, None);
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(database.pool())
        .await
        .expect("disable check for corruption test");
    sqlx::query("UPDATE session_write SET status = 'invalid'")
        .execute(database.pool())
        .await
        .expect("corrupt status");
    assert!(matches!(
        WriteRecordRow::load(database.pool(), "session", false, None).await,
        Err(SessionError::InvalidData { .. })
    ));
    sqlx::query("DROP TABLE session_write")
        .execute(database.pool())
        .await
        .expect("remove storage");
    assert!(matches!(
        WriteRecordRow::load(database.pool(), "session", false, None).await,
        Err(SessionError::QueryContext { .. })
    ));
}

#[tokio::test]
async fn diagnostic_acknowledgement_is_atomic_and_scoped_to_presented_records() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let journal = guard.write_journal();
    let shown = journal
        .intent("shown", Path::new("/repo"), "one.txt", None, b"one")
        .await
        .expect("shown intent");
    let omitted = journal
        .intent("omitted", Path::new("/repo"), "two.txt", None, b"two")
        .await
        .expect("omitted intent");
    fail(&database, &mut guard).await;
    let mut retry = database
        .begin_turn("session", "retry")
        .await
        .expect("retry");
    sqlx::query(
        "CREATE TRIGGER reject_ack BEFORE UPDATE OF acknowledged_by_turn ON session_write BEGIN \
         SELECT RAISE(FAIL, 'ack unavailable'); END",
    )
    .execute(database.pool())
    .await
    .expect("trigger");

    // Act
    let error = database
        .complete_turn(
            "session",
            retry.turn_position,
            &[],
            Some("new-session"),
            &[shown],
        )
        .await
        .expect_err("acknowledgement failure");
    let pending = WriteRecordRow::load(database.pool(), "session", true, None)
        .await
        .expect("pending diagnostics");
    let before_commit = database
        .load_session("session")
        .await
        .expect("rolled back session");
    sqlx::query("DROP TRIGGER reject_ack")
        .execute(database.pool())
        .await
        .expect("remove trigger");
    database
        .complete_turn(
            "session",
            retry.turn_position,
            &[],
            Some("new-session"),
            &[shown],
        )
        .await
        .expect("atomic completion");
    retry.guard.disarm();
    let remaining = WriteRecordRow::load(database.pool(), "session", true, None)
        .await
        .expect("unacknowledged records");
    let all = WriteRecordRow::load(database.pool(), "session", false, None)
        .await
        .expect("full journal");

    // Assert
    assert!(matches!(
        error,
        SessionError::QueryContext {
            operation: "acknowledge persistent write diagnostics",
            ..
        }
    ));
    assert_eq!(pending.len(), 2);
    assert_eq!(before_commit.provider_session_id, None);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, omitted);
    assert_eq!(all.len(), 2);
    assert_eq!(
        database
            .load_session("session")
            .await
            .expect("committed session")
            .provider_session_id
            .as_deref(),
        Some("new-session")
    );
}

#[tokio::test]
async fn root_migration_preserves_existing_records_and_sequence() {
    // Arrange
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("legacy database");
    for migration in [
        include_str!("../migrations/001_create_session.sql"),
        include_str!("../migrations/002_add_session_turn_lifecycle.sql"),
        include_str!("../migrations/003_add_session_turn_owner_token.sql"),
        include_str!("../migrations/004_add_session_write.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(&pool)
            .await
            .expect("legacy migration");
    }
    sqlx::raw_sql(r#"
INSERT INTO session (id, output_schema, max_history_bytes, created_at, updated_at)
VALUES ('session', '{"type":"object"}', 4096, 0, 0);
INSERT INTO session_turn (session_id, turn_position, status, created_at, updated_at)
VALUES ('session', 0, 'failed', 0, 0);
INSERT INTO session_write (id, session_id, turn_position, call_id, repository_root, path, resulting_hash, status)
VALUES (7, 'session', 0, 'call', '/repo/λ', 'file.txt', 'hash', 'applied'),
   (9, 'session', 0, 'deleted', '/repo/λ', 'file.txt', 'hash', 'pending');
DELETE FROM session_write WHERE id = 9;
"#).execute(&pool).await.expect("legacy data");

    // Act
    sqlx::raw_sql(include_str!(
        "../migrations/005_update_session_write_diagnostics.sql"
    ))
    .execute(&pool)
    .await
    .expect("upgrade journal");
    let records = WriteRecordRow::load(&pool, "session", true, None)
        .await
        .expect("migrated journal");
    let sequence = sqlx::query_scalar::<_, i64>(
        "SELECT seq FROM sqlite_sequence WHERE name = 'session_write'",
    )
    .fetch_one(&pool)
    .await
    .expect("write sequence");

    // Assert
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, 7);
    assert_eq!(records[0].call_id, "call");
    assert_eq!(records[0].repository_root, PathBuf::from("/repo/λ"));
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(sequence, 9);
}
