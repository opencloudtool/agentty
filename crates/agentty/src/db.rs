//! Database layer for persisting session metadata using `SQLite` via `SQLx`.
//!
//! # Database Maintenance Guide
//!
//! ## Adding a new table
//! 1. Create a new migration file in `crates/agentty/migrations/` with the next
//!    sequence number (e.g., `002_create_tasks.sql`).
//! 2. Write the `CREATE TABLE` statement in that file.
//! 3. Add corresponding CRUD methods to [`Database`].
//! 4. The migration runs automatically on next app launch via
//!    [`Database::open`].
//!
//! ## Updating an existing table
//! 1. Create a new migration file (e.g., `003_add_status_to_sessions.sql`).
//! 2. Write the SQL statements to alter the schema. For all supported `SQLite`
//!    operations, refer to <https://www.sqlite.org/lang.html>.
//! 3. Update [`SessionRow`] and query strings in [`Database`] methods.
//!
//! ## Migration versioning
//! - Migrations are embedded at compile time via `sqlx::migrate!()`.
//! - Files must be named `NNN_description.sql` with a monotonically increasing
//!   prefix (e.g., `001_`, `002_`).
//! - `SQLx` tracks applied migrations in the `_sqlx_migrations` table.
//! - On each launch, [`Database::open`] runs any unapplied migrations.
//!
//! ## Downgrading
//! - `SQLx` does not support automatic downgrades. To roll back a migration,
//!   create a new forward migration that reverses the changes (e.g.,
//!   `004_revert_status_column.sql`).
//! - For development, you can delete the database file and let migrations
//!   recreate it from scratch.
//!
//! ## Repository pattern
//!
//! All query methods live on a single [`Database`] struct rather than being
//! split into per-entity repository types. This keeps the data layer flat and
//! easy to navigate: callers depend on one type, and adding a new table means
//! adding methods to the existing impl block instead of introducing a new
//! file and wiring a new dependency. The trade-off is a growing impl, which
//! is managed by grouping methods by entity (see ordering rules below) and
//! keeping each method small — typically a single `sqlx::query` call with
//! error mapping.
//!
//! ## Method conventions
//!
//! ### Naming
//! Methods follow the pattern `{verb}_{entity}_{field}` where:
//! - **verb**: the operation (`insert`, `load`, `get`, `update`, `delete`,
//!   `append`, `increment`, `clear`, `mark`, `upsert`, `backfill`, `is`,
//!   `fail`, `request`).
//! - **entity**: singular table name (`session`, `project`,
//!   `session_operation`).
//! - **field** (optional): the column or concept being acted on (e.g.,
//!   `status`, `title`, `base_branch`).
//!
//! ### Ordering
//! Methods are grouped by entity. Within each entity group, place CRUD
//! operations first, then field updates, then specialized queries.
//! Infrastructure methods (`open`, `pool`) come before all entity groups.
//! Tests follow the same order as the methods they cover.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::model::SessionStats;

/// Subdirectory under the agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default database filename.
pub const DB_FILE: &str = "agentty.db";

/// Thin wrapper around a `SQLite` connection pool providing query methods.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

/// Row returned when loading a project from the `project` table.
pub struct ProjectRow {
    pub git_branch: Option<String>,
    pub id: i64,
    pub path: String,
}

/// Row returned when loading a session from the `session` table.
pub struct SessionRow {
    pub base_branch: String,
    pub created_at: i64,
    pub id: String,
    pub input_tokens: i64,
    pub model: String,
    pub output: String,
    pub output_tokens: i64,
    pub permission_mode: String,
    pub project_id: Option<i64>,
    pub prompt: String,
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

impl Database {
    /// Opens the `SQLite` database and runs embedded migrations.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database cannot
    /// be opened, or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create database directory: {err}"))?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|err| format!("Failed to connect to database: {err}"))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|err| format!("Failed to run migrations: {err}"))?;

        Ok(Self { pool })
    }

