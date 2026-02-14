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

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::model::SessionStats;

pub const DB_DIR: &str = "db";
pub const DB_FILE: &str = "agentty.db";

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

pub struct ProjectRow {
    pub git_branch: Option<String>,
    pub id: i64,
    pub path: String,
}

pub struct SessionRow {
    pub agent: String,
    pub base_branch: String,
    pub commit_count: i64,
    pub created_at: i64,
    pub id: String,
    pub input_tokens: Option<i64>,
    pub model: String,
    pub output: String,
    pub output_tokens: Option<i64>,
    pub project_id: Option<i64>,
    pub prompt: String,
    pub status: String,
    pub title: Option<String>,
    pub updated_at: i64,
}

impl Database {
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

    /// Sets `project_id` for sessions that do not yet reference a project.
    ///
    /// # Errors
    /// Returns an error if the backfill update fails.
    pub async fn backfill_sessions_project(&self, project_id: i64) -> Result<(), String> {
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

    /// Inserts a newly created session row.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be inserted.
    pub async fn insert_session(
        &self,
        id: &str,
        agent: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), String> {
        sqlx::query(
            r"
INSERT INTO session (id, agent, model, base_branch, status, project_id, prompt, output)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
",
        )
        .bind(id)
        .bind(agent)
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
SELECT id, agent, model, base_branch, status, title, project_id, prompt, output,
       commit_count, created_at, updated_at, input_tokens, output_tokens
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
                agent: row.get("agent"),
                base_branch: row.get("base_branch"),
                commit_count: row.get("commit_count"),
                created_at: row.get("created_at"),
                id: row.get("id"),
                input_tokens: row.get("input_tokens"),
                model: row.get("model"),
                output: row.get("output"),
                output_tokens: row.get("output_tokens"),
                project_id: row.get("project_id"),
                prompt: row.get("prompt"),
                status: row.get("status"),
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

    /// Increments the commit count for a session by one.
    ///
    /// # Errors
    /// Returns an error if the update fails.
    pub async fn increment_commit_count(&self, id: &str) -> Result<(), String> {
        sqlx::query(
            r"
UPDATE session
SET commit_count = commit_count + 1
WHERE id = ?
",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to increment commit count: {err}"))?;

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

    /// Updates token statistics for a session.
    ///
    /// # Errors
    /// Returns an error if the stats update fails.
    pub async fn update_session_stats(&self, id: &str, stats: &SessionStats) -> Result<(), String> {
        sqlx::query(
            r"
UPDATE session
SET input_tokens = ?, output_tokens = ?
WHERE id = ?
",
        )
        .bind(stats.input_tokens)
        .bind(stats.output_tokens)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to update session stats: {err}"))?;

        Ok(())
    }

    /// Updates the persisted agent/model pair for a session.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_agent_and_model(
        &self,
        id: &str,
        agent: &str,
        model: &str,
    ) -> Result<(), String> {
        sqlx::query(
            r"
UPDATE session
SET agent = ?, model = ?
WHERE id = ?
",
        )
        .bind(agent)
        .bind(model)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to update session agent/model: {err}"))?;

        Ok(())
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

    /// Clears a session's chat history, resetting it to a fresh state.
    ///
    /// Resets output, prompt, title, status, and token statistics while
    /// preserving the session identity, worktree, agent, and model.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn clear_session_history(&self, id: &str) -> Result<(), String> {
        sqlx::query(
            r"
UPDATE session
SET output = '', prompt = '', title = NULL, status = 'New',
    input_tokens = NULL, output_tokens = NULL
WHERE id = ?
",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("Failed to clear session history: {err}"))?;

        Ok(())
    }

    /// Returns the persisted base branch for a session, when present.
    ///
    /// # Errors
    /// Returns an error if the base branch lookup query fails.
    pub async fn get_base_branch(&self, id: &str) -> Result<Option<String>, String> {
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
    async fn test_backfill_sessions_project() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        // Insert session without project_id (simulates legacy data)
        sqlx::query(
            r"
INSERT INTO session (id, agent, base_branch)
VALUES (?, ?, ?)
",
        )
        .bind("orphan")
        .bind("claude")
        .bind("main")
        .execute(db.pool())
        .await
        .expect("failed to insert");

        // Act
        db.backfill_sessions_project(project_id)
            .await
            .expect("failed to backfill");

        // Assert
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "orphan");
        assert_eq!(sessions[0].project_id, Some(project_id));
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
            .insert_session(
                "sess1",
                "claude",
                "claude-opus-4-6",
                "main",
                "Done",
                project_id,
            )
            .await;

        // Assert
        assert!(result.is_ok());

        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].model, "claude-opus-4-6");
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
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let result = db
            .insert_session(
                "sess1",
                "gemini",
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
        db.insert_session(
            "beta",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");
        db.insert_session(
            "alpha",
            "gemini",
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
        assert_eq!(sessions[0].agent, "claude");
        assert_eq!(sessions[0].base_branch, "main");
        assert_eq!(sessions[0].model, "claude-opus-4-6");
        assert_eq!(sessions[0].status, "Done");
        assert_eq!(sessions[1].id, "alpha");
        assert_eq!(sessions[1].agent, "gemini");
        assert_eq!(sessions[1].base_branch, "develop");
        assert_eq!(sessions[1].model, "gemini-3-flash-preview");
        assert_eq!(sessions[1].status, "InProgress");
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
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha");
        db.insert_session(
            "beta",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
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
    async fn test_update_session_status() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert");

        let initial_sessions = db.load_sessions().await.expect("failed to load");
        let initial_updated_at = initial_sessions[0].updated_at;

        // Act
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let result = db.update_session_status("sess1", "Done").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].status, "Done");
        assert!(
            sessions[0].updated_at > initial_updated_at,
            "updated_at should be updated by trigger"
        );
    }

    #[tokio::test]
    async fn test_increment_commit_count() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        db.increment_commit_count("sess1")
            .await
            .expect("failed to increment");
        db.increment_commit_count("sess1")
            .await
            .expect("failed to increment");

        // Assert
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].commit_count, 2);
    }

    #[tokio::test]
    async fn test_update_session_title() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "New",
            project_id,
        )
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
    async fn test_update_session_agent_and_model() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "New",
            project_id,
        )
        .await
        .expect("failed to insert");

        let initial_sessions = db.load_sessions().await.expect("failed to load");
        let initial_updated_at = initial_sessions[0].updated_at;

        // Act
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let result = db
            .update_session_agent_and_model("sess1", "codex", "gpt-5.2-codex")
            .await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].agent, "codex");
        assert_eq!(sessions[0].model, "gpt-5.2-codex");
        assert!(
            sessions[0].updated_at > initial_updated_at,
            "updated_at should be updated by trigger"
        );
    }

    #[tokio::test]
    async fn test_insert_session_title_is_null_by_default() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "New",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert_eq!(sessions[0].title, None);
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
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_a,
        )
        .await
        .expect("failed to insert");
        db.insert_session(
            "sess2",
            "gemini",
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
    async fn test_delete_session() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
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
    async fn test_get_base_branch_exists() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let branch = db.get_base_branch("sess1").await.expect("failed to get");

        // Assert
        assert_eq!(branch, Some("main".to_string()));
    }

    #[tokio::test]
    async fn test_get_base_branch_not_found() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let branch = db
            .get_base_branch("nonexistent")
            .await
            .expect("failed to get");

        // Assert
        assert_eq!(branch, None);
    }

    #[tokio::test]
    async fn test_update_session_stats() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let stats = SessionStats {
            input_tokens: Some(1000),
            output_tokens: Some(500),
        };
        let result = db.update_session_stats("sess1", &stats).await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].input_tokens, Some(1000));
        assert_eq!(sessions[0].output_tokens, Some(500));
    }

    #[tokio::test]
    async fn test_load_sessions_stats_null_by_default() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert_eq!(sessions[0].input_tokens, None);
        assert_eq!(sessions[0].output_tokens, None);
    }

    #[tokio::test]
    async fn test_clear_session_history() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "claude",
            "claude-opus-4-6",
            "main",
            "Review",
            project_id,
        )
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
        let stats = SessionStats {
            input_tokens: Some(1000),
            output_tokens: Some(500),
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
        assert_eq!(sessions[0].title, None);
        assert_eq!(sessions[0].status, "New");
        assert_eq!(sessions[0].input_tokens, None);
        assert_eq!(sessions[0].output_tokens, None);
        // Preserved fields
        assert_eq!(sessions[0].agent, "claude");
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
}
