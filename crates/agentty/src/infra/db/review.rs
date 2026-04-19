//! Session review-request persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::domain::session::ReviewRequest;
use crate::infra::db::DbError;

/// Row returned when loading one `session_review_request`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionReviewRequestRow {
    pub display_id: String,
    pub forge_kind: String,
    pub last_refreshed_at: i64,
    pub source_branch: String,
    pub state: String,
    pub status_summary: Option<String>,
    pub target_branch: String,
    pub title: String,
    pub web_url: String,
}

/// Review-request persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait ReviewRepository: Send + Sync {
    /// Updates the persisted forge review-request linkage for a session.
    async fn update_session_review_request(
        &self,
        id: &str,
        review_request: Option<ReviewRequest>,
    ) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`ReviewRepository`].
#[derive(Clone)]
pub(crate) struct SqliteReviewRepository(SqlitePool);

impl SqliteReviewRepository {
    /// Creates a review repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

#[async_trait]
impl ReviewRepository for SqliteReviewRepository {
    async fn update_session_review_request(
        &self,
        id: &str,
        review_request: Option<ReviewRequest>,
    ) -> Result<(), DbError> {
        if let Some(review_request) = review_request.as_ref() {
            sqlx::query(
                r"
INSERT INTO session_review_request (
    session_id,
    display_id,
    forge_kind,
    last_refreshed_at,
    source_branch,
    state,
    status_summary,
    target_branch,
    title,
    web_url
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(session_id) DO UPDATE
SET display_id = excluded.display_id,
    forge_kind = excluded.forge_kind,
    last_refreshed_at = excluded.last_refreshed_at,
    source_branch = excluded.source_branch,
    state = excluded.state,
    status_summary = excluded.status_summary,
    target_branch = excluded.target_branch,
    title = excluded.title,
    web_url = excluded.web_url
",
            )
            .bind(id)
            .bind(review_request.summary.display_id.as_str())
            .bind(review_request.summary.forge_kind.as_str())
            .bind(review_request.last_refreshed_at)
            .bind(review_request.summary.source_branch.as_str())
            .bind(review_request.summary.state.as_str())
            .bind(review_request.summary.status_summary.as_deref())
            .bind(review_request.summary.target_branch.as_str())
            .bind(review_request.summary.title.as_str())
            .bind(review_request.summary.web_url.as_str())
            .execute(&self.0)
            .await?;
        } else {
            sqlx::query(
                r"
DELETE FROM session_review_request
WHERE session_id = ?
",
            )
            .bind(id)
            .execute(&self.0)
            .await?;
        }

        Ok(())
    }
}
