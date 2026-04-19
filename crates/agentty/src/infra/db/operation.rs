//! Session-operation persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::infra::db::{DbError, unix_timestamp_now};

/// Persisted operation lifecycle state for one session command.
pub struct SessionOperationRow {
    pub cancel_requested: bool,
    pub finished_at: Option<i64>,
    pub heartbeat_at: Option<i64>,
    pub id: String,
    pub kind: String,
    pub last_error: Option<String>,
    pub queued_at: i64,
    pub session_id: String,
    pub started_at: Option<i64>,
    pub status: String,
}

/// Session-operation persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait OperationRepository: Send + Sync {
    /// Marks unfinished operations as failed after process restart.
    async fn fail_unfinished_session_operations(&self, reason: &str) -> Result<(), DbError>;

    /// Returns whether cancellation is requested for a specific operation.
    async fn is_cancel_requested_for_operation(&self, operation_id: &str) -> Result<bool, DbError>;

    /// Returns whether an operation is still unfinished.
    async fn is_session_operation_unfinished(&self, operation_id: &str) -> Result<bool, DbError>;

    /// Loads operations still waiting in queue or currently running.
    async fn load_unfinished_session_operations(&self)
    -> Result<Vec<SessionOperationRow>, DbError>;

    /// Marks an operation as canceled.
    async fn mark_session_operation_canceled(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), DbError>;

    /// Marks an operation as completed successfully.
    async fn mark_session_operation_done(&self, operation_id: &str) -> Result<(), DbError>;

    /// Marks an operation as failed with an error message.
    async fn mark_session_operation_failed(
        &self,
        operation_id: &str,
        error: &str,
    ) -> Result<(), DbError>;

    /// Marks an operation as running and refreshes its heartbeat timestamp.
    async fn mark_session_operation_running(&self, operation_id: &str) -> Result<(), DbError>;

    /// Inserts a queued operation row for a session.
    async fn insert_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<(), DbError>;

    /// Requests cancellation for unfinished operations of a session.
    async fn request_cancel_for_session_operations(&self, session_id: &str) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`OperationRepository`].
#[derive(Clone)]
pub(crate) struct SqliteOperationRepository(SqlitePool);

impl SqliteOperationRepository {
    /// Creates an operation repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

/// Row returned when loading one non-null boolean scalar value.
struct RequiredBoolValueRow {
    value: bool,
}

#[async_trait]
impl OperationRepository for SqliteOperationRepository {
    async fn fail_unfinished_session_operations(&self, reason: &str) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query(
            r"
UPDATE session_operation
SET status = 'failed',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = ?,
    cancel_requested = 1
WHERE status IN ('queued', 'running')
",
        )
        .bind(now)
        .bind(now)
        .bind(reason)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn is_cancel_requested_for_operation(&self, operation_id: &str) -> Result<bool, DbError> {
        let row = sqlx::query_as!(
            RequiredBoolValueRow,
            r#"
SELECT EXISTS(
    SELECT 1
    FROM session_operation
    WHERE id = ?
      AND cancel_requested = 1
      AND status IN ('queued', 'running')
) AS "value!: _"
"#,
            operation_id
        )
        .fetch_one(&self.0)
        .await?;

        Ok(row.value)
    }

    async fn is_session_operation_unfinished(&self, operation_id: &str) -> Result<bool, DbError> {
        let row = sqlx::query_as!(
            RequiredBoolValueRow,
            r#"
SELECT EXISTS(
    SELECT 1
    FROM session_operation
    WHERE id = ?
      AND status IN ('queued', 'running')
) AS "value!: _"
"#,
            operation_id
        )
        .fetch_one(&self.0)
        .await?;

        Ok(row.value)
    }

    async fn load_unfinished_session_operations(
        &self,
    ) -> Result<Vec<SessionOperationRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionOperationRow,
            r#"
SELECT id AS "id!", session_id AS "session_id!", kind AS "kind!", status AS "status!",
       queued_at, started_at, finished_at,
       heartbeat_at, last_error,
       cancel_requested AS "cancel_requested: _"
FROM session_operation
WHERE status IN ('queued', 'running')
ORDER BY queued_at ASC, id ASC
            "#
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows)
    }

    async fn mark_session_operation_canceled(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query(
            r"
UPDATE session_operation
SET status = 'canceled',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(now)
        .bind(reason)
        .bind(operation_id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn mark_session_operation_done(&self, operation_id: &str) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query(
            r"
UPDATE session_operation
SET status = 'done',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = NULL
WHERE id = ?
",
        )
        .bind(now)
        .bind(now)
        .bind(operation_id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn mark_session_operation_failed(
        &self,
        operation_id: &str,
        error: &str,
    ) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query(
            r"
UPDATE session_operation
SET status = 'failed',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(now)
        .bind(error)
        .bind(operation_id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn mark_session_operation_running(&self, operation_id: &str) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query(
            r"
UPDATE session_operation
SET status = 'running',
    started_at = COALESCE(started_at, ?),
    heartbeat_at = ?,
    last_error = NULL
WHERE id = ?
",
        )
        .bind(now)
        .bind(now)
        .bind(operation_id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn insert_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<(), DbError> {
        let queued_at = unix_timestamp_now();

        sqlx::query(
            r"
INSERT INTO session_operation (id, session_id, kind, status, queued_at)
VALUES (?, ?, ?, 'queued', ?)
",
        )
        .bind(operation_id)
        .bind(session_id)
        .bind(kind)
        .bind(queued_at)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn request_cancel_for_session_operations(&self, session_id: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session_operation
SET cancel_requested = 1
WHERE session_id = ?
  AND status IN ('queued', 'running')
",
        )
        .bind(session_id)
        .execute(&self.0)
        .await?;

        Ok(())
    }
}
