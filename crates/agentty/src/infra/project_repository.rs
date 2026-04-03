//! Project-scoped `SQLite` query helpers for [`Database`].

use crate::infra::db::{Database, DbError, ProjectListRow, ProjectRow, unix_timestamp_now};

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

impl Database {
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
        .execute(self.pool())
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
        .fetch_one(self.pool())
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
        .fetch_optional(self.pool())
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
        .fetch_all(self.pool())
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
        .execute(self.pool())
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
        .execute(self.pool())
        .await?;

        Ok(())
    }
}
