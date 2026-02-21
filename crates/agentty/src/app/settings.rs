use ratatui::widgets::TableState;

use crate::agent::{AgentKind, AgentModel};
use crate::app::AppServices;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingName {
    DefaultModel,
}

impl SettingName {
    fn as_str(self) -> &'static str {
        match self {
            Self::DefaultModel => "DefaultModel",
        }
    }
}

/// Manages user-configurable application settings.
pub struct SettingsManager {
    pub default_model: AgentModel,
    pub table_state: TableState,
}

impl SettingsManager {
    /// Creates a settings manager and loads persisted values from the database.
    pub async fn new(services: &AppServices) -> Self {
        let default_model = services
            .db()
            .get_setting(SettingName::DefaultModel.as_str())
            .await
            .unwrap_or(None)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| AgentKind::Gemini.default_model());

        let mut table_state = TableState::default();
        table_state.select(Some(0));

        Self {
            default_model,
            table_state,
        }
    }

    /// Moves the settings selection to the next row.
    pub fn next(&mut self) {
        // No-op for single row
    }

    /// Moves the settings selection to the previous row.
    pub fn previous(&mut self) {
        // No-op for single row
    }

    /// Cycles the currently selected model and persists it.
    pub async fn cycle_model(&mut self, services: &AppServices) {
        let all_models: Vec<AgentModel> = AgentKind::ALL
            .iter()
            .flat_map(|kind| kind.models())
            .copied()
            .collect();

        let current_index = all_models
            .iter()
            .position(|m| *m == self.default_model)
            .unwrap_or(0);

        let next_index = (current_index + 1) % all_models.len();
        self.default_model = all_models[next_index];

        let _ = services
            .db()
            .upsert_setting(
                SettingName::DefaultModel.as_str(),
                self.default_model.as_str(),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_returns_default_model_setting_name() {
        // Arrange
        let setting_name = SettingName::DefaultModel;

        // Act
        let setting_name_value = setting_name.as_str();

        // Assert
        assert_eq!(setting_name_value, "DefaultModel");
    }
}