    /// Returns a reference to the underlying connection pool.
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
    ) -> Result<i64, String> {
        sqlx::query(
            r"
INSERT INTO project (path, git_branch)
VALUES (?, ?)
ON CONFLICT(path) DO UPDATE
SET git_branch = excluded.git_branch
",
        )
        .bind(path)
        .bind(git_branch)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to upsert project: {err}"))?;

        let row = sqlx::query(
            r"
SELECT id
FROM project
WHERE path = ?
",
        )
        .bind(path)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| format!("Failed to fetch project id: {err}"))?;

        Ok(row.get("id"))
    }

    /// Looks up a project by identifier.
    ///
    /// # Errors
    /// Returns an error if the project lookup query fails.
    pub async fn get_project(&self, id: i64) -> Result<Option<ProjectRow>, String> {
        let row = sqlx::query(
            r"
SELECT id, path, git_branch
FROM project
WHERE id = ?
",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| format!("Failed to get project: {err}"))?;

        Ok(row.map(|row| ProjectRow {
            git_branch: row.get("git_branch"),
            id: row.get("id"),
            path: row.get("path"),
        }))
    }

    /// Loads all configured projects ordered by path.
    ///
    /// # Errors
    /// Returns an error if project rows cannot be read from the database.
    pub async fn load_projects(&self) -> Result<Vec<ProjectRow>, String> {
        let rows = sqlx::query(
            r"
SELECT id, path, git_branch
FROM project
ORDER BY path
",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| format!("Failed to load projects: {err}"))?;

        Ok(rows
            .iter()
            .map(|row| ProjectRow {
                git_branch: row.get("git_branch"),
                id: row.get("id"),
                path: row.get("path"),
            })
            .collect())
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
    ) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to insert session: {err}"))?;

        Ok(())
    }

    /// Loads all sessions ordered by most recent update.
    ///
    /// # Errors
    /// Returns an error if session rows cannot be read from the database.
    pub async fn load_sessions(&self) -> Result<Vec<SessionRow>, String> {
        let rows = sqlx::query(
            r"
SELECT id, model, base_branch, status, title, project_id, prompt, output,
       created_at, updated_at, input_tokens, output_tokens, permission_mode, size, summary
FROM session
ORDER BY updated_at DESC, id
",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| format!("Failed to load sessions: {err}"))?;

        Ok(rows
            .iter()
            .map(|row| SessionRow {
                base_branch: row.get("base_branch"),
                created_at: row.get("created_at"),
                id: row.get("id"),
                input_tokens: row.get("input_tokens"),
                model: row.get("model"),
                output: row.get("output"),
                output_tokens: row.get("output_tokens"),
                permission_mode: row.get("permission_mode"),
                project_id: row.get("project_id"),
                prompt: row.get("prompt"),
                size: row.get("size"),
                status: row.get("status"),
                summary: row.get("summary"),
                title: row.get("title"),
                updated_at: row.get("updated_at"),
            })
            .collect())
    }

    /// Loads lightweight session metadata used for cheap change detection.
    ///
    /// Returns `(session_count, max_updated_at)` from the `session` table so
    /// callers can decide whether a full `load_sessions()` refresh is needed.
    ///
    /// # Errors
    /// Returns an error if metadata cannot be queried from the database.
    pub async fn load_sessions_metadata(&self) -> Result<(i64, i64), String> {
        let row = sqlx::query(
            r"
SELECT COUNT(*) AS session_count, COALESCE(MAX(updated_at), 0) AS max_updated_at
FROM session
",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|err| format!("Failed to load session metadata: {err}"))?;

        let session_count = row.get("session_count");
        let max_updated_at = row.get("max_updated_at");

        Ok((session_count, max_updated_at))
    }

    /// Deletes a session row by identifier.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be deleted.
    pub async fn delete_session(&self, id: &str) -> Result<(), String> {
        sqlx::query(
            r"
DELETE FROM session
WHERE id = ?
",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to delete session: {err}"))?;
        Ok(())
    }

    /// Updates the status for a session row.
    ///
    /// # Errors
    /// Returns an error if the status update fails.
    pub async fn update_session_status(&self, id: &str, status: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to update session status: {err}"))?;
        Ok(())
    }

    /// Updates the size bucket for a session row.
    ///
    /// The update is skipped when the stored value already matches `size`.
    ///
    /// # Errors
    /// Returns an error if the size update fails.
    pub async fn update_session_size(&self, id: &str, size: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to update session size: {err}"))?;

        Ok(())
    }

    /// Updates the saved prompt for a session row.
    ///
    /// # Errors
    /// Returns an error if the prompt update fails.
    pub async fn update_session_prompt(&self, id: &str, prompt: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to update session prompt: {err}"))?;

        Ok(())
    }

    /// Updates the display title for a session row.
    ///
    /// # Errors
    /// Returns an error if the title update fails.
    pub async fn update_session_title(&self, id: &str, title: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to update session title: {err}"))?;

        Ok(())
    }

    /// Updates the terminal summary for a session row.
    ///
    /// # Errors
    /// Returns an error if the summary update fails.
    pub async fn update_session_summary(&self, id: &str, summary: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to update session summary: {err}"))?;

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
    pub async fn update_session_stats(&self, id: &str, stats: &SessionStats) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to update session stats: {err}"))?;

        Ok(())
    }

    /// Updates the persisted model for a session.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_model(&self, id: &str, model: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to update session model: {err}"))?;

        Ok(())
    }

    /// Updates the persisted permission mode for a session.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_permission_mode(
        &self,
        id: &str,
        permission_mode: &str,
    ) -> Result<(), String> {
        sqlx::query(
            r"
UPDATE session
SET permission_mode = ?
WHERE id = ?
",
        )
        .bind(permission_mode)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to update session permission mode: {err}"))?;

        Ok(())
    }

    /// Appends text to the saved output for a session row.
    ///
    /// # Errors
    /// Returns an error if the output append update fails.
    pub async fn append_session_output(&self, id: &str, chunk: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to append session output: {err}"))?;

        Ok(())
    }

    /// Clears a session's chat history, resetting it to a fresh state.
    ///
    /// Resets output, prompt, title, and status while preserving the session
    /// identity, worktree, agent, model, and accumulated token statistics.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn clear_session_history(&self, id: &str) -> Result<(), String> {
        sqlx::query(
            r"
UPDATE session
SET output = '', prompt = '', title = NULL, summary = NULL, status = 'New'
WHERE id = ?
",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to clear session history: {err}"))?;

        Ok(())
    }

    /// Sets `project_id` for sessions that do not yet reference a project.
    ///
    /// # Errors
    /// Returns an error if the backfill update fails.
    pub async fn backfill_session_project(&self, project_id: i64) -> Result<(), String> {
        sqlx::query(
            r"
UPDATE session
SET project_id = ?
WHERE project_id IS NULL
",
        )
        .bind(project_id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to backfill sessions: {err}"))?;
        Ok(())
    }

    /// Returns the persisted base branch for a session, when present.
    ///
    /// # Errors
    /// Returns an error if the base branch lookup query fails.
    pub async fn get_session_base_branch(&self, id: &str) -> Result<Option<String>, String> {
        let row = sqlx::query(
            r"
SELECT base_branch
FROM session
WHERE id = ?
",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| format!("Failed to get base branch: {err}"))?;

        Ok(row.map(|row| row.get("base_branch")))
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
    ) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to insert session operation: {err}"))?;

        Ok(())
    }

    /// Loads operations still waiting in queue or currently running.
    ///
    /// # Errors
    /// Returns an error if operation rows cannot be read.
    pub async fn load_unfinished_session_operations(
        &self,
    ) -> Result<Vec<SessionOperationRow>, String> {
        let rows = sqlx::query(
            r"
SELECT id, session_id, kind, status, queued_at, started_at, finished_at,
       heartbeat_at, last_error, cancel_requested
FROM session_operation
WHERE status IN ('queued', 'running')
ORDER BY queued_at ASC, id ASC
",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| format!("Failed to load unfinished session operations: {err}"))?;

        Ok(rows
            .iter()
            .map(|row| SessionOperationRow {
                cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
                finished_at: row.get("finished_at"),
                heartbeat_at: row.get("heartbeat_at"),
                id: row.get("id"),
                kind: row.get("kind"),
                last_error: row.get("last_error"),
                queued_at: row.get("queued_at"),
                session_id: row.get("session_id"),
                started_at: row.get("started_at"),
                status: row.get("status"),
            })
            .collect())
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
    ) -> Result<bool, String> {
        let row = sqlx::query(
            r"
SELECT EXISTS(
    SELECT 1
    FROM session_operation
    WHERE id = ?
      AND status IN ('queued', 'running')
) AS is_unfinished
",
        )
        .bind(operation_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| format!("Failed to check unfinished operation state: {err}"))?;

        Ok(row.get::<i64, _>("is_unfinished") != 0)
    }

    /// Marks an operation as running and refreshes its heartbeat timestamp.
    ///
    /// # Errors
    /// Returns an error if the operation row cannot be updated.
    pub async fn mark_session_operation_running(&self, operation_id: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to mark session operation as running: {err}"))?;

        Ok(())
    }

    /// Marks an operation as completed successfully.
    ///
    /// # Errors
    /// Returns an error if the operation row cannot be updated.
    pub async fn mark_session_operation_done(&self, operation_id: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to mark session operation as done: {err}"))?;

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
    ) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to mark session operation as failed: {err}"))?;

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
    ) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to mark session operation as canceled: {err}"))?;

        Ok(())
    }

    /// Requests cancellation for unfinished operations of a session.
    ///
    /// # Errors
    /// Returns an error if the operation rows cannot be updated.
    pub async fn request_cancel_for_session_operations(
        &self,
        session_id: &str,
    ) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to request cancel for session operations: {err}"))?;

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
    ) -> Result<bool, String> {
        let row = sqlx::query(
            r"
SELECT EXISTS(
    SELECT 1
    FROM session_operation
    WHERE session_id = ?
      AND cancel_requested = 1
      AND status IN ('queued', 'running')
) AS is_requested
",
        )
        .bind(session_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| format!("Failed to check cancel request for session operations: {err}"))?;

        Ok(row.get::<i64, _>("is_requested") != 0)
    }

    /// Marks unfinished operations as failed after process restart.
    ///
    /// # Errors
    /// Returns an error if operation rows cannot be updated.
    pub async fn fail_unfinished_session_operations(&self, reason: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to fail unfinished session operations: {err}"))?;

        Ok(())
    }

    /// Inserts or updates a setting by name.
    ///
    /// # Errors
    /// Returns an error if the setting row cannot be written.
    pub async fn upsert_setting(&self, name: &str, value: &str) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to upsert setting: {err}"))?;

        Ok(())
    }

    /// Looks up a setting value by name.
    ///
    /// # Errors
    /// Returns an error if the setting lookup query fails.
    pub async fn get_setting(&self, name: &str) -> Result<Option<String>, String> {
        let row = sqlx::query(
            r"
SELECT value
FROM setting
WHERE name = ?
",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| format!("Failed to get setting: {err}"))?;

        Ok(row.map(|row| row.get("value")))
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
    ) -> Result<(), String> {
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
        .await
        .map_err(|err| format!("Failed to upsert session usage: {err}"))?;

        Ok(())
    }

    /// Loads per-model token usage rows for a session, ordered by model name.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn load_session_usage(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionUsageRow>, String> {
        let rows = sqlx::query(
            r"
SELECT session_id, model, created_at, input_tokens, invocation_count, output_tokens
FROM session_usage
WHERE session_id = ?
ORDER BY model
",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| format!("Failed to load session usage: {err}"))?;

        Ok(rows
            .iter()
            .map(|row| SessionUsageRow {
                created_at: row.get("created_at"),
                input_tokens: row.get("input_tokens"),
                invocation_count: row.get("invocation_count"),
                model: row.get("model"),
                output_tokens: row.get("output_tokens"),
                session_id: row.get("session_id"),
            })
            .collect())
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
    ) -> Result<Option<(i64, i64)>, String> {
        let row = sqlx::query_as::<_, (i64, i64)>(
            r"
SELECT created_at, updated_at
FROM session
WHERE id = ?
",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| format!("Failed to load session timestamps: {err}"))?;

        Ok(row)
    }
}

fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

#[cfg(test)]
impl Database {
    /// Opens an in-memory `SQLite` database for tests and runs migrations.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    pub async fn open_in_memory() -> Result<Self, String> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|err| format!("Failed to connect to in-memory database: {err}"))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|err| format!("Failed to run migrations: {err}"))?;

        Ok(Self { pool })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_in_memory() {
        // Arrange & Act
        let db = Database::open_in_memory().await;

        // Assert
        assert!(db.is_ok());
    }

    #[tokio::test]
    async fn test_open_creates_directory_and_file() {
        // Arrange
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let db_path = dir.path().join("subdir").join("test.db");

        // Act
        let db = Database::open(&db_path).await;

        // Assert
        assert!(db.is_ok());
        assert!(db_path.exists());
    }

    #[tokio::test]
    async fn test_upsert_project() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");

        // Assert
        assert!(id > 0);
        let project = db
            .get_project(id)
            .await
            .expect("failed to get")
            .expect("expected project to exist");
        assert_eq!(project.path, "/tmp/project");
        assert_eq!(project.git_branch, Some("main".to_string()));
    }

    #[tokio::test]
    async fn test_upsert_project_idempotent() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let id_first = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");

        // Act
        let id_second = db
            .upsert_project("/tmp/project", Some("develop"))
            .await
            .expect("failed to upsert");

        // Assert
        assert_eq!(id_first, id_second);
        let project = db
            .get_project(id_second)
            .await
            .expect("failed to get")
            .expect("expected project to exist");
        assert_eq!(project.git_branch, Some("develop".to_string()));
    }

    #[tokio::test]
    async fn test_get_project() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");

        // Act
        let project = db.get_project(id).await.expect("failed to get");

        // Assert
        assert!(project.is_some());
        let project = project.expect("expected project to exist");
        assert_eq!(project.id, id);
        assert_eq!(project.path, "/tmp/project");
    }

    #[tokio::test]
    async fn test_get_project_not_found() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let project = db.get_project(999).await.expect("failed to get");

        // Assert
        assert!(project.is_none());
    }

    #[tokio::test]
    async fn test_load_projects() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        db.upsert_project("/tmp/beta", Some("main"))
            .await
            .expect("failed to upsert");
        db.upsert_project("/tmp/alpha", None)
            .await
            .expect("failed to upsert");

        // Act
        let projects = db.load_projects().await.expect("failed to load");

        // Assert
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].path, "/tmp/alpha");
        assert_eq!(projects[0].git_branch, None);
        assert_eq!(projects[1].path, "/tmp/beta");
        assert_eq!(projects[1].git_branch, Some("main".to_string()));
    }

    #[tokio::test]
    async fn test_load_projects_empty() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let projects = db.load_projects().await.expect("failed to load");

        // Assert
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn test_insert_session() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");

        // Act
        let result = db
            .insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await;

        // Assert
        assert!(result.is_ok());

        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].model, "claude-opus-4-6");
        assert_eq!(sessions[0].permission_mode, "auto_edit");
        assert_eq!(sessions[0].size, "XS");
        assert!(sessions[0].created_at > 0);
        assert!(sessions[0].updated_at > 0);
    }

    #[tokio::test]
    async fn test_insert_duplicate_session_fails() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let result = db
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "develop",
                "Done",
                project_id,
            )
            .await;

        // Assert
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_insert_session_title_is_null_by_default() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "New", project_id)
            .await
            .expect("failed to insert");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert_eq!(sessions[0].title, None);
        assert_eq!(sessions[0].summary, None);
    }

    #[tokio::test]
    async fn test_load_sessions_empty() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn test_load_sessions_ordered_by_updated_at_desc() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("beta", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");
        db.insert_session(
            "alpha",
            "gemini-3-flash-preview",
            "develop",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 1
WHERE id = 'alpha'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 2
WHERE id = 'beta'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta timestamp");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "beta");
        assert_eq!(sessions[0].base_branch, "main");
        assert_eq!(sessions[0].model, "claude-opus-4-6");
        assert_eq!(sessions[0].status, "Done");
        assert_eq!(sessions[1].id, "alpha");
        assert_eq!(sessions[1].base_branch, "develop");
        assert_eq!(sessions[1].model, "gemini-3-flash-preview");
        assert_eq!(sessions[1].status, "InProgress");
    }

    #[tokio::test]
    async fn test_load_sessions_include_project_ids() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_a = db
            .upsert_project("/tmp/alpha", Some("main"))
            .await
            .expect("failed to upsert");
        let project_b = db
            .upsert_project("/tmp/beta", Some("develop"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_a)
            .await
            .expect("failed to insert");
        db.insert_session(
            "sess2",
            "gemini-3-flash-preview",
            "develop",
            "Done",
            project_b,
        )
        .await
        .expect("failed to insert");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert_eq!(sessions.len(), 2);
        assert!(
            sessions
                .iter()
                .any(|session| session.id == "sess1" && session.project_id == Some(project_a))
        );
        assert!(
            sessions
                .iter()
                .any(|session| session.id == "sess1" && session.model == "claude-opus-4-6")
        );
        assert!(
            sessions
                .iter()
                .any(|session| session.id == "sess2" && session.project_id == Some(project_b))
        );
        assert!(
            sessions
                .iter()
                .any(|session| session.id == "sess2" && session.model == "gemini-3-flash-preview")
        );
    }

    #[tokio::test]
    async fn test_load_sessions_metadata_empty() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let metadata = db
            .load_sessions_metadata()
            .await
            .expect("failed to load metadata");

        // Assert
        assert_eq!(metadata, (0, 0));
    }

    #[tokio::test]
    async fn test_load_sessions_metadata_with_rows() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "alpha",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha");
        db.insert_session("beta", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert beta");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 100
WHERE id = 'alpha'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 200
WHERE id = 'beta'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta timestamp");

        // Act
        let metadata = db
            .load_sessions_metadata()
            .await
            .expect("failed to load metadata");

        // Assert
        assert_eq!(metadata, (2, 200));
    }

    #[tokio::test]
    async fn test_delete_session() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let result = db.delete_session("sess1").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_session_succeeds() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let result = db.delete_session("nonexistent").await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_update_session_status() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert");

        let initial_sessions = db.load_sessions().await.expect("failed to load");
        let initial_updated_at = initial_sessions[0].updated_at;

        // Act
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let result = db.update_session_status("sess1", "Merging").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].status, "Merging");
        assert!(
            sessions[0].updated_at > initial_updated_at,
            "updated_at should be updated by trigger"
        );
    }

    #[tokio::test]
    async fn test_update_session_size() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert");
        let initial_sessions = db.load_sessions().await.expect("failed to load");
        let initial_updated_at = initial_sessions[0].updated_at;

        // Act
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let result = db.update_session_size("sess1", "L").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].size, "L");
        assert!(
            sessions[0].updated_at > initial_updated_at,
            "updated_at should be updated by trigger"
        );
    }

    #[tokio::test]
    async fn test_update_session_title() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "New", project_id)
            .await
            .expect("failed to insert");

        // Act
        let result = db.update_session_title("sess1", "Fix the login bug").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].title, Some("Fix the login bug".to_string()));
    }

    #[tokio::test]
    async fn test_update_session_summary() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");
        let summary = "Session completed with database and UI updates.";

        // Act
        let result = db.update_session_summary("sess1", summary).await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].summary, Some(summary.to_string()));
    }

    #[tokio::test]
    async fn test_update_session_stats() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let stats = SessionStats {
            input_tokens: 1000,
            output_tokens: 500,
        };
        let result = db.update_session_stats("sess1", &stats).await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].input_tokens, 1000);
        assert_eq!(sessions[0].output_tokens, 500);
    }

    #[tokio::test]
    async fn test_update_session_stats_sums_across_calls() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let first = SessionStats {
            input_tokens: 1000,
            output_tokens: 500,
        };
        db.update_session_stats("sess1", &first)
            .await
            .expect("first update");

        let second = SessionStats {
            input_tokens: 2000,
            output_tokens: 700,
        };
        db.update_session_stats("sess1", &second)
            .await
            .expect("second update");

        // Assert
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].input_tokens, 3000);
        assert_eq!(sessions[0].output_tokens, 1200);
    }

    #[tokio::test]
    async fn test_update_session_stats_noop_when_both_zero() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let stats = SessionStats {
            input_tokens: 0,
            output_tokens: 0,
        };
        let result = db.update_session_stats("sess1", &stats).await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].input_tokens, 0);
        assert_eq!(sessions[0].output_tokens, 0);
    }

    #[tokio::test]
    async fn test_load_sessions_stats_zero_by_default() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert_eq!(sessions[0].input_tokens, 0);
        assert_eq!(sessions[0].output_tokens, 0);
    }

    #[tokio::test]
    async fn test_update_session_model() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "New", project_id)
            .await
            .expect("failed to insert");
        db.upsert_setting("DefaultModel", "claude-opus-4-6")
            .await
            .expect("failed to upsert setting");

        let initial_sessions = db.load_sessions().await.expect("failed to load");
        let initial_updated_at = initial_sessions[0].updated_at;

        // Act
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let result = db.update_session_model("sess1", "gpt-5.2-codex").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].model, "gpt-5.2-codex");
        let default_model_setting = db
            .get_setting("DefaultModel")
            .await
            .expect("failed to load setting");
        assert_eq!(default_model_setting, Some("claude-opus-4-6".to_string()));
        assert!(
            sessions[0].updated_at > initial_updated_at,
            "updated_at should be updated by trigger"
        );
    }

    #[tokio::test]
    async fn test_clear_session_history() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Review", project_id)
            .await
            .expect("failed to insert");
        db.update_session_prompt("sess1", "Fix the bug")
            .await
            .expect("failed to update prompt");
        db.append_session_output("sess1", "Agent output here")
            .await
            .expect("failed to append output");
        db.update_session_title("sess1", "Fix the bug")
            .await
            .expect("failed to update title");
        db.update_session_summary("sess1", "Summary to reset")
            .await
            .expect("failed to update summary");
        let stats = SessionStats {
            input_tokens: 1000,
            output_tokens: 500,
        };
        db.update_session_stats("sess1", &stats)
            .await
            .expect("failed to update stats");

        // Act
        let result = db.clear_session_history("sess1").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].output, "");
        assert_eq!(sessions[0].prompt, "");
        assert_eq!(sessions[0].summary, None);
        assert_eq!(sessions[0].title, None);
        assert_eq!(sessions[0].status, "New");
        assert_eq!(sessions[0].input_tokens, 1000);
        assert_eq!(sessions[0].output_tokens, 500);
        // Preserved fields
        assert_eq!(sessions[0].model, "claude-opus-4-6");
        assert_eq!(sessions[0].base_branch, "main");
    }

    #[tokio::test]
    async fn test_clear_session_history_nonexistent_id() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let result = db.clear_session_history("nonexistent").await;

        // Assert — no-op, no error
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_backfill_session_project() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        // Insert session without project_id (simulates legacy data)
        sqlx::query(
            r"
INSERT INTO session (id, base_branch)
VALUES (?, ?)
",
        )
        .bind("orphan")
        .bind("main")
        .execute(db.pool())
        .await
        .expect("failed to insert");

        // Act
        db.backfill_session_project(project_id)
            .await
            .expect("failed to backfill");

        // Assert
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "orphan");
        assert_eq!(sessions[0].project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_get_session_base_branch_exists() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let branch = db
            .get_session_base_branch("sess1")
            .await
            .expect("failed to get");

        // Assert
        assert_eq!(branch, Some("main".to_string()));
    }

    #[tokio::test]
    async fn test_get_session_base_branch_not_found() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let branch = db
            .get_session_base_branch("nonexistent")
            .await
            .expect("failed to get");

        // Assert
        assert_eq!(branch, None);
    }

    #[tokio::test]
    async fn test_insert_and_load_unfinished_session_operations() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "gemini-3-flash-preview", "main", "New", project_id)
            .await
            .expect("failed to insert session");
        db.insert_session_operation("op-1", "sess1", "start_prompt")
            .await
            .expect("failed to insert operation");

        // Act
        let operations = db
            .load_unfinished_session_operations()
            .await
            .expect("failed to load operations");

        // Assert
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].id, "op-1");
        assert_eq!(operations[0].session_id, "sess1");
        assert_eq!(operations[0].kind, "start_prompt");
        assert_eq!(operations[0].status, "queued");
        assert!(!operations[0].cancel_requested);
    }

    #[tokio::test]
    async fn test_is_session_operation_unfinished() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.insert_session_operation("op-4", "sess1", "reply")
            .await
            .expect("failed to insert operation");

        // Act
        let is_unfinished_before = db
            .is_session_operation_unfinished("op-4")
            .await
            .expect("failed to read unfinished flag");
        db.mark_session_operation_done("op-4")
            .await
            .expect("failed to mark operation done");
        let is_unfinished_after = db
            .is_session_operation_unfinished("op-4")
            .await
            .expect("failed to read unfinished flag after done");

        // Assert
        assert!(is_unfinished_before);
        assert!(!is_unfinished_after);
    }

    #[tokio::test]
    async fn test_mark_session_operation_running_and_done() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("sess1", "gemini-3-flash-preview", "main", "New", project_id)
            .await
            .expect("failed to insert session");
        db.insert_session_operation("op-2", "sess1", "reply")
            .await
            .expect("failed to insert operation");

        // Act
        db.mark_session_operation_running("op-2")
            .await
            .expect("failed to mark running");
        let running_operations = db
            .load_unfinished_session_operations()
            .await
            .expect("failed to load running operations");
        db.mark_session_operation_done("op-2")
            .await
            .expect("failed to mark done");
        let operations = db
            .load_unfinished_session_operations()
            .await
            .expect("failed to load operations");

        // Assert
        assert_eq!(running_operations.len(), 1);
        assert_eq!(running_operations[0].status, "running");
        assert!(operations.is_empty());
    }

    #[tokio::test]
    async fn test_request_cancel_for_session_operations() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.insert_session_operation("op-3", "sess1", "start_prompt")
            .await
            .expect("failed to insert operation");

        // Act
        db.request_cancel_for_session_operations("sess1")
            .await
            .expect("failed to request cancel");
        let is_requested = db
            .is_cancel_requested_for_session_operations("sess1")
            .await
            .expect("failed to check cancel request");
        db.mark_session_operation_canceled("op-3", "user requested")
            .await
            .expect("failed to mark canceled");
        let pending_after_cancel = db
            .is_cancel_requested_for_session_operations("sess1")
            .await
            .expect("failed to check cancel request after cancel");

        // Assert
        assert!(is_requested);
        assert!(!pending_after_cancel);
    }

    // ── session_usage ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_upsert_session_usage_inserts_and_loads() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("s1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");
        let stats = SessionStats {
            input_tokens: 100,
            output_tokens: 50,
        };

        // Act
        db.upsert_session_usage("s1", "claude-opus-4-6", &stats)
            .await
            .expect("failed to upsert");
        let rows = db
            .load_session_usage("s1")
            .await
            .expect("failed to load usage");

        // Assert
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-opus-4-6");
        assert_eq!(rows[0].input_tokens, 100);
        assert_eq!(rows[0].output_tokens, 50);
        assert_eq!(rows[0].invocation_count, 1);
        assert_eq!(rows[0].session_id.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn test_upsert_session_usage_accumulates_across_calls() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("s1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");
        let stats = SessionStats {
            input_tokens: 100,
            output_tokens: 50,
        };

        // Act
        db.upsert_session_usage("s1", "claude-opus-4-6", &stats)
            .await
            .expect("failed to upsert first");
        db.upsert_session_usage("s1", "claude-opus-4-6", &stats)
            .await
            .expect("failed to upsert second");
        let rows = db
            .load_session_usage("s1")
            .await
            .expect("failed to load usage");

        // Assert
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 200);
        assert_eq!(rows[0].output_tokens, 100);
        assert_eq!(rows[0].invocation_count, 2);
    }

    #[tokio::test]
    async fn test_upsert_session_usage_noop_when_both_zero() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("s1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");
        let stats = SessionStats {
            input_tokens: 0,
            output_tokens: 0,
        };

        // Act
        db.upsert_session_usage("s1", "claude-opus-4-6", &stats)
            .await
            .expect("failed to upsert");
        let rows = db
            .load_session_usage("s1")
            .await
            .expect("failed to load usage");

        // Assert
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_upsert_session_usage_multiple_models() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("s1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");

        // Act
        db.upsert_session_usage(
            "s1",
            "claude-opus-4-6",
            &SessionStats {
                input_tokens: 100,
                output_tokens: 50,
            },
        )
        .await
        .expect("failed to upsert claude");
        db.upsert_session_usage(
            "s1",
            "gemini-3-flash-preview",
            &SessionStats {
                input_tokens: 200,
                output_tokens: 80,
            },
        )
        .await
        .expect("failed to upsert gemini");
        let rows = db
            .load_session_usage("s1")
            .await
            .expect("failed to load usage");

        // Assert
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].model, "claude-opus-4-6");
        assert_eq!(rows[1].model, "gemini-3-flash-preview");
    }

    #[tokio::test]
    async fn test_load_session_usage_empty() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("s1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let rows = db
            .load_session_usage("s1")
            .await
            .expect("failed to load usage");

        // Assert
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_upsert_session_usage_survives_session_deletion() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("s1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");
        db.upsert_session_usage(
            "s1",
            "claude-opus-4-6",
            &SessionStats {
                input_tokens: 500,
                output_tokens: 200,
            },
        )
        .await
        .expect("failed to upsert usage");

        // Act
        db.delete_session("s1")
            .await
            .expect("failed to delete session");

        // Assert — usage row survives with NULL session_id (ON DELETE SET NULL)
        let row = sqlx::query(
            r"SELECT session_id, input_tokens, output_tokens FROM session_usage WHERE model = ?",
        )
        .bind("claude-opus-4-6")
        .fetch_optional(&db.pool)
        .await
        .expect("failed to query");
        let row = row.expect("usage row should still exist");
        let session_id: Option<String> = row.get("session_id");
        assert!(
            session_id.is_none(),
            "session_id should be NULL after delete"
        );
        assert_eq!(row.get::<i64, _>("input_tokens"), 500);
        assert_eq!(row.get::<i64, _>("output_tokens"), 200);
    }

    #[tokio::test]
    async fn test_load_session_timestamps_returns_values() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session("s1", "claude-opus-4-6", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let timestamps = db
            .load_session_timestamps("s1")
            .await
            .expect("failed to load timestamps");

        // Assert
        let (created_at, updated_at) = timestamps.expect("timestamps should exist");
        assert!(created_at > 0);
        assert!(updated_at > 0);
    }

    #[tokio::test]
    async fn test_load_session_timestamps_not_found() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let timestamps = db
            .load_session_timestamps("nonexistent")
            .await
            .expect("failed to load timestamps");

        // Assert
        assert!(timestamps.is_none());
    }
}
