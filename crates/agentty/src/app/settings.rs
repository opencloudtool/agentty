use ratatui::widgets::TableState;

use crate::agent::{AgentKind, AgentModel};
use crate::app::AppServices;

/// Names of persisted application settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingName {
    DefaultModel,
    DevServer,
    LongestSessionDurationSeconds,
}

impl SettingName {
    /// Returns the persisted key name for this setting.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DefaultModel => "DefaultModel",
            Self::DevServer => "DevServer",
            Self::LongestSessionDurationSeconds => "LongestSessionDurationSeconds",
        }
    }
}

/// Declares how a settings row is edited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingControl {
    Selector,
    TextInput,
}

/// Backing table rows for the settings page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingRow {
    DefaultModel,
    DevServer,
}

impl SettingRow {
    const ALL: [Self; 2] = [Self::DefaultModel, Self::DevServer];
    const ROW_COUNT: usize = Self::ALL.len();

    /// Builds a row selector from the table row index.
    fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::DefaultModel)
    }

    /// Returns the display label for the row.
    fn label(self) -> &'static str {
        match self {
            Self::DefaultModel => "Default Model",
            Self::DevServer => "Dev Server",
        }
    }

    /// Returns how this row is edited.
    fn control(self) -> SettingControl {
        match self {
            Self::DefaultModel => SettingControl::Selector,
            Self::DevServer => SettingControl::TextInput,
        }
    }

    /// Returns the persisted setting name represented by this row.
    fn setting_name(self) -> SettingName {
        match self {
            Self::DefaultModel => SettingName::DefaultModel,
            Self::DevServer => SettingName::DevServer,
        }
    }
}

/// Manages user-configurable application settings.
pub struct SettingsManager {
    pub default_model: AgentModel,
    pub dev_server: String,
    pub table_state: TableState,
    editing_text_row: Option<SettingRow>,
}

impl SettingsManager {
    /// Creates a settings manager and loads persisted values from the database.
    pub async fn new(services: &AppServices) -> Self {
        let default_model = services
            .db()
            .get_setting(SettingName::DefaultModel.as_str())
            .await
            .unwrap_or(None)
            .and_then(|setting| setting.parse().ok())
            .unwrap_or_else(|| AgentKind::Gemini.default_model());

        let dev_server = services
            .db()
            .get_setting(SettingName::DevServer.as_str())
            .await
            .unwrap_or(None)
            .unwrap_or_default();

        let mut table_state = TableState::default();
        table_state.select(Some(0));

        Self {
            default_model,
            dev_server,
            table_state,
            editing_text_row: None,
        }
    }

    /// Moves the settings selection to the next row.
    pub fn next(&mut self) {
        if !self.is_editing_text_input() {
            let next_index = (self.selected_row_index() + 1) % SettingRow::ROW_COUNT;
            self.table_state.select(Some(next_index));
        }
    }

    /// Moves the settings selection to the previous row.
    pub fn previous(&mut self) {
        if !self.is_editing_text_input() {
            let current_index = self.selected_row_index();
            let previous_index = if current_index == 0 {
                SettingRow::ROW_COUNT - 1
            } else {
                current_index - 1
            };
            self.table_state.select(Some(previous_index));
        }
    }

    /// Handles the primary action for the selected setting row.
    pub async fn handle_enter(&mut self, services: &AppServices) {
        let selected_row = self.selected_row();

        match selected_row.control() {
            SettingControl::Selector => {
                self.cycle_selector_row(services, selected_row).await;
            }
            SettingControl::TextInput => {
                self.toggle_text_input(selected_row);
            }
        }
    }

    /// Returns whether any settings text input editor is active.
    #[must_use]
    pub fn is_editing_text_input(&self) -> bool {
        self.editing_text_row.is_some()
    }

    /// Exits settings text input editing mode.
    pub fn stop_text_input_editing(&mut self) {
        self.editing_text_row = None;
    }

    /// Appends one character to the selected text setting value and persists
    /// it.
    pub async fn append_selected_text_character(
        &mut self,
        services: &AppServices,
        character: char,
    ) {
        if let Some(editing_row) = self.editing_text_row
            && self.append_text_character(editing_row, character)
        {
            self.persist_text_setting(services, editing_row).await;
        }
    }

    /// Removes the last character from the selected text setting and persists
    /// it.
    pub async fn remove_selected_text_character(&mut self, services: &AppServices) {
        if let Some(editing_row) = self.editing_text_row
            && self.remove_text_character(editing_row)
        {
            self.persist_text_setting(services, editing_row).await;
        }
    }

    /// Returns settings table rows as `(name, value)` pairs.
    #[must_use]
    pub fn settings_rows(&self) -> Vec<(&'static str, String)> {
        SettingRow::ALL
            .iter()
            .map(|row| (row.label(), self.display_value_for_row(*row)))
            .collect()
    }

