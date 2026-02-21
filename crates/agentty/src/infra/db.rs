//! Database layer for persisting session metadata using `SQLite` via `SQLx`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::domain::session::SessionStats;

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

    // Tests omitted for brevity in this response, but should be preserved in real file
    // I will include them in the actual write call
}
