//! Session-activity persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::domain::session::DailyActivity;
use crate::infra::db::{DbError, unix_timestamp_now};

/// Session-activity persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait ActivityRepository: Send + Sync {
    #[cfg(test)]
    /// Rebuilds `session_activity` rows from current `session.created_at`.
    async fn backfill_session_activity_from_sessions(&self) -> Result<(), DbError>;

    #[cfg(test)]
    /// Deletes all rows from `session_activity`.
    async fn clear_session_activity(&self) -> Result<(), DbError>;

    /// Persists one session-creation activity event at a specific Unix
    /// timestamp.
    async fn insert_session_creation_activity_at(
        &self,
        session_id: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError>;

    /// Persists one session-creation activity event at the current Unix
    /// timestamp.
    async fn insert_session_creation_activity_now(&self, session_id: &str) -> Result<(), DbError>;

    /// Loads aggregated session-creation activity counts keyed by local day.
    async fn load_session_activity(&self) -> Result<Vec<DailyActivity>, DbError>;

    #[cfg(test)]
    /// Loads persisted activity event timestamps used for activity stats.
    async fn load_session_activity_timestamps(&self) -> Result<Vec<i64>, DbError>;
}

/// `SQLite` implementation of [`ActivityRepository`].
#[derive(Clone)]
pub(crate) struct SqliteActivityRepository(SqlitePool);

impl SqliteActivityRepository {
    /// Creates an activity repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

/// Row returned when loading aggregated session activity by local day.
struct DailyActivityQueryRow {
    day_key: i64,
    session_count: i64,
}

impl DailyActivityQueryRow {
    /// Converts one aggregate query row into the public daily activity model.
    fn into_daily_activity(self) -> DailyActivity {
        DailyActivity {
            day_key: self.day_key,
            session_count: u32::try_from(self.session_count).unwrap_or(u32::MAX),
        }
    }
}

#[cfg(test)]
/// Row returned when loading one session activity timestamp.
struct TimestampValueRow {
    created_at: i64,
}

#[async_trait]
impl ActivityRepository for SqliteActivityRepository {
    #[cfg(test)]
    async fn backfill_session_activity_from_sessions(&self) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO session_activity (session_id, created_at)
SELECT id, created_at
FROM session
",
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    #[cfg(test)]
    async fn clear_session_activity(&self) -> Result<(), DbError> {
        sqlx::query(
            r"
DELETE FROM session_activity
",
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn insert_session_creation_activity_at(
        &self,
        session_id: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO session_activity (session_id, created_at)
VALUES (?, ?)
ON CONFLICT(session_id) DO NOTHING
",
        )
        .bind(session_id)
        .bind(timestamp_seconds)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn insert_session_creation_activity_now(&self, session_id: &str) -> Result<(), DbError> {
        self.insert_session_creation_activity_at(session_id, unix_timestamp_now())
            .await
    }

    async fn load_session_activity(&self) -> Result<Vec<DailyActivity>, DbError> {
        let rows = sqlx::query_as!(
            DailyActivityQueryRow,
            r#"
SELECT CAST(
           unixepoch(datetime(created_at, 'unixepoch', 'localtime', 'start of day', 'utc')) / 86400
           AS INTEGER
       ) AS "day_key!: _",
       COUNT(*) AS "session_count!: _"
FROM session_activity
WHERE created_at IS NOT NULL
GROUP BY 1
ORDER BY 1
"#
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows
            .into_iter()
            .map(DailyActivityQueryRow::into_daily_activity)
            .collect())
    }

    #[cfg(test)]
    async fn load_session_activity_timestamps(&self) -> Result<Vec<i64>, DbError> {
        let rows = sqlx::query_as!(
            TimestampValueRow,
            r"
SELECT created_at
FROM session_activity
ORDER BY id
",
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows.into_iter().map(|row| row.created_at).collect())
    }
}
