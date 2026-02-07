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

pub const DB_DIR: &str = "db";
pub const DB_FILE: &str = "agentty.db";

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

pub struct SessionRow {
    pub agent: String,
    pub base_branch: String,
    pub name: String,
    pub status: String,
}

impl Database {
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

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

    pub async fn insert_session(
        &self,
        name: &str,
        agent: &str,
        base_branch: &str,
        status: &str,
    ) -> Result<(), String> {
        sqlx::query("INSERT INTO session (name, agent, base_branch, status) VALUES (?, ?, ?, ?)")
            .bind(name)
            .bind(agent)
            .bind(base_branch)
            .bind(status)
            .execute(&self.pool)
            .await
            .map_err(|err| format!("Failed to insert session: {err}"))?;
        Ok(())
    }

    pub async fn load_sessions(&self) -> Result<Vec<SessionRow>, String> {
        let rows =
            sqlx::query("SELECT name, agent, base_branch, status FROM session ORDER BY name")
                .fetch_all(&self.pool)
                .await
                .map_err(|err| format!("Failed to load sessions: {err}"))?;

        Ok(rows
            .iter()
            .map(|row| SessionRow {
                agent: row.get("agent"),
                base_branch: row.get("base_branch"),
                name: row.get("name"),
                status: row.get("status"),
            })
            .collect())
    }

    pub async fn update_session_status(&self, name: &str, status: &str) -> Result<(), String> {
        sqlx::query("UPDATE session SET status = ? WHERE name = ?")
            .bind(status)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|err| format!("Failed to update session status: {err}"))?;
        Ok(())
    }

    pub async fn delete_session(&self, name: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM session WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|err| format!("Failed to delete session: {err}"))?;
        Ok(())
    }

    pub async fn get_base_branch(&self, name: &str) -> Result<Option<String>, String> {
        let row = sqlx::query("SELECT base_branch FROM session WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| format!("Failed to get base branch: {err}"))?;

        Ok(row.map(|row| row.get("base_branch")))
    }
}

#[cfg(test)]
impl Database {
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
    async fn test_insert_session() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");

        // Act
        let result = db.insert_session("sess1", "claude", "main", "Done").await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_insert_duplicate_session_fails() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        db.insert_session("sess1", "claude", "main", "Done")
            .await
            .expect("failed to insert");

        // Act
        let result = db
            .insert_session("sess1", "gemini", "develop", "Done")
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
    async fn test_load_sessions_ordered_by_name() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        db.insert_session("beta", "claude", "main", "Done")
            .await
            .expect("failed to insert");
        db.insert_session("alpha", "gemini", "develop", "InProgress")
            .await
            .expect("failed to insert");

        // Act
        let sessions = db.load_sessions().await.expect("failed to load");

        // Assert
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "alpha");
        assert_eq!(sessions[0].agent, "gemini");
        assert_eq!(sessions[0].base_branch, "develop");
        assert_eq!(sessions[0].status, "InProgress");
        assert_eq!(sessions[1].name, "beta");
        assert_eq!(sessions[1].agent, "claude");
        assert_eq!(sessions[1].base_branch, "main");
        assert_eq!(sessions[1].status, "Done");
    }

    #[tokio::test]
    async fn test_update_session_status() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        db.insert_session("sess1", "claude", "main", "InProgress")
            .await
            .expect("failed to insert");

        // Act
        let result = db.update_session_status("sess1", "Done").await;

        // Assert
        assert!(result.is_ok());
        let sessions = db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions[0].status, "Done");
    }

    #[tokio::test]
    async fn test_delete_session() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        db.insert_session("sess1", "claude", "main", "Done")
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
        db.insert_session("sess1", "claude", "main", "Done")
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
}
