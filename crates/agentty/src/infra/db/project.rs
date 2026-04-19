//! Project-scoped persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::infra::db::{DbError, unix_timestamp_now};

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

/// Project-focused persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait ProjectRepository: Send + Sync {
    /// Looks up a project by identifier.
    async fn get_project(&self, id: i64) -> Result<Option<ProjectRow>, DbError>;

    /// Loads all configured projects with aggregated session stats.
    async fn load_projects_with_stats(&self) -> Result<Vec<ProjectListRow>, DbError>;

    #[cfg(test)]
    /// Updates favorite state for one project.
    async fn set_project_favorite(&self, project_id: i64, is_favorite: bool)
    -> Result<(), DbError>;

    /// Marks a project as recently opened at the current Unix timestamp.
    async fn touch_project_last_opened(&self, project_id: i64) -> Result<(), DbError>;

    /// Inserts or updates a project by path and returns its identifier.
    async fn upsert_project(&self, path: &str, git_branch: Option<String>) -> Result<i64, DbError>;
}

/// `SQLite` implementation of [`ProjectRepository`].
#[derive(Clone)]
pub(crate) struct SqliteProjectRepository(SqlitePool);

impl SqliteProjectRepository {
    /// Creates a project repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

/// Scalar row used to return one required project identifier.
struct ProjectIdValueRow {
    value: i64,
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
    /// Converts optional joined aggregate values into the public row shape.
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

#[async_trait]
impl ProjectRepository for SqliteProjectRepository {
    async fn get_project(&self, id: i64) -> Result<Option<ProjectRow>, DbError> {
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
        .fetch_optional(&self.0)
        .await?;

        Ok(row)
    }

    async fn load_projects_with_stats(&self) -> Result<Vec<ProjectListRow>, DbError> {
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
        .fetch_all(&self.0)
        .await?;

        Ok(rows
            .into_iter()
            .map(ProjectListQueryRow::into_project_list_row)
            .collect())
    }

    #[cfg(test)]
    async fn set_project_favorite(
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
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn touch_project_last_opened(&self, project_id: i64) -> Result<(), DbError> {
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
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn upsert_project(&self, path: &str, git_branch: Option<String>) -> Result<i64, DbError> {
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
        .bind(git_branch.as_deref())
        .execute(&self.0)
        .await?;

        let row = sqlx::query_as!(
            ProjectIdValueRow,
            r#"
SELECT id AS "value!: _"
FROM project
WHERE path = ?
"#,
            path
        )
        .fetch_one(&self.0)
        .await?;

        Ok(row.value)
    }
}
