//! Database layer for persisting session metadata using `SQLite` via `SQLx`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::domain::agent::ReasoningLevel;
use crate::domain::session::{DailyActivity, ReviewRequest, SessionStats};
use crate::domain::setting::SettingName;

/// Typed error returned by [`Database`] operations.
///
/// Wraps the underlying `SQLx`, migration, and I/O failures so callers can
/// distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A SQL query or connection-pool operation failed.
    #[error("{0}")]
    Query(#[from] sqlx::Error),

    /// An embedded schema migration failed during database open.
    #[error("{0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// A filesystem operation failed (e.g. creating the database directory).
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Subdirectory under the agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default database filename.
pub const DB_FILE: &str = "agentty.db";

/// Maximum number of pooled `SQLite` connections for the on-disk database.
///
/// A value greater than `1` allows read operations to continue while
/// background writers flush session output.
pub const DB_POOL_MAX_CONNECTIONS: u32 = 10;

/// Thin wrapper around a `SQLite` connection pool providing query methods.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

/// Row returned when loading a project from the `project` table.
pub struct ProjectRow {
    pub created_at: i64,
    pub display_name: Option<String>,
    pub git_branch: Option<String>,
    pub id: i64,
    pub is_favorite: bool,
    pub last_opened_at: Option<i64>,
    pub path: String,
    pub updated_at: i64,
}

/// Row returned when loading one project with aggregated session statistics.
pub struct ProjectListRow {
    pub active_session_count: i64,
    pub created_at: i64,
    pub display_name: Option<String>,
    pub git_branch: Option<String>,
    pub id: i64,
    pub is_favorite: bool,
    pub last_opened_at: Option<i64>,
    pub last_session_updated_at: Option<i64>,
    pub path: String,
    pub session_count: i64,
    pub updated_at: i64,
}

/// Macro-mapped row returned when loading one project with optional joined
/// session aggregates.
struct ProjectListQueryRow {
    active_session_count: Option<i64>,
    created_at: i64,
    display_name: Option<String>,
    git_branch: Option<String>,
    id: i64,
    is_favorite: bool,
    last_opened_at: Option<i64>,
    last_session_updated_at: Option<i64>,
    path: String,
    session_count: Option<i64>,
    updated_at: i64,
}

impl ProjectListQueryRow {
    /// Converts the optional joined aggregate values into the public project
    /// list row shape.
    fn into_project_list_row(self) -> ProjectListRow {
        let Self {
            active_session_count,
            created_at,
            display_name,
            git_branch,
            id,
            is_favorite,
            last_opened_at,
            last_session_updated_at,
            path,
            session_count,
            updated_at,
        } = self;

        ProjectListRow {
            active_session_count: active_session_count.unwrap_or(0),
            created_at,
            display_name,
            git_branch,
            id,
            is_favorite,
            last_opened_at,
            last_session_updated_at,
            path,
            session_count: session_count.unwrap_or(0),
            updated_at,
        }
    }
}

/// Row returned when loading one `session_review_request`.
#[derive(Clone, Debug, Eq, PartialEq, sqlx::FromRow)]
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

/// Row returned when loading a session from the `session` table.
///
/// Includes optional normalized forge review-request linkage metadata loaded
/// through the `session_review_request` table when the session has been
/// published for remote review.
pub struct SessionRow {
    pub base_branch: String,
    pub created_at: i64,
    pub id: String,
    pub input_tokens: i64,
    pub model: String,
    pub output: String,
    pub output_tokens: i64,
    pub project_id: Option<i64>,
    pub prompt: String,
    pub published_upstream_ref: Option<String>,
    pub questions: Option<String>,
    pub review_request: Option<SessionReviewRequestRow>,
    pub size: String,
    pub status: String,
    pub summary: Option<String>,
    pub title: Option<String>,
    pub updated_at: i64,
}

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

/// Row returned when loading one session activity timestamp.
struct TimestampValueRow {
    pub created_at: i64,
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

/// Row returned when loading both persisted timestamps for one session.
struct SessionTimestampsRow {
    created_at: i64,
    updated_at: i64,
}

/// Row returned when loading an optional `i64` scalar value.
struct OptionalI64ValueRow {
    value: Option<i64>,
}

/// Row returned when loading an optional string scalar value.
struct OptionalStringValueRow {
    value: Option<String>,
}

/// Row returned when loading a required string scalar value.
struct RequiredStringValueRow {
    value: String,
}

/// Row returned when loading session count and latest-update metadata.
struct SessionMetadataRow {
    max_updated_at: i64,
    session_count: i64,
}

/// Row returned when loading one non-null boolean scalar value.
struct RequiredBoolValueRow {
    value: bool,
}

/// Row returned when loading one non-null `i64` scalar value.
struct RequiredI64ValueRow {
    value: i64,
}

/// Row returned when loading one `session` plus aliased
/// `session_review_request` join columns.
struct SessionJoinRow {
    base_branch: String,
    created_at: i64,
    id: String,
    input_tokens: i64,
    model: String,
    output: String,
    output_tokens: i64,
    project_id: Option<i64>,
    prompt: String,
    published_upstream_ref: Option<String>,
    questions: Option<String>,
    review_request_display_id: Option<String>,
    review_request_forge_kind: Option<String>,
    review_request_last_refreshed_at: Option<i64>,
    review_request_source_branch: Option<String>,
    review_request_state: Option<String>,
    review_request_status_summary: Option<String>,
    review_request_target_branch: Option<String>,
    review_request_title: Option<String>,
    review_request_web_url: Option<String>,
    size: String,
    status: String,
    summary: Option<String>,
    title: Option<String>,
    updated_at: i64,
}

impl SessionJoinRow {
    /// Converts the macro-mapped join row into the public `SessionRow` model.
    fn into_session_row(self) -> SessionRow {
        let Self {
            base_branch,
            created_at,
            id,
            input_tokens,
            model,
            output,
            output_tokens,
            project_id,
            prompt,
            published_upstream_ref,
            questions,
            review_request_display_id,
            review_request_forge_kind,
            review_request_last_refreshed_at,
            review_request_source_branch,
            review_request_state,
            review_request_status_summary,
            review_request_target_branch,
            review_request_title,
            review_request_web_url,
            size,
            status,
            summary,
            title,
            updated_at,
        } = self;

        let review_request = SessionReviewRequestJoinRow {
            display_id: review_request_display_id,
            forge_kind: review_request_forge_kind,
            last_refreshed_at: review_request_last_refreshed_at,
            source_branch: review_request_source_branch,
            state: review_request_state,
            status_summary: review_request_status_summary,
            target_branch: review_request_target_branch,
            title: review_request_title,
            web_url: review_request_web_url,
        }
        .into_review_request_row();

        SessionRow {
            base_branch,
            created_at,
            id,
            input_tokens,
            model,
            output,
            output_tokens,
            project_id,
            prompt,
            published_upstream_ref,
            questions,
            review_request,
            size,
            status,
            summary,
            title,
            updated_at,
        }
    }
}

/// Aliased nullable `session_review_request` columns loaded through a joined
/// session query.
struct SessionReviewRequestJoinRow {
    display_id: Option<String>,
    forge_kind: Option<String>,
    last_refreshed_at: Option<i64>,
    source_branch: Option<String>,
    state: Option<String>,
    status_summary: Option<String>,
    target_branch: Option<String>,
    title: Option<String>,
    web_url: Option<String>,
}

impl SessionReviewRequestJoinRow {
    /// Converts the joined nullable columns into a review-request row only
    /// when every required field is present.
    fn into_review_request_row(self) -> Option<SessionReviewRequestRow> {
        let Self {
            display_id,
            forge_kind,
            last_refreshed_at,
            source_branch,
            state,
            status_summary,
            target_branch,
            title,
            web_url,
        } = self;

        Some(SessionReviewRequestRow {
            display_id: display_id?,
            forge_kind: forge_kind?,
            last_refreshed_at: last_refreshed_at?,
            source_branch: source_branch?,
            state: state?,
            status_summary,
            target_branch: target_branch?,
            title: title?,
            web_url: web_url?,
        })
    }
}

impl Database {
    /// Opens the `SQLite` database and runs embedded migrations.
    ///
    /// Uses up to `DB_POOL_MAX_CONNECTIONS` pooled connections so UI reads do
    /// not serialize behind frequent background writes.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database cannot
    /// be opened, or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self { pool })
    }

    /// Returns the shared `SQLite` connection pool for lower-level query
    /// access.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Inserts or updates a project by path and returns its identifier.
    ///
    /// # Errors
    /// Returns an error if the project row cannot be written or read.
    pub async fn upsert_project(
        &self,
        path: &str,
        git_branch: Option<&str>,
    ) -> Result<i64, DbError> {
        sqlx::query(
            r"
INSERT INTO project (path, git_branch, created_at, updated_at)
VALUES (?, ?, unixepoch(), unixepoch())
ON CONFLICT(path) DO UPDATE
SET git_branch = excluded.git_branch,
    updated_at = unixepoch()
",
        )
        .bind(path)
        .bind(git_branch)
        .execute(&self.pool)
        .await?;

        let row = sqlx::query_as!(
            RequiredI64ValueRow,
            r#"
SELECT id AS "value!: _"
FROM project
WHERE path = ?
"#,
            path
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.value)
    }

    /// Looks up a project by identifier.
    ///
    /// # Errors
    /// Returns an error if the project lookup query fails.
    pub async fn get_project(&self, id: i64) -> Result<Option<ProjectRow>, DbError> {
        let row = sqlx::query_as!(
            ProjectRow,
            r#"
SELECT created_at,
       display_name,
       git_branch,
       id,
       is_favorite AS "is_favorite: _",
       last_opened_at,
       path,
       updated_at
FROM project
WHERE id = ?
"#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    /// Loads all configured projects with aggregated session stats.
    ///
    /// # Errors
    /// Returns an error if project rows cannot be read from the database.
    pub async fn load_projects_with_stats(&self) -> Result<Vec<ProjectListRow>, DbError> {
        let rows = sqlx::query_as!(
            ProjectListQueryRow,
            r#"
WITH stats AS (
    SELECT project_id,
           MAX(updated_at) AS last_session_updated_at,
           COUNT(*) AS session_count,
           COUNT(CASE WHEN status NOT IN ('Done', 'Canceled', 'Queued', 'Merging')
                      THEN 1 END) AS active_session_count
    FROM session
    WHERE project_id IS NOT NULL
    GROUP BY project_id
)
SELECT stats.active_session_count,
       p.created_at AS "created_at!",
       p.display_name,
       p.git_branch,
       p.id AS "id!",
       p.is_favorite AS "is_favorite: _",
       p.last_opened_at,
       stats.last_session_updated_at,
       p.path,
       stats.session_count,
       p.updated_at AS "updated_at!"
FROM project AS p
LEFT JOIN stats
ON stats.project_id = p.id
ORDER BY p.is_favorite DESC,
         COALESCE(p.last_opened_at, 0) DESC,
         p.path
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(ProjectListQueryRow::into_project_list_row)
            .collect())
    }

    /// Marks a project as recently opened at the current Unix timestamp.
    ///
    /// # Errors
    /// Returns an error if the project row cannot be updated.
    pub async fn touch_project_last_opened(&self, project_id: i64) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query(
            r"
UPDATE project
SET last_opened_at = ?,
    updated_at = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(now)
        .bind(project_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates favorite state for one project.
    ///
    /// # Errors
    /// Returns an error if the project row cannot be updated.
    pub async fn set_project_favorite(
        &self,
        project_id: i64,
        is_favorite: bool,
    ) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query(
            r"
UPDATE project
SET is_favorite = ?,
    updated_at = ?
WHERE id = ?
",
        )
        .bind(i64::from(is_favorite))
        .bind(now)
        .bind(project_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Inserts a newly created session row.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be inserted.
    pub async fn insert_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO session (id, model, base_branch, status, project_id, prompt, output)
VALUES (?, ?, ?, ?, ?, ?, ?)
",
        )
        .bind(id)
        .bind(model)
        .bind(base_branch)
        .bind(status)
        .bind(project_id)
        .bind("")
        .bind("")
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Persists one session-creation activity event at the current Unix
    /// timestamp.
    ///
    /// Duplicate events for the same session are ignored.
    ///
    /// # Errors
    /// Returns an error if the activity event cannot be inserted.
    pub async fn insert_session_creation_activity_now(
        &self,
        session_id: &str,
    ) -> Result<(), DbError> {
        self.insert_session_creation_activity_at(session_id, unix_timestamp_now())
            .await
    }

    /// Persists one session-creation activity event at a specific Unix
    /// timestamp.
    ///
    /// Duplicate events for the same session are ignored.
    ///
    /// # Errors
    /// Returns an error if the activity event cannot be inserted.
    pub async fn insert_session_creation_activity_at(
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Loads all sessions ordered by most recent update.
    ///
    /// # Errors
    /// Returns an error if session rows cannot be read from the database.
    pub async fn load_sessions_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionJoinRow,
            r#"
SELECT session.base_branch AS "base_branch!",
       session.created_at AS "created_at!",
       session.id AS "id!",
       session.input_tokens AS "input_tokens!",
       session.model AS "model!",
       session.output AS "output!",
       session.output_tokens AS "output_tokens!",
       session.project_id,
       session.prompt AS "prompt!",
       session.published_upstream_ref,
       session.questions,
       session_review_request.display_id AS "review_request_display_id?",
       session_review_request.forge_kind AS "review_request_forge_kind?",
       session_review_request.last_refreshed_at AS "review_request_last_refreshed_at?",
       session_review_request.source_branch AS "review_request_source_branch?",
       session_review_request.state AS "review_request_state?",
       session_review_request.status_summary AS "review_request_status_summary?",
       session_review_request.target_branch AS "review_request_target_branch?",
       session_review_request.title AS "review_request_title?",
       session_review_request.web_url AS "review_request_web_url?",
       session.size AS "size!",
       session.status AS "status!",
       session.summary,
       session.title,
       session.updated_at AS "updated_at!"
FROM session
LEFT JOIN session_review_request
ON session_review_request.session_id = session.id
WHERE session.project_id = ?
ORDER BY session.updated_at DESC, session.id
"#,
            project_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(SessionJoinRow::into_session_row)
            .collect())
    }

    /// Loads all sessions ordered by most recent update.
    ///
    /// # Errors
    /// Returns an error if session rows cannot be read from the database.
    pub async fn load_sessions(&self) -> Result<Vec<SessionRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionJoinRow,
            r#"
SELECT session.base_branch AS "base_branch!",
       session.created_at AS "created_at!",
       session.id AS "id!",
       session.input_tokens AS "input_tokens!",
       session.model AS "model!",
       session.output AS "output!",
       session.output_tokens AS "output_tokens!",
       session.project_id,
       session.prompt AS "prompt!",
       session.published_upstream_ref,
       session.questions,
       session_review_request.display_id AS "review_request_display_id?",
       session_review_request.forge_kind AS "review_request_forge_kind?",
       session_review_request.last_refreshed_at AS "review_request_last_refreshed_at?",
       session_review_request.source_branch AS "review_request_source_branch?",
       session_review_request.state AS "review_request_state?",
       session_review_request.status_summary AS "review_request_status_summary?",
       session_review_request.target_branch AS "review_request_target_branch?",
       session_review_request.title AS "review_request_title?",
       session_review_request.web_url AS "review_request_web_url?",
       session.size AS "size!",
       session.status AS "status!",
       session.summary,
       session.title,
       session.updated_at AS "updated_at!"
FROM session
LEFT JOIN session_review_request
ON session_review_request.session_id = session.id
ORDER BY session.updated_at DESC, session.id
"#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(SessionJoinRow::into_session_row)
            .collect())
    }

    /// Loads persisted activity event timestamps used for activity stats.
    ///
    /// # Errors
    /// Returns an error if activity timestamps cannot be read from the
    /// database.
    pub async fn load_session_activity_timestamps(&self) -> Result<Vec<i64>, DbError> {
        let rows = sqlx::query_as!(
            TimestampValueRow,
            r"
SELECT created_at
FROM session_activity
ORDER BY id
",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|row| row.created_at).collect())
    }

    /// Loads aggregated session-creation activity counts keyed by local day.
    ///
    /// Activity history stays available after session deletion because counts
    /// are sourced from immutable `session_activity` rows instead of live
    /// `session` records.
    ///
    /// # Errors
    /// Returns an error if daily activity cannot be aggregated from the
    /// database.
    pub async fn load_session_activity(&self) -> Result<Vec<DailyActivity>, DbError> {
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
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(DailyActivityQueryRow::into_daily_activity)
            .collect())
    }

    /// Loads lightweight session metadata used for cheap change detection.
    ///
    /// Returns `(session_count, max_updated_at)` from the `session` table so
    /// callers can decide whether a full `load_sessions()` refresh is needed.
    ///
    /// # Errors
    /// Returns an error if metadata cannot be queried from the database.
    pub async fn load_sessions_metadata(&self) -> Result<(i64, i64), DbError> {
        let row = sqlx::query_as!(
            SessionMetadataRow,
            r#"
SELECT (SELECT COUNT(*) FROM session) AS "session_count!: _",
       COALESCE(
           (
               SELECT updated_at
               FROM session
               ORDER BY updated_at DESC, id
               LIMIT 1
           ),
           0
       ) AS "max_updated_at!: _"
"#
        )
        .fetch_one(&self.pool)
        .await?;

        Ok((row.session_count, row.max_updated_at))
    }

    /// Deletes a session row by identifier.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be deleted.
    pub async fn delete_session(&self, id: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
DELETE FROM session
WHERE id = ?
",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Updates the status for a session row.
    ///
    /// # Errors
    /// Returns an error if the status update fails.
    pub async fn update_session_status(&self, id: &str, status: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET status = ?
WHERE id = ?
",
        )
        .bind(status)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Overrides the `updated_at` timestamp for one session row.
    ///
    /// This is primarily used by deterministic ordering tests.
    ///
    /// # Errors
    /// Returns an error if the timestamp update fails.
    pub async fn update_session_updated_at(
        &self,
        id: &str,
        updated_at: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(updated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Overrides the `created_at` timestamp for one session row.
    ///
    /// This is primarily used by activity aggregation tests.
    ///
    /// # Errors
    /// Returns an error if the timestamp update fails.
    pub async fn update_session_created_at(
        &self,
        id: &str,
        created_at: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET created_at = ?
WHERE id = ?
",
        )
        .bind(created_at)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Deletes all rows from `session_activity`.
    ///
    /// # Errors
    /// Returns an error if deleting activity rows fails.
    pub async fn clear_session_activity(&self) -> Result<(), DbError> {
        sqlx::query(
            r"
DELETE FROM session_activity
",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Rebuilds `session_activity` rows from current `session.created_at`.
    ///
    /// # Errors
    /// Returns an error if backfilling activity rows fails.
    pub async fn backfill_session_activity_from_sessions(&self) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO session_activity (session_id, created_at)
SELECT id, created_at
FROM session
",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the size bucket for a session row.
    ///
    /// The update is skipped when the stored value already matches `size`.
    ///
    /// # Errors
    /// Returns an error if the size update fails.
    pub async fn update_session_size(&self, id: &str, size: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET size = ?
WHERE id = ?
  AND size <> ?
",
        )
        .bind(size)
        .bind(id)
        .bind(size)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the model clarification questions for a session row.
    ///
    /// # Errors
    /// Returns an error if the questions update fails.
    pub async fn update_session_questions(&self, id: &str, questions: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET questions = ?
WHERE id = ?
",
        )
        .bind(questions)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the saved prompt for a session row.
    ///
    /// # Errors
    /// Returns an error if the prompt update fails.
    pub async fn update_session_prompt(&self, id: &str, prompt: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET prompt = ?
WHERE id = ?
",
        )
        .bind(prompt)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the display title for a session row.
    ///
    /// # Errors
    /// Returns an error if the title update fails.
    pub async fn update_session_title(&self, id: &str, title: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET title = ?
WHERE id = ?
",
        )
        .bind(title)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the persisted session summary text for a session row.
    ///
    /// This field stores the raw agent `summary` payload during
    /// review/question states and, once the session reaches `Done`, the merge
    /// workflow rewrites it into markdown with `# Summary` and `# Commit`
    /// sections.
    ///
    /// # Errors
    /// Returns an error if the summary update fails.
    pub async fn update_session_summary(&self, id: &str, summary: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET summary = ?
WHERE id = ?
",
        )
        .bind(summary)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Accumulates token statistics for a session.
    ///
    /// Each call **adds** the provided values to the existing totals so that
    /// per-invocation stats reported by the agent CLI are summed over the
    /// lifetime of the session.
    ///
    /// # Errors
    /// Returns an error if the stats update fails.
    pub async fn update_session_stats(
        &self,
        id: &str,
        stats: &SessionStats,
    ) -> Result<(), DbError> {
        if stats.input_tokens == 0 && stats.output_tokens == 0 {
            return Ok(());
        }

        sqlx::query(
            r"
UPDATE session
SET input_tokens = input_tokens + ?,
    output_tokens = output_tokens + ?
WHERE id = ?
",
        )
        .bind(stats.input_tokens.cast_signed())
        .bind(stats.output_tokens.cast_signed())
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the persisted model for a session.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_model(&self, id: &str, model: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET model = ?
WHERE id = ?
",
        )
        .bind(model)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the persisted provider conversation identifier for a session.
    ///
    /// The identifier stores the provider-native thread/session id used to
    /// resume app-server context without transcript replay after runtime
    /// restart.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_provider_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET provider_conversation_id = ?
WHERE id = ?
",
        )
        .bind(provider_conversation_id)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Updates the persisted upstream reference for a published session
    /// branch.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_published_upstream_ref(
        &self,
        id: &str,
        published_upstream_ref: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET published_upstream_ref = ?
WHERE id = ?
",
        )
        .bind(published_upstream_ref)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Loads the persisted forge review-request linkage for one session.
    ///
    /// Returns `Ok(None)` when the session has no linked review request.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub async fn load_session_review_request(
        &self,
        session_id: &str,
    ) -> Result<Option<ReviewRequest>, DbError> {
        let row: Option<SessionReviewRequestRow> = sqlx::query_as(
            r"
SELECT display_id, forge_kind, last_refreshed_at, source_branch, state,
       status_summary, target_branch, title, web_url
FROM session_review_request
WHERE session_id = ?
",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|row| {
            let forge_kind = row.forge_kind.parse().ok()?;
            let state = row.state.parse().ok()?;

            Some(ReviewRequest {
                last_refreshed_at: row.last_refreshed_at,
                summary: ag_forge::ReviewRequestSummary {
                    display_id: row.display_id,
                    forge_kind,
                    source_branch: row.source_branch,
                    state,
                    status_summary: row.status_summary,
                    target_branch: row.target_branch,
                    title: row.title,
                    web_url: row.web_url,
                },
            })
        }))
    }

    /// Updates the persisted forge review-request linkage for a session.
    ///
    /// Passing `None` deletes the linked `session_review_request` row. Local
    /// session status transitions should keep the link intact by persisting the
    /// latest metadata instead of clearing it.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_review_request(
        &self,
        id: &str,
        review_request: Option<&ReviewRequest>,
    ) -> Result<(), DbError> {
        if let Some(review_request) = review_request {
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
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                r"
DELETE FROM session_review_request
WHERE session_id = ?
",
            )
            .bind(id)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    /// Replaces the full output for a session row.
    ///
    /// Used when an operation needs to rewrite the persisted transcript
    /// instead of appending incremental chunks.
    ///
    /// # Errors
    /// Returns an error if the output update fails.
    pub async fn replace_session_output(&self, id: &str, output: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET output = ?
WHERE id = ?
",
        )
        .bind(output)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Appends text to the saved output for a session row.
    ///
    /// # Errors
    /// Returns an error if the output append update fails.
    pub async fn append_session_output(&self, id: &str, chunk: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET output = output || ?
WHERE id = ?
",
        )
        .bind(chunk)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Sets `project_id` for sessions that do not yet reference a project.
    ///
    /// # Errors
    /// Returns an error if the backfill update fails.
    pub async fn backfill_session_project(&self, project_id: i64) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET project_id = ?
WHERE project_id IS NULL
",
        )
        .bind(project_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Returns the persisted base branch for a session, when present.
    ///
    /// # Errors
    /// Returns an error if the base branch lookup query fails.
    pub async fn get_session_base_branch(&self, id: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as!(
            RequiredStringValueRow,
            r#"
SELECT base_branch AS "value!: _"
FROM session
WHERE id = ?
"#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| row.value))
    }

    /// Returns the provider conversation identifier for a session, when
    /// present.
    ///
    /// # Errors
    /// Returns an error if the lookup query fails.
    pub async fn get_session_provider_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as!(
            OptionalStringValueRow,
            r#"
SELECT provider_conversation_id AS "value: _"
FROM session
WHERE id = ?
"#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|row| row.value))
    }

    /// Inserts a queued operation row for a session.
    ///
    /// # Errors
    /// Returns an error if the operation row cannot be inserted.
    pub async fn insert_session_operation(
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Loads operations still waiting in queue or currently running.
    ///
    /// # Errors
    /// Returns an error if operation rows cannot be read.
    pub async fn load_unfinished_session_operations(
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
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    /// Returns whether an operation is still unfinished.
    ///
    /// Unfinished means the operation is in `queued` or `running` status.
    ///
    /// # Errors
    /// Returns an error if operation state cannot be read.
    pub async fn is_session_operation_unfinished(
        &self,
        operation_id: &str,
    ) -> Result<bool, DbError> {
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
        .fetch_one(&self.pool)
        .await?;

        Ok(row.value)
    }

    /// Marks an operation as running and refreshes its heartbeat timestamp.
    ///
    /// # Errors
    /// Returns an error if the operation row cannot be updated.
    pub async fn mark_session_operation_running(&self, operation_id: &str) -> Result<(), DbError> {
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Marks an operation as completed successfully.
    ///
    /// # Errors
    /// Returns an error if the operation row cannot be updated.
    pub async fn mark_session_operation_done(&self, operation_id: &str) -> Result<(), DbError> {
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Marks an operation as failed with an error message.
    ///
    /// # Errors
    /// Returns an error if the operation row cannot be updated.
    pub async fn mark_session_operation_failed(
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Marks an operation as canceled.
    ///
    /// # Errors
    /// Returns an error if the operation row cannot be updated.
    pub async fn mark_session_operation_canceled(
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Requests cancellation for unfinished operations of a session.
    ///
    /// # Errors
    /// Returns an error if the operation rows cannot be updated.
    pub async fn request_cancel_for_session_operations(
        &self,
        session_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session_operation
SET cancel_requested = 1
WHERE session_id = ?
  AND status IN ('queued', 'running')
",
        )
        .bind(session_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Returns whether cancellation is requested for unfinished session
    /// operations.
    ///
    /// # Errors
    /// Returns an error if cancellation state cannot be read.
    pub async fn is_cancel_requested_for_session_operations(
        &self,
        session_id: &str,
    ) -> Result<bool, DbError> {
        let row = sqlx::query_as!(
            RequiredBoolValueRow,
            r#"
SELECT EXISTS(
    SELECT 1
    FROM session_operation
    WHERE session_id = ?
      AND cancel_requested = 1
      AND status IN ('queued', 'running')
) AS "value!: _"
"#,
            session_id
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.value)
    }

    /// Marks unfinished operations as failed after process restart.
    ///
    /// # Errors
    /// Returns an error if operation rows cannot be updated.
    pub async fn fail_unfinished_session_operations(&self, reason: &str) -> Result<(), DbError> {
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Inserts or updates a setting by name.
    ///
    /// # Errors
    /// Returns an error if the setting row cannot be written.
    pub async fn upsert_setting(&self, name: &str, value: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
        )
        .bind(name)
        .bind(value)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Inserts or updates one project-scoped setting by project and name.
    ///
    /// # Errors
    /// Returns an error if the project-setting row cannot be written.
    pub async fn upsert_project_setting(
        &self,
        project_id: i64,
        name: &str,
        value: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE
SET value = excluded.value
",
        )
        .bind(project_id)
        .bind(name)
        .bind(value)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Looks up a setting value by name.
    ///
    /// # Errors
    /// Returns an error if the setting lookup query fails.
    pub async fn get_setting(&self, name: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as!(
            RequiredStringValueRow,
            r#"
SELECT value AS "value!: _"
FROM setting
WHERE name = ?
"#,
            name
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| row.value))
    }

    /// Looks up one project-scoped setting value by project and name.
    ///
    /// # Errors
    /// Returns an error if the project-setting lookup query fails.
    pub async fn get_project_setting(
        &self,
        project_id: i64,
        name: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as!(
            RequiredStringValueRow,
            r#"
SELECT value AS "value!: _"
FROM project_setting
WHERE project_id = ?
  AND name = ?
"#,
            project_id,
            name
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| row.value))
    }

    /// Persists one project-scoped reasoning-effort setting.
    ///
    /// # Errors
    /// Returns an error if project settings persistence fails.
    pub async fn set_project_reasoning_level(
        &self,
        project_id: i64,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError> {
        self.upsert_project_setting(
            project_id,
            SettingName::ReasoningLevel.as_str(),
            reasoning_level.codex(),
        )
        .await
    }

    /// Loads one project-scoped reasoning-effort setting.
    ///
    /// Missing or unparsable values fall back to [`ReasoningLevel::default`].
    ///
    /// # Errors
    /// Returns an error if project settings lookup fails.
    pub async fn load_project_reasoning_level(
        &self,
        project_id: i64,
    ) -> Result<ReasoningLevel, DbError> {
        let setting_value = self
            .get_project_setting(project_id, SettingName::ReasoningLevel.as_str())
            .await?;

        let reasoning_level = setting_value
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok())
            .unwrap_or_default();

        Ok(reasoning_level)
    }

    /// Persists the global reasoning-effort setting.
    ///
    /// # Errors
    /// Returns an error if settings persistence fails.
    pub async fn set_reasoning_level(
        &self,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError> {
        self.upsert_setting(
            SettingName::ReasoningLevel.as_str(),
            reasoning_level.codex(),
        )
        .await
    }

    /// Loads the persisted reasoning-effort setting.
    ///
    /// Missing or unparsable values fall back to [`ReasoningLevel::default`].
    ///
    /// # Errors
    /// Returns an error if settings lookup fails.
    pub async fn load_reasoning_level(&self) -> Result<ReasoningLevel, DbError> {
        let setting_value = self
            .get_setting(SettingName::ReasoningLevel.as_str())
            .await?;

        let reasoning_level = setting_value
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok())
            .unwrap_or_default();

        Ok(reasoning_level)
    }

    /// Loads the project identifier associated with one session.
    ///
    /// # Errors
    /// Returns an error if the session lookup query fails.
    pub async fn load_session_project_id(&self, session_id: &str) -> Result<Option<i64>, DbError> {
        let row = sqlx::query_as!(
            OptionalI64ValueRow,
            r#"
SELECT project_id AS "value: _"
FROM session
WHERE id = ?
"#,
            session_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|row| row.value))
    }

    /// Loads the persisted summary text associated with one session.
    ///
    /// # Errors
    /// Returns an error if the session summary lookup query fails.
    pub async fn load_session_summary(&self, session_id: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query_scalar::<_, Option<String>>(
            r"
SELECT summary
FROM session
WHERE id = ?
",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.flatten())
    }

    /// Persists the active project identifier in application settings.
    ///
    /// # Errors
    /// Returns an error if settings persistence fails.
    pub async fn set_active_project_id(&self, project_id: i64) -> Result<(), DbError> {
        self.upsert_setting("ActiveProjectId", &project_id.to_string())
            .await
    }

    /// Loads the active project identifier from application settings.
    ///
    /// # Errors
    /// Returns an error if settings lookup fails.
    pub async fn load_active_project_id(&self) -> Result<Option<i64>, DbError> {
        let setting_value = sqlx::query_as!(
            RequiredStringValueRow,
            r#"
SELECT value AS "value!: _"
FROM setting
WHERE name = 'ActiveProjectId'
"#
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|row| row.value);

        Ok(setting_value.and_then(|value| value.parse::<i64>().ok()))
    }

    /// Accumulates per-model token usage for a session.
    ///
    /// Each call inserts a new row if the `(session_id, model)` pair does not
    /// exist, or adds the provided values to the existing totals.
    /// `invocation_count` is incremented by 1 on each call.
    ///
    /// # Errors
    /// Returns an error if the upsert fails.
    pub async fn upsert_session_usage(
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
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Loads per-model token usage rows for a session, ordered by model name.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn load_session_usage(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionUsageRow>, DbError> {
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
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    /// Returns `(created_at, updated_at)` timestamps for a session.
    ///
    /// Returns `None` if the session does not exist.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn load_session_timestamps(
        &self,
        session_id: &str,
    ) -> Result<Option<(i64, i64)>, DbError> {
        let row = sqlx::query_as!(
            SessionTimestampsRow,
            r#"
SELECT created_at, updated_at
FROM session
WHERE id = ?
            "#,
            session_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| (row.created_at, row.updated_at)))
    }
}

fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

impl Database {
    /// Opens an in-memory `SQLite` database and runs migrations.
    ///
    /// This is primarily used by tests and any ephemeral workflows that need
    /// an isolated database instance.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self { pool })
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::session::{ForgeKind, ReviewRequestState, ReviewRequestSummary};

    /// Environment flag used to run the DST regression helper in an isolated
    /// subprocess with a fixed timezone.
    const DST_TEST_SUBPROCESS_ENV: &str = "AGENTTY_DST_TEST_SUBPROCESS";

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
            .insert_session(session_id, "gpt-5.3-codex", base_branch, status, project_id)
            .await
            .expect("failed to insert session fixture");
    }

    /// Loads one session row by identifier through `load_sessions()`.
    async fn load_session_row(database: &Database, session_id: &str) -> SessionRow {
        database
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

    /// Builds one deterministic joined-session row fixture for conversion
    /// tests.
    fn session_join_row_fixture() -> SessionJoinRow {
        SessionJoinRow {
            base_branch: "main".to_string(),
            created_at: 100,
            id: "session-a".to_string(),
            input_tokens: 11,
            model: "gpt-5.3-codex".to_string(),
            output: "Saved output".to_string(),
            output_tokens: 29,
            project_id: Some(7),
            prompt: "Implement feature".to_string(),
            published_upstream_ref: Some("origin/session-a".to_string()),
            questions: Some("Question text".to_string()),
            review_request_display_id: Some("#42".to_string()),
            review_request_forge_kind: Some("GitHub".to_string()),
            review_request_last_refreshed_at: Some(456),
            review_request_source_branch: Some("feature/forge".to_string()),
            review_request_state: Some("Open".to_string()),
            review_request_status_summary: Some("2 approvals, checks passing".to_string()),
            review_request_target_branch: Some("main".to_string()),
            review_request_title: Some("Add forge review support".to_string()),
            review_request_web_url: Some(
                "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ),
            size: "M".to_string(),
            status: "Review".to_string(),
            summary: Some("Summary text".to_string()),
            title: Some("Review session".to_string()),
            updated_at: 200,
        }
    }

    /// Verifies `open()` creates missing parent directories before opening the
    /// on-disk database.
    #[tokio::test]
    async fn test_open_creates_missing_parent_directory() {
        // Arrange
        let temp_dir = tempdir().expect("temp dir should be created");
        let db_path = temp_dir.path().join("nested").join("db").join(DB_FILE);

        // Act
        let database = Database::open(&db_path)
            .await
            .expect("database should open with missing parent directories");

        // Assert
        assert!(db_path.parent().is_some_and(std::path::Path::is_dir));
        assert!(!database.pool().is_closed());
    }

    /// Verifies `load_sessions()` maps persisted joined session fields.
    #[tokio::test]
    async fn test_load_sessions_maps_joined_session_fields() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        let review_request = review_request_fixture();

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .update_session_created_at("session-a", 100)
            .await
            .expect("failed to update session created_at");
        database
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session updated_at");
        database
            .update_session_size("session-a", "L")
            .await
            .expect("failed to update session size");
        database
            .update_session_questions("session-a", "[\"Need logs?\"]")
            .await
            .expect("failed to update session questions");
        database
            .update_session_prompt("session-a", "Implement the feature")
            .await
            .expect("failed to update session prompt");
        database
            .update_session_title("session-a", "Feature work")
            .await
            .expect("failed to update session title");
        database
            .update_session_summary("session-a", "Implemented the requested feature")
            .await
            .expect("failed to update session summary");
        database
            .update_session_stats(
                "session-a",
                &SessionStats {
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to update session stats");
        database
            .update_session_model("session-a", "claude-opus-4.1")
            .await
            .expect("failed to update session model");
        database
            .update_session_published_upstream_ref("session-a", Some("origin/agentty/session-a"))
            .await
            .expect("failed to update published upstream ref");
        database
            .update_session_review_request("session-a", Some(&review_request))
            .await
            .expect("failed to update review request");
        database
            .replace_session_output("session-a", "First line")
            .await
            .expect("failed to replace session output");
        database
            .append_session_output("session-a", "\nSecond line")
            .await
            .expect("failed to append session output");
        database
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session updated_at");

        // Act
        let session_row = load_session_row(&database, "session-a").await;

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.base_branch, "main");
        assert_eq!(session_row.created_at, 100);
        assert_eq!(session_row.updated_at, 200);
        assert_eq!(session_row.model, "claude-opus-4.1");
        assert_eq!(session_row.status, "Review");
        assert_eq!(session_row.project_id, Some(project_id));
        assert_eq!(session_row.prompt, "Implement the feature");
        assert_eq!(session_row.output, "First line\nSecond line");
        assert_eq!(session_row.input_tokens, 11);
        assert_eq!(session_row.output_tokens, 29);
        assert_eq!(session_row.size, "L");
        assert_eq!(
            session_row.summary.as_deref(),
            Some("Implemented the requested feature")
        );
        assert_eq!(session_row.questions.as_deref(), Some("[\"Need logs?\"]"));
        assert_eq!(session_row.title.as_deref(), Some("Feature work"));
        assert_eq!(
            session_row.published_upstream_ref.as_deref(),
            Some("origin/agentty/session-a")
        );
        assert_review_request_row(&session_row);
    }

    /// Verifies `load_sessions_for_project()` filters rows by project id.
    #[tokio::test]
    async fn test_load_sessions_for_project_filters_to_project_rows() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project("/tmp/project-b", Some("develop"))
            .await
            .expect("failed to insert second project");

        insert_session_fixture(&database, "session-a", "main", "Review", first_project_id).await;
        insert_session_fixture(&database, "session-b", "main", "Done", first_project_id).await;
        insert_session_fixture(&database, "session-c", "develop", "Done", second_project_id).await;
        database
            .update_session_updated_at("session-a", 300)
            .await
            .expect("failed to update session-a updated_at");
        database
            .update_session_updated_at("session-b", 200)
            .await
            .expect("failed to update session-b updated_at");
        database
            .update_session_updated_at("session-c", 100)
            .await
            .expect("failed to update session-c updated_at");

        // Act
        let session_rows = database
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

    /// Verifies `load_sessions_metadata()` returns session count and max
    /// `updated_at`.
    #[tokio::test]
    async fn test_load_sessions_metadata_returns_count_and_latest_timestamp() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        insert_session_fixture(&database, "session-b", "main", "Done", project_id).await;
        database
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session-a updated_at");
        database
            .update_session_updated_at("session-b", 300)
            .await
            .expect("failed to update session-b updated_at");

        // Act
        let session_metadata = database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .update_session_created_at("session-a", 111)
            .await
            .expect("failed to update session created_at");
        database
            .update_session_updated_at("session-a", 222)
            .await
            .expect("failed to update session updated_at");

        // Act
        let session_timestamps = database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "release", "Done", project_id).await;

        // Act
        let base_branch = database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to insert usage row");

        // Act
        database
            .delete_session("session-a")
            .await
            .expect("failed to delete session");
        let deleted_session = database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-queued", "session-a", "merge")
            .await
            .expect("failed to insert queued operation");
        database
            .insert_session_operation("operation-running", "session-a", "sync")
            .await
            .expect("failed to insert running operation");
        database
            .insert_session_operation("operation-done", "session-a", "review")
            .await
            .expect("failed to insert done operation");
        database
            .mark_session_operation_running("operation-running")
            .await
            .expect("failed to mark running operation");
        database
            .mark_session_operation_running("operation-done")
            .await
            .expect("failed to mark done operation running");
        database
            .mark_session_operation_done("operation-done")
            .await
            .expect("failed to mark done operation");

        // Act
        let unfinished_rows = database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-queued", "session-a", "merge")
            .await
            .expect("failed to insert queued operation");
        database
            .insert_session_operation("operation-done", "session-a", "review")
            .await
            .expect("failed to insert done operation");
        database
            .mark_session_operation_running("operation-done")
            .await
            .expect("failed to mark done operation running");
        database
            .mark_session_operation_done("operation-done")
            .await
            .expect("failed to mark done operation");

        // Act
        database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");
        database
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");
        database
            .mark_session_operation_done("operation-a")
            .await
            .expect("failed to mark operation done");

        // Act
        let is_unfinished = database
            .is_session_operation_unfinished("operation-a")
            .await
            .expect("failed to check unfinished operation state");

        // Assert
        assert!(!is_unfinished);
    }

    /// Verifies `is_cancel_requested_for_session_operations()` reflects the
    /// current cancel-request state for unfinished rows.
    #[tokio::test]
    async fn test_is_cancel_requested_for_session_operations_tracks_unfinished_rows() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");

        database
            .request_cancel_for_session_operations("session-a")
            .await
            .expect("failed to request cancel");

        // Act
        let cancel_requested = database
            .is_cancel_requested_for_session_operations("session-a")
            .await
            .expect("failed to check cancel request state");

        // Assert
        assert!(cancel_requested);
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");

        // Act
        database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");
        database
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");

        // Act
        database
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

    /// Verifies `SessionJoinRow::into_session_row()` drops partially
    /// populated review-request columns instead of surfacing an invalid row
    /// model.
    #[test]
    fn test_session_join_row_ignores_partial_review_request_columns() {
        // Arrange
        let mut session_join_row = session_join_row_fixture();
        session_join_row.review_request_last_refreshed_at = None;

        // Act
        let session_row = session_join_row.into_session_row();

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.project_id, Some(7));
        assert_eq!(session_row.status, "Review");
        assert_eq!(session_row.review_request, None);
    }

    /// Verifies `SessionJoinRow::into_session_row()` maps a fully populated
    /// review-request into the public session row model.
    #[test]
    fn test_session_join_row_maps_review_request_columns() {
        // Arrange
        let session_join_row = session_join_row_fixture();

        // Act
        let session_row = session_join_row.into_session_row();

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.project_id, Some(7));
        assert_eq!(
            session_row.published_upstream_ref.as_deref(),
            Some("origin/session-a")
        );
        assert_eq!(session_row.questions.as_deref(), Some("Question text"));
        assert_eq!(session_row.summary.as_deref(), Some("Summary text"));
        assert_eq!(session_row.title.as_deref(), Some("Review session"));
        assert_review_request_row(&session_row);
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to insert first usage row");
        database
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    input_tokens: 3,
                    output_tokens: 5,
                },
            )
            .await
            .expect("failed to update existing usage row");
        database
            .upsert_session_usage("session-a", "ignored-model", &SessionStats::default())
            .await
            .expect("failed to ignore zero-usage update");

        // Act
        let usage_rows = database
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
            .upsert_setting("DefaultSmartModel", AgentModel::Gemini31ProPreview.as_str())
            .await
            .expect("failed to persist default smart model");
        database
            .upsert_setting("DefaultFastModel", AgentModel::Gpt53Codex.as_str())
            .await
            .expect("failed to persist default fast model");
        database
            .upsert_setting("DefaultReviewModel", AgentModel::ClaudeOpus46.as_str())
            .await
            .expect("failed to persist default review model");

        // Act
        let default_smart_model = database
            .get_setting("DefaultSmartModel")
            .await
            .expect("failed to load default smart model");
        let default_fast_model = database
            .get_setting("DefaultFastModel")
            .await
            .expect("failed to load default fast model");
        let default_review_model = database
            .get_setting("DefaultReviewModel")
            .await
            .expect("failed to load default review model");

        // Assert
        assert_eq!(
            default_smart_model,
            Some(AgentModel::Gemini31ProPreview.as_str().to_string())
        );
        assert_eq!(
            default_fast_model,
            Some(AgentModel::Gpt53Codex.as_str().to_string())
        );
        assert_eq!(
            default_review_model,
            Some(AgentModel::ClaudeOpus46.as_str().to_string())
        );
    }

    #[tokio::test]
    async fn test_project_setting_round_trip_is_isolated_per_project() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project("/tmp/project-b", Some("main"))
            .await
            .expect("failed to insert second project");

        database
            .upsert_project_setting(
                first_project_id,
                SettingName::OpenCommand.as_str(),
                "npm run dev",
            )
            .await
            .expect("failed to persist first project setting");
        database
            .upsert_project_setting(
                second_project_id,
                SettingName::OpenCommand.as_str(),
                "cargo test",
            )
            .await
            .expect("failed to persist second project setting");

        // Act
        let first_project_setting = database
            .get_project_setting(first_project_id, SettingName::OpenCommand.as_str())
            .await
            .expect("failed to load first project setting");
        let second_project_setting = database
            .get_project_setting(second_project_id, SettingName::OpenCommand.as_str())
            .await
            .expect("failed to load second project setting");

        // Assert
        assert_eq!(first_project_setting, Some("npm run dev".to_string()));
        assert_eq!(second_project_setting, Some("cargo test".to_string()));
    }

    #[tokio::test]
    async fn test_project_reasoning_level_round_trip_uses_typed_setting_helpers() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        // Act
        database
            .set_project_reasoning_level(project_id, ReasoningLevel::Low)
            .await
            .expect("failed to persist project reasoning level");
        let reasoning_level = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load project reasoning level");

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::Low);
    }

    #[tokio::test]
    async fn test_load_project_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        // Act
        let missing_setting_level = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load default project reasoning level");
        database
            .upsert_project_setting(
                project_id,
                SettingName::ReasoningLevel.as_str(),
                "unsupported",
            )
            .await
            .expect("failed to insert unsupported project reasoning level");
        let invalid_setting_level = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load fallback project reasoning level");

        // Assert
        assert_eq!(missing_setting_level, ReasoningLevel::High);
        assert_eq!(invalid_setting_level, ReasoningLevel::High);
    }

    #[tokio::test]
    async fn test_reasoning_level_round_trip_uses_typed_setting_helpers() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        database
            .set_reasoning_level(ReasoningLevel::Low)
            .await
            .expect("failed to persist reasoning level");
        let reasoning_level = database
            .load_reasoning_level()
            .await
            .expect("failed to load reasoning level");

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::Low);
    }

    #[tokio::test]
    async fn test_load_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let missing_setting_level = database
            .load_reasoning_level()
            .await
            .expect("failed to load default reasoning level");
        database
            .upsert_setting(SettingName::ReasoningLevel.as_str(), "unsupported")
            .await
            .expect("failed to insert unsupported reasoning level");
        let invalid_setting_level = database
            .load_reasoning_level()
            .await
            .expect("failed to load fallback reasoning level");

        // Assert
        assert_eq!(missing_setting_level, ReasoningLevel::High);
        assert_eq!(invalid_setting_level, ReasoningLevel::High);
    }

    #[tokio::test]
    async fn test_session_provider_conversation_id_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .update_session_provider_conversation_id("session-a", Some("thread-123"))
            .await
            .expect("failed to set provider conversation id");
        let stored_id = database
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load provider conversation id");
        database
            .update_session_provider_conversation_id("session-a", None)
            .await
            .expect("failed to clear provider conversation id");
        let cleared_id = database
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load cleared provider conversation id");

        // Assert
        assert_eq!(stored_id, Some("thread-123".to_string()));
        assert_eq!(cleared_id, None);
    }

    #[tokio::test]
    async fn test_session_published_upstream_ref_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .update_session_published_upstream_ref("session-a", Some("origin/agentty/session-a"))
            .await
            .expect("failed to persist session published upstream ref");
        let persisted_row = database
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing persisted session row");
        database
            .update_session_published_upstream_ref("session-a", None)
            .await
            .expect("failed to clear session published upstream ref");
        let cleared_row = database
            .load_sessions()
            .await
            .expect("failed to load sessions after clearing")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing cleared session row");

        // Assert
        assert_eq!(
            persisted_row.published_upstream_ref.as_deref(),
            Some("origin/agentty/session-a")
        );
        assert_eq!(cleared_row.published_upstream_ref, None);
    }

    #[tokio::test]
    async fn test_session_review_request_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let review_request = review_request_fixture();

        // Act
        database
            .update_session_review_request("session-a", Some(&review_request))
            .await
            .expect("failed to persist session review request");
        let persisted_row = database
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing persisted session row");
        database
            .update_session_review_request("session-a", None)
            .await
            .expect("failed to clear session review request");
        let cleared_row = database
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
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .insert_session_creation_activity_at("session-a", 123)
            .await
            .expect("failed to persist activity event");
        let activity_timestamps = database
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
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .insert_session_creation_activity_at("session-a", 100)
            .await
            .expect("failed to persist first activity event");
        database
            .insert_session_creation_activity_at("session-a", 200)
            .await
            .expect("failed to persist duplicate activity event");
        let activity_timestamps = database
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
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .insert_session_creation_activity_at("session-a", 100)
            .await
            .expect("failed to persist first activity event");
        database
            .insert_session("session-b", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .insert_session_creation_activity_at("session-b", 200)
            .await
            .expect("failed to persist second activity event");
        database
            .delete_session("session-a")
            .await
            .expect("failed to delete first session");

        // Act
        let activity_timestamps = database
            .load_session_activity_timestamps()
            .await
            .expect("failed to load activity timestamps");

        // Assert
        assert_eq!(activity_timestamps, vec![100, 200]);
    }

    /// Verifies `load_session_activity()` groups immutable activity rows by
    /// local day.
    #[tokio::test]
    async fn test_load_session_activity_groups_counts_by_local_day() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .insert_session("session-b", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .insert_session("session-c", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert third session");

        let first_day_timestamp = 10 * 86_400 + 10;
        let second_timestamp_same_day = 10 * 86_400 + 600;
        let second_day_timestamp = 11 * 86_400 + 50;

        database
            .clear_session_activity()
            .await
            .expect("failed to clear session activity");
        database
            .insert_session_creation_activity_at("session-a", first_day_timestamp)
            .await
            .expect("failed to persist first activity event");
        database
            .insert_session_creation_activity_at("session-b", second_timestamp_same_day)
            .await
            .expect("failed to persist second activity event");
        database
            .insert_session_creation_activity_at("session-c", second_day_timestamp)
            .await
            .expect("failed to persist third activity event");

        let expected_activity = vec![
            DailyActivity {
                day_key: local_day_key(first_day_timestamp),
                session_count: 2,
            },
            DailyActivity {
                day_key: local_day_key(second_day_timestamp),
                session_count: 1,
            },
        ];

        // Act
        let activity = database
            .load_session_activity()
            .await
            .expect("failed to load aggregated session activity");

        // Assert
        assert_eq!(activity, expected_activity);
    }

    #[tokio::test]
    async fn test_load_projects_with_stats_returns_session_counts_and_last_update() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert session-a");
        database
            .insert_session("session-b", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert session-b");

        // Act
        let projects = database
            .load_projects_with_stats()
            .await
            .expect("failed to load projects with stats");

        // Assert
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].session_count, 2);
        assert!(projects[0].last_session_updated_at.is_some());
    }

    /// Converts one Unix timestamp into the local day key used by heatmap
    /// activity rows.
    fn local_day_key(timestamp_seconds: i64) -> i64 {
        let utc_timestamp = time::OffsetDateTime::from_unix_timestamp(timestamp_seconds)
            .expect("timestamp should be valid for test fixture");
        let local_offset = time::UtcOffset::local_offset_at(utc_timestamp)
            .expect("local offset should resolve for test fixture");

        timestamp_seconds
            .saturating_add(i64::from(local_offset.whole_seconds()))
            .div_euclid(86_400)
    }

    /// Verifies the SQL activity aggregation matches Rust local-day grouping
    /// across a known daylight-saving transition in an isolated timezone-fixed
    /// subprocess.
    #[test]
    fn test_load_session_activity_matches_rust_grouping_across_dst_transition() {
        // Arrange
        if !cfg!(unix) {
            return;
        }

        let current_test_binary = env::current_exe().expect("failed to resolve current test bin");

        // Act
        let output = Command::new(current_test_binary)
            .env(DST_TEST_SUBPROCESS_ENV, "1")
            .env("TZ", "America/Los_Angeles")
            .arg(
                "test_load_session_activity_matches_rust_grouping_across_dst_transition_subprocess",
            )
            .arg("--exact")
            .arg("--test-threads=1")
            .output()
            .expect("failed to run DST subprocess test");

        // Assert
        assert!(
            output.status.success(),
            "DST subprocess test failed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Verifies the SQL activity aggregation keeps timestamps on both sides of
    /// the 2024 spring-forward transition in the same local day when Rust's
    /// per-event local-offset calculation says they should.
    #[tokio::test]
    async fn test_load_session_activity_matches_rust_grouping_across_dst_transition_subprocess() {
        // Arrange
        if !cfg!(unix) || env::var_os(DST_TEST_SUBPROCESS_ENV).is_none() {
            return;
        }

        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .insert_session("session-b", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .insert_session("session-c", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert third session");
        database
            .clear_session_activity()
            .await
            .expect("failed to clear activity history");

        // `2024-03-10T01:30:00-08:00`, still before the DST jump.
        let before_dst_jump = 1_710_063_000_i64;
        // `2024-03-10T03:30:00-07:00`, after the skipped hour.
        let after_dst_jump = 1_710_066_600_i64;
        // `2024-03-11T00:30:00-07:00`, the next local day.
        let next_local_day = 1_710_142_200_i64;

        database
            .insert_session_creation_activity_at("session-a", before_dst_jump)
            .await
            .expect("failed to persist pre-DST activity");
        database
            .insert_session_creation_activity_at("session-b", after_dst_jump)
            .await
            .expect("failed to persist post-DST activity");
        database
            .insert_session_creation_activity_at("session-c", next_local_day)
            .await
            .expect("failed to persist next-day activity");

        let first_day_key = local_day_key(before_dst_jump);
        let second_day_key = local_day_key(after_dst_jump);
        let third_day_key = local_day_key(next_local_day);

        // Act
        let activity = database
            .load_session_activity()
            .await
            .expect("failed to load grouped session activity");

        // Assert
        assert_eq!(first_day_key, second_day_key);
        assert_ne!(second_day_key, third_day_key);
        assert_eq!(
            activity,
            vec![
                DailyActivity {
                    day_key: first_day_key,
                    session_count: 2,
                },
                DailyActivity {
                    day_key: third_day_key,
                    session_count: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_set_and_load_active_project_id_round_trip() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");

        // Act
        database
            .set_active_project_id(project_id)
            .await
            .expect("failed to persist active project id");
        let active_project_id = database
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
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let loaded_project_id = database
            .load_session_project_id("session-a")
            .await
            .expect("failed to load session project id");

        // Assert
        assert_eq!(loaded_project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_load_session_summary_returns_persisted_summary() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-a", "gpt-5.3-codex", "main", "Done", project_id)
            .await
            .expect("failed to insert session");
        database
            .update_session_summary("session-a", "persisted summary")
            .await
            .expect("failed to update session summary");

        // Act
        let loaded_summary = database
            .load_session_summary("session-a")
            .await
            .expect("failed to load session summary");

        // Assert
        assert_eq!(loaded_summary.as_deref(), Some("persisted summary"));
    }

    #[tokio::test]
    async fn test_set_project_favorite_updates_project_state() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");

        // Act
        database
            .set_project_favorite(project_id, true)
            .await
            .expect("failed to set project favorite");
        let project = database
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
        sqlx::query("DROP TABLE session")
            .execute(database.pool())
            .await
            .expect("failed to drop table");

        // Act
        let result = database.load_sessions_metadata().await;

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
        sqlx::query("DROP TABLE session")
            .execute(database.pool())
            .await
            .expect("failed to drop table");

        // Act
        let result = database.load_sessions_metadata().await;

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

    // NOTE: `DbError::Migration` is not directly tested because
    // `Database::open` and `Database::open_in_memory` run migrations
    // atomically after connecting — there is no injection point to
    // pre-corrupt the schema before migrations execute. The `#[from]`
    // derive mapping from `sqlx::migrate::MigrateError` is validated
    // at compile time by `thiserror`.
}
