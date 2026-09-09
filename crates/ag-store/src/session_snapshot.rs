//! Transactional session-snapshot persistence store.

use std::sync::Arc;

use sqlx::SqlitePool;

use super::session::ForkSessionSnapshot;
use super::status;
use crate::timestamp::TimestampSource;
use crate::{DbError, DbResultExt};

const FORK_SESSION_SNAPSHOT: &str = "fork session snapshot";

/// Internal store that owns cross-table fork snapshot transactions.
#[derive(Clone)]
pub(super) struct SessionSnapshotStore {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SessionSnapshotStore {
    /// Creates a snapshot store backed by one pool and timestamp source.
    pub(super) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }

    /// Forks durable session state while clearing source-specific linkage.
    pub(super) async fn fork(
        &self,
        snapshot: ForkSessionSnapshot<'_>,
        start_ref: Option<&str>,
    ) -> Result<(), DbError> {
        let ForkSessionSnapshot {
            new_session_id,
            source_session_id,
            status,
        } = snapshot;
        status::validate_session(status)?;
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await.db_context(FORK_SESSION_SNAPSHOT)?;
        let insert_result = sqlx::query(
            r"
INSERT INTO session (
    id, agent, model, base_branch, status, project_id, prompt,
    title, permission_mode, reasoning_level, response_style, speed_mode, added_lines, deleted_lines, has_diff, size,
    input_tokens, output_tokens, is_draft, parent_session_id, personality_id,
    provider_conversation_id, applied_personality_id, applied_personality_prompt_hash,
    app_server_instruction_provider_conversation_id, questions, published_upstream_ref,
    merged_commit_hash, focused_review_text, focused_review_diff_hash,
    stack_base_commit_hash, in_progress_total_seconds, in_progress_started_at,
    created_at, updated_at
)
SELECT ?, agent, model, base_branch, ?, project_id, prompt,
       title, permission_mode, reasoning_level, response_style, speed_mode, 0, 0, NULL, 'XS', 0, 0, 0, NULL, personality_id,
       NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, NULL,
       ?, ?
FROM session
WHERE id = ?
",
        )
        .bind(new_session_id)
        .bind(status)
        .bind(now)
        .bind(now)
        .bind(source_session_id)
        .execute(&mut *transaction)
        .await
        .db_context(FORK_SESSION_SNAPSHOT)?;
        if insert_result.rows_affected() != 1 {
            return Err(DbError::QueryContext {
                operation: FORK_SESSION_SNAPSHOT,
                source: sqlx::Error::RowNotFound,
            });
        }

        sqlx::query!(
            r#"
INSERT INTO session_message (session_id, position, kind, content, created_at)
SELECT ?, position, kind, content, created_at
FROM session_message
WHERE session_id = ?
ORDER BY position, id
"#,
            new_session_id,
            source_session_id,
        )
        .execute(&mut *transaction)
        .await
        .db_context(FORK_SESSION_SNAPSHOT)?;

        if let Some(start_ref) = start_ref {
            sqlx::query(
                "INSERT INTO session_preparation (session_id, state, start_ref) VALUES (?, \
                 'preparing', ?)",
            )
            .bind(new_session_id)
            .bind(start_ref)
            .execute(&mut *transaction)
            .await
            .db_context(FORK_SESSION_SNAPSHOT)?;
        }

        transaction
            .commit()
            .await
            .db_context(FORK_SESSION_SNAPSHOT)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppRepositories;
    use crate::connection::open_in_memory_pool;

    #[tokio::test]
    async fn missing_snapshot_source_reports_semantic_operation_context() {
        // Arrange
        let pool = open_in_memory_pool(1)
            .await
            .expect("failed to open in-memory db");
        let repositories = AppRepositories::from_pool(pool);

        // Act
        let error = repositories
            .sessions()
            .fork_session_snapshot(ForkSessionSnapshot {
                new_session_id: "fork-session",
                source_session_id: "missing-session",
                status: "Draft",
            })
            .await
            .expect_err("fork should fail");

        // Assert
        assert!(matches!(
            error,
            DbError::QueryContext {
                operation: FORK_SESSION_SNAPSHOT,
                source: sqlx::Error::RowNotFound,
            }
        ));
    }
}
