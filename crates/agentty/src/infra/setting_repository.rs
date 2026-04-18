//! Setting-scoped `SQLite` query helpers for [`Database`].

use crate::domain::agent::ReasoningLevel;
use crate::domain::setting::SettingName;
use crate::infra::db::{Database, DbError};

/// Scalar row used to return one required setting value.
struct RequiredSettingValueRow {
    value: String,
}

impl Database {
    /// Inserts or updates a setting by name.
    ///
    /// # Errors
    /// Returns an error if the setting row cannot be written.
    pub(crate) async fn upsert_setting(
        &self,
        name: SettingName,
        value: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
        )
        .bind(name.as_str())
        .bind(value)
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Inserts or updates one project-scoped setting by project and name.
    ///
    /// # Errors
    /// Returns an error if the project-setting row cannot be written.
    pub(crate) async fn upsert_project_setting(
        &self,
        project_id: i64,
        name: SettingName,
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
        .bind(name.as_str())
        .bind(value)
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Looks up a setting value by name.
    ///
    /// # Errors
    /// Returns an error if the setting lookup query fails.
    pub(crate) async fn get_setting(&self, name: SettingName) -> Result<Option<String>, DbError> {
        let setting_name = name.as_str();
        let row = sqlx::query_as!(
            RequiredSettingValueRow,
            r#"
SELECT value AS "value!: _"
FROM setting
WHERE name = ?
"#,
            setting_name
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(row.map(|row| row.value))
    }

    /// Looks up one project-scoped setting value by project and name.
    ///
    /// # Errors
    /// Returns an error if the project-setting lookup query fails.
    pub(crate) async fn get_project_setting(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<Option<String>, DbError> {
        let setting_name = name.as_str();
        let row = sqlx::query_as!(
            RequiredSettingValueRow,
            r#"
SELECT value AS "value!: _"
FROM project_setting
WHERE project_id = ?
  AND name = ?
"#,
            project_id,
            setting_name
        )
        .fetch_optional(self.pool())
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
            SettingName::ReasoningLevel,
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
            .get_project_setting(project_id, SettingName::ReasoningLevel)
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
        self.upsert_setting(SettingName::ReasoningLevel, reasoning_level.codex())
            .await
    }

    /// Loads the persisted reasoning-effort setting.
    ///
    /// Missing or unparsable values fall back to [`ReasoningLevel::default`].
    ///
    /// # Errors
    /// Returns an error if settings lookup fails.
    pub async fn load_reasoning_level(&self) -> Result<ReasoningLevel, DbError> {
        let setting_value = self.get_setting(SettingName::ReasoningLevel).await?;

        let reasoning_level = setting_value
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok())
            .unwrap_or_default();

        Ok(reasoning_level)
    }

    /// Persists the active project identifier in application settings.
    ///
    /// # Errors
    /// Returns an error if settings persistence fails.
    pub async fn set_active_project_id(&self, project_id: i64) -> Result<(), DbError> {
        self.upsert_setting(SettingName::ActiveProjectId, &project_id.to_string())
            .await
    }

    /// Loads the active project identifier from application settings.
    ///
    /// # Errors
    /// Returns an error if settings lookup fails.
    pub async fn load_active_project_id(&self) -> Result<Option<i64>, DbError> {
        let setting_value = self.get_setting(SettingName::ActiveProjectId).await?;

        Ok(setting_value.and_then(|value| value.parse::<i64>().ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens an isolated in-memory database for each setting-repository test
    /// so the tests do not share project rows or settings state.
    async fn open_test_db() -> Database {
        Database::open_in_memory()
            .await
            .expect("failed to open in-memory db")
    }

    /// Inserts a stable project row used by project-scoped setting tests.
    async fn insert_project(database: &Database) -> i64 {
        database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project")
    }

    #[tokio::test]
    async fn upsert_setting_inserts_new_row_then_replaces_value_on_conflict() {
        // Arrange
        let database = open_test_db().await;

        // Act
        database
            .upsert_setting(SettingName::OpenCommand, "npm run dev")
            .await
            .expect("failed to insert setting");
        let initial_value = database
            .get_setting(SettingName::OpenCommand)
            .await
            .expect("failed to load setting");
        database
            .upsert_setting(SettingName::OpenCommand, "cargo test")
            .await
            .expect("failed to update setting");
        let updated_value = database
            .get_setting(SettingName::OpenCommand)
            .await
            .expect("failed to reload setting");

        // Assert
        assert_eq!(initial_value, Some("npm run dev".to_string()));
        assert_eq!(updated_value, Some("cargo test".to_string()));
    }

    #[tokio::test]
    async fn get_setting_returns_none_when_not_set() {
        // Arrange
        let database = open_test_db().await;

        // Act
        let value = database
            .get_setting(SettingName::OpenCommand)
            .await
            .expect("failed to look up missing setting");

        // Assert
        assert!(value.is_none());
    }

    #[tokio::test]
    async fn upsert_project_setting_isolates_values_between_projects() {
        // Arrange
        let database = open_test_db().await;
        let first_project_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project("/tmp/project-b", Some("main"))
            .await
            .expect("failed to insert second project");

        // Act
        database
            .upsert_project_setting(first_project_id, SettingName::OpenCommand, "npm run dev")
            .await
            .expect("failed to persist first project setting");
        database
            .upsert_project_setting(second_project_id, SettingName::OpenCommand, "cargo test")
            .await
            .expect("failed to persist second project setting");
        let first_value = database
            .get_project_setting(first_project_id, SettingName::OpenCommand)
            .await
            .expect("failed to load first project setting");
        let second_value = database
            .get_project_setting(second_project_id, SettingName::OpenCommand)
            .await
            .expect("failed to load second project setting");

        // Assert
        assert_eq!(first_value, Some("npm run dev".to_string()));
        assert_eq!(second_value, Some("cargo test".to_string()));
    }

    #[tokio::test]
    async fn upsert_project_setting_overwrites_value_on_conflict() {
        // Arrange
        let database = open_test_db().await;
        let project_id = insert_project(&database).await;
        database
            .upsert_project_setting(project_id, SettingName::OpenCommand, "old value")
            .await
            .expect("failed to persist initial project setting");

        // Act
        database
            .upsert_project_setting(project_id, SettingName::OpenCommand, "new value")
            .await
            .expect("failed to overwrite project setting");
        let value = database
            .get_project_setting(project_id, SettingName::OpenCommand)
            .await
            .expect("failed to load project setting");

        // Assert
        assert_eq!(value, Some("new value".to_string()));
    }

    #[tokio::test]
    async fn get_project_setting_returns_none_when_not_set() {
        // Arrange
        let database = open_test_db().await;
        let project_id = insert_project(&database).await;

        // Act
        let value = database
            .get_project_setting(project_id, SettingName::OpenCommand)
            .await
            .expect("failed to look up missing project setting");

        // Assert
        assert!(value.is_none());
    }

    #[tokio::test]
    async fn project_reasoning_level_round_trips_through_typed_helpers() {
        // Arrange
        let database = open_test_db().await;
        let project_id = insert_project(&database).await;

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
    async fn load_project_reasoning_level_falls_back_to_default_when_missing_or_invalid() {
        // Arrange
        let database = open_test_db().await;
        let project_id = insert_project(&database).await;

        // Act
        let missing = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load default project reasoning level");
        database
            .upsert_project_setting(project_id, SettingName::ReasoningLevel, "garbage")
            .await
            .expect("failed to insert invalid reasoning level value");
        let invalid = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load fallback project reasoning level");

        // Assert
        assert_eq!(missing, ReasoningLevel::default());
        assert_eq!(invalid, ReasoningLevel::default());
    }

    #[tokio::test]
    async fn reasoning_level_round_trips_through_typed_helpers() {
        // Arrange
        let database = open_test_db().await;

        // Act
        database
            .set_reasoning_level(ReasoningLevel::Medium)
            .await
            .expect("failed to persist reasoning level");
        let reasoning_level = database
            .load_reasoning_level()
            .await
            .expect("failed to load reasoning level");

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::Medium);
    }

    #[tokio::test]
    async fn load_reasoning_level_falls_back_to_default_when_missing_or_invalid() {
        // Arrange
        let database = open_test_db().await;

        // Act
        let missing = database
            .load_reasoning_level()
            .await
            .expect("failed to load default reasoning level");
        database
            .upsert_setting(SettingName::ReasoningLevel, "garbage")
            .await
            .expect("failed to insert invalid reasoning level value");
        let invalid = database
            .load_reasoning_level()
            .await
            .expect("failed to load fallback reasoning level");

        // Assert
        assert_eq!(missing, ReasoningLevel::default());
        assert_eq!(invalid, ReasoningLevel::default());
    }

    #[tokio::test]
    async fn active_project_id_round_trips_through_typed_helpers() {
        // Arrange
        let database = open_test_db().await;
        let project_id = insert_project(&database).await;

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
    async fn load_active_project_id_returns_none_when_unset_or_unparsable() {
        // Arrange
        let database = open_test_db().await;

        // Act
        let unset = database
            .load_active_project_id()
            .await
            .expect("failed to load unset active project id");
        database
            .upsert_setting(SettingName::ActiveProjectId, "not-a-number")
            .await
            .expect("failed to insert invalid active project id value");
        let invalid = database
            .load_active_project_id()
            .await
            .expect("failed to load fallback active project id");

        // Assert
        assert!(unset.is_none());
        assert!(invalid.is_none());
    }
}