    /// Returns the footer hint text for the settings page.
    #[must_use]
    pub fn footer_hint(&self) -> &'static str {
        if self.is_editing_text_input() {
            "Editing setting value: type text, Enter to finish, Esc to cancel"
        } else {
            "Settings: Enter cycles selector values or starts text editing"
        }
    }

    /// Returns the currently selected row index.
    fn selected_row_index(&self) -> usize {
        self.table_state
            .selected()
            .unwrap_or(0)
            .min(SettingRow::ROW_COUNT - 1)
    }

    /// Returns the currently selected settings row.
    fn selected_row(&self) -> SettingRow {
        SettingRow::from_index(self.selected_row_index())
    }

    /// Returns whether a specific text-input row is currently being edited.
    fn is_editing_text_input_for(&self, row: SettingRow) -> bool {
        self.editing_text_row == Some(row)
    }

    /// Toggles text editing mode for the requested row.
    fn toggle_text_input(&mut self, row: SettingRow) {
        if self.editing_text_row == Some(row) {
            self.editing_text_row = None;

            return;
        }

        self.editing_text_row = Some(row);
    }

    /// Appends text to the selected setting row.
    fn append_text_character(&mut self, row: SettingRow, character: char) -> bool {
        match row.control() {
            SettingControl::Selector => false,
            SettingControl::TextInput => {
                self.dev_server.push(character);

                true
            }
        }
    }

    /// Removes one character from the selected setting row.
    fn remove_text_character(&mut self, row: SettingRow) -> bool {
        match row.control() {
            SettingControl::Selector => false,
            SettingControl::TextInput => self.dev_server.pop().is_some(),
        }
    }

    /// Returns the text displayed for a row value.
    fn display_value_for_row(&self, row: SettingRow) -> String {
        match row.control() {
            SettingControl::Selector => self.default_model.as_str().to_string(),
            SettingControl::TextInput => {
                if self.is_editing_text_input_for(row) {
                    format!("{}|", self.dev_server)
                } else if self.dev_server.is_empty() {
                    "<empty>".to_string()
                } else {
                    self.dev_server.clone()
                }
            }
        }
    }

    /// Cycles selector-type rows and persists their updated values.
    async fn cycle_selector_row(&mut self, services: &AppServices, row: SettingRow) {
        if matches!(row.control(), SettingControl::TextInput) {
            return;
        }

        match row.setting_name() {
            SettingName::DefaultModel => {
                self.cycle_model(services).await;
            }
            SettingName::DevServer | SettingName::LongestSessionDurationSeconds => {}
        }
    }

    /// Persists the current value for a text-input row.
    async fn persist_text_setting(&self, services: &AppServices, row: SettingRow) {
        if !matches!(row.control(), SettingControl::TextInput) {
            return;
        }

        match row.setting_name() {
            SettingName::DevServer => {
                let _ = services
                    .db()
                    .upsert_setting(SettingName::DevServer.as_str(), &self.dev_server)
                    .await;
            }
            SettingName::DefaultModel | SettingName::LongestSessionDurationSeconds => {}
        }
    }

    /// Cycles the currently selected model and persists it.
    async fn cycle_model(&mut self, services: &AppServices) {
        let all_models: Vec<AgentModel> = AgentKind::ALL
            .iter()
            .flat_map(|kind| kind.models())
            .copied()
            .collect();

        let current_index = all_models
            .iter()
            .position(|model| *model == self.default_model)
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
    use ratatui::widgets::TableState;

    use super::*;

    fn new_settings_manager() -> SettingsManager {
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        SettingsManager {
            default_model: AgentKind::Gemini.default_model(),
            dev_server: String::new(),
            table_state,
            editing_text_row: None,
        }
    }

    #[test]
    fn setting_name_as_str_returns_default_model() {
        // Arrange & Act
        let setting_name = SettingName::DefaultModel.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultModel");
    }

    #[test]
    fn setting_name_as_str_returns_dev_server() {
        // Arrange & Act
        let setting_name = SettingName::DevServer.as_str();

        // Assert
        assert_eq!(setting_name, "DevServer");
    }

    #[test]
    fn next_moves_selection_to_dev_server_row() {
        // Arrange
        let mut manager = new_settings_manager();

        // Act
        manager.next();

        // Assert
        assert_eq!(manager.table_state.selected(), Some(1));
    }

    #[test]
    fn previous_wraps_to_dev_server_row_from_default_model_row() {
        // Arrange
        let mut manager = new_settings_manager();

        // Act
        manager.previous();

        // Assert
        assert_eq!(manager.table_state.selected(), Some(1));
    }

    #[test]
    fn is_editing_text_input_returns_false_by_default() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let is_editing = manager.is_editing_text_input();

        // Assert
        assert!(!is_editing);
    }

    #[test]
    fn settings_rows_include_default_model_and_dev_server() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "Default Model");
        assert_eq!(rows[1].0, "Dev Server");
    }

    #[test]
    fn footer_hint_returns_editing_text_when_text_input_is_active() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.editing_text_row = Some(SettingRow::DevServer);

        // Act
        let footer_hint = manager.footer_hint();

        // Assert
        assert_eq!(
            footer_hint,
            "Editing setting value: type text, Enter to finish, Esc to cancel"
        );
    }

    #[test]
    fn settings_rows_show_empty_placeholder_for_dev_server() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[1].1, "<empty>");
    }

    #[test]
    fn settings_rows_show_cursor_for_dev_server_while_editing() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.dev_server = "http://localhost:5173".to_string();
        manager.editing_text_row = Some(SettingRow::DevServer);

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[1].1, "http://localhost:5173|");
    }
}
