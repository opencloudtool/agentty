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

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens an isolated in-memory database for each repository test so the
    /// tests do not share project rows or settings state.
    async fn open_test_db() -> Database {
        Database::open_in_memory()
            .await
            .expect("failed to open in-memory db")
    }

    #[tokio::test]
    async fn upsert_project_inserts_new_path_and_returns_id() {
        // Arrange
        let database = open_test_db().await;

        // Act
        let project_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to upsert project");
        let project = database
            .get_project(project_id)
            .await
            .expect("failed to load project")
            .expect("expected project to exist");

        // Assert
        assert_eq!(project.id, project_id);
        assert_eq!(project.path, "/tmp/project-a");
        assert_eq!(project.git_branch.as_deref(), Some("main"));
        assert!(!project.is_favorite);
        assert!(project.last_opened_at.is_none());
    }

    #[tokio::test]
    async fn upsert_project_updates_git_branch_for_existing_path() {
        // Arrange
        let database = open_test_db().await;
        let original_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to insert original project");

        // Act
        let updated_id = database
            .upsert_project("/tmp/project-a", Some("develop"))
            .await
            .expect("failed to update existing project");
        let project = database
            .get_project(updated_id)
            .await
            .expect("failed to load project")
            .expect("expected project to exist");

        // Assert
        assert_eq!(updated_id, original_id);
        assert_eq!(project.git_branch.as_deref(), Some("develop"));
    }

    #[tokio::test]
    async fn upsert_project_clears_git_branch_when_none() {
        // Arrange
        let database = open_test_db().await;
        let project_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to insert project");

        // Act
        database
            .upsert_project("/tmp/project-a", None)
            .await
            .expect("failed to clear git branch");
        let project = database
            .get_project(project_id)
            .await
            .expect("failed to load project")
            .expect("expected project to exist");

        // Assert
        assert!(project.git_branch.is_none());
    }

    #[tokio::test]
    async fn get_project_returns_none_for_missing_id() {
        // Arrange
        let database = open_test_db().await;

        // Act
        let project = database
            .get_project(999)
            .await
            .expect("failed to look up project");

        // Assert
        assert!(project.is_none());
    }

    #[tokio::test]
    async fn load_projects_with_stats_returns_zero_aggregates_when_no_sessions_present() {
        // Arrange
        let database = open_test_db().await;
        database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to upsert project");

        // Act
        let projects = database
            .load_projects_with_stats()
            .await
            .expect("failed to load projects with stats");

        // Assert
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].session_count, 0);
        assert_eq!(projects[0].active_session_count, 0);
        assert!(projects[0].last_session_updated_at.is_none());
    }

    #[tokio::test]
    async fn load_projects_with_stats_excludes_terminal_states_from_active_count() {
        // Arrange
        let database = open_test_db().await;
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-active",
                "gpt-5.4",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert active session");
        database
            .insert_session("session-done", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert completed session");
        database
            .insert_session("session-queued", "gpt-5.4", "main", "Queued", project_id)
            .await
            .expect("failed to insert queued session");

        // Act
        let projects = database
            .load_projects_with_stats()
            .await
            .expect("failed to load projects with stats");

        // Assert
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].session_count, 3);
        assert_eq!(projects[0].active_session_count, 1);
        assert!(projects[0].last_session_updated_at.is_some());
    }

    #[tokio::test]
    async fn load_projects_with_stats_orders_favorites_before_others() {
        // Arrange
        let database = open_test_db().await;
        let plain_id = database
            .upsert_project("/tmp/plain", Some("main"))
            .await
            .expect("failed to insert plain project");
        let favorite_id = database
            .upsert_project("/tmp/favorite", Some("main"))
            .await
            .expect("failed to insert favorite project");
        database
            .set_project_favorite(favorite_id, true)
            .await
            .expect("failed to mark favorite");

        // Act
        let projects = database
            .load_projects_with_stats()
            .await
            .expect("failed to load projects with stats");

        // Assert
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].id, favorite_id);
        assert!(projects[0].is_favorite);
        assert_eq!(projects[1].id, plain_id);
        assert!(!projects[1].is_favorite);
    }

    #[tokio::test]
    async fn touch_project_last_opened_sets_timestamp() {
        // Arrange
        let database = open_test_db().await;
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        let before_touch = database
            .get_project(project_id)
            .await
            .expect("failed to load project")
            .expect("expected project to exist");
        assert!(before_touch.last_opened_at.is_none());

        // Act
        database
            .touch_project_last_opened(project_id)
            .await
            .expect("failed to touch project");
        let after_touch = database
            .get_project(project_id)
            .await
            .expect("failed to reload project")
            .expect("expected project to exist");

        // Assert
        assert!(after_touch.last_opened_at.is_some());
        assert!(after_touch.updated_at >= before_touch.updated_at);
    }

    #[tokio::test]
    async fn set_project_favorite_can_toggle_state_in_both_directions() {
        // Arrange
        let database = open_test_db().await;
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        // Act
        database
            .set_project_favorite(project_id, true)
            .await
            .expect("failed to mark favorite");
        let after_set = database
            .get_project(project_id)
            .await
            .expect("failed to load project")
            .expect("expected project to exist");

        database
            .set_project_favorite(project_id, false)
            .await
            .expect("failed to clear favorite");
        let after_clear = database
            .get_project(project_id)
            .await
            .expect("failed to reload project")
            .expect("expected project to exist");

        // Assert
        assert!(after_set.is_favorite);
        assert!(!after_clear.is_favorite);
    }
}
