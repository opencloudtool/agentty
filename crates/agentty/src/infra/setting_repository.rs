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
