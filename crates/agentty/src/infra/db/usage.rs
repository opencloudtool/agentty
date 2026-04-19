//! Session-usage persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::domain::session::SessionStats;
use crate::infra::db::DbError;

/// Row returned when loading per-model token usage from the `session_usage`
/// table.
pub struct SessionUsageRow {
    pub created_at: i64,
    pub input_tokens: i64,
    pub invocation_count: i64,
    pub model: String,
    pub output_tokens: i64,
    pub session_id: Option<String>,
}

/// Session-usage persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait UsageRepository: Send + Sync {
    /// Loads per-model token usage rows for a session, ordered by model name.
    async fn load_session_usage(&self, session_id: &str) -> Result<Vec<SessionUsageRow>, DbError>;

    /// Accumulates per-model token usage for a session.
    async fn upsert_session_usage(
        &self,
        session_id: &str,
        model: &str,
        stats: &SessionStats,
    ) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`UsageRepository`].
#[derive(Clone)]
pub(crate) struct SqliteUsageRepository(SqlitePool);

impl SqliteUsageRepository {
    /// Creates a usage repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

#[async_trait]
impl UsageRepository for SqliteUsageRepository {
    async fn load_session_usage(&self, session_id: &str) -> Result<Vec<SessionUsageRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionUsageRow,
            r#"
SELECT session_id, model, created_at, input_tokens, invocation_count, output_tokens
FROM session_usage
WHERE session_id = ?
ORDER BY model
            "#,
            session_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows)
    }

    async fn upsert_session_usage(
        &self,
        session_id: &str,
        model: &str,
        stats: &SessionStats,
    ) -> Result<(), DbError> {
        if stats.input_tokens == 0 && stats.output_tokens == 0 {
            return Ok(());
        }

        sqlx::query(
            r"
INSERT INTO session_usage (session_id, model, input_tokens, output_tokens, invocation_count)
VALUES (?, ?, ?, ?, 1)
ON CONFLICT(session_id, model) DO UPDATE SET
    input_tokens = input_tokens + excluded.input_tokens,
    output_tokens = output_tokens + excluded.output_tokens,
    invocation_count = invocation_count + 1
",
        )
        .bind(session_id)
        .bind(model)
        .bind(stats.input_tokens.cast_signed())
        .bind(stats.output_tokens.cast_signed())
        .execute(&self.0)
        .await?;

        Ok(())
    }
}
