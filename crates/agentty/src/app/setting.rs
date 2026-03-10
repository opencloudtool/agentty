use ratatui::widgets::TableState;

use crate::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::app::AppServices;
use crate::domain::input::InputState;
use crate::domain::setting::SettingName;

/// Loads the persisted smart-model default used for new sessions.
///
/// This prefers the new `DefaultSmartModel` key and falls back to the legacy
/// `DefaultModel` key for backward compatibility.
pub(crate) async fn load_default_smart_model_setting(
    services: &AppServices,
    project_id: Option<i64>,
    fallback_model: AgentModel,
) -> AgentModel {
    if let Some(model) =
        load_model_setting(services, project_id, SettingName::DefaultSmartModel).await
    {
        return model;
    }

    if let Some(model) = load_legacy_default_smart_model_setting(services).await {
        return model;
    }

    fallback_model
}

/// Loads the persisted fast-model default used by lightweight background
/// workflows.
///
/// This prefers `DefaultFastModel` and falls back to the resolved smart-model
/// default when the fast-model setting is missing.
pub(crate) async fn load_default_fast_model_setting(
    services: &AppServices,
    project_id: Option<i64>,
    fallback_model: AgentModel,
) -> AgentModel {
    if let Some(model) =
        load_model_setting(services, project_id, SettingName::DefaultFastModel).await
    {
        return model;
    }

    load_default_smart_model_setting(services, project_id, fallback_model).await
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
    ReasoningLevel,
    DefaultSmartModel,
    DefaultFastModel,
    DefaultReviewModel,
    OpenCommand,
}

impl SettingRow {
    const ALL: [Self; 5] = [
        Self::ReasoningLevel,
        Self::DefaultSmartModel,
        Self::DefaultFastModel,
        Self::DefaultReviewModel,
        Self::OpenCommand,
    ];
    const ROW_COUNT: usize = Self::ALL.len();

    /// Builds a row selector from the table row index.
    fn from_index(index: usize) -> Self {
        Self::ALL
            .get(index)
            .copied()
            .unwrap_or(Self::ReasoningLevel)
    }

    /// Returns the display label for the row.
    fn label(self) -> &'static str {
        match self {
            Self::ReasoningLevel => "Reasoning Level",
            Self::DefaultSmartModel => "Default Smart Model",
            Self::DefaultFastModel => "Default Fast Model",
            Self::DefaultReviewModel => "Default Review Model",
            Self::OpenCommand => "Open Commands",
        }
    }

    /// Returns how this row is edited.
    fn control(self) -> SettingControl {
        match self {
            Self::ReasoningLevel
            | Self::DefaultSmartModel
            | Self::DefaultFastModel
            | Self::DefaultReviewModel => SettingControl::Selector,
            Self::OpenCommand => SettingControl::TextInput,
        }
    }

    /// Returns the persisted setting name represented by this row.
    fn setting_name(self) -> SettingName {
        match self {
            Self::ReasoningLevel => SettingName::ReasoningLevel,
            Self::DefaultSmartModel => SettingName::DefaultSmartModel,
            Self::DefaultFastModel => SettingName::DefaultFastModel,
            Self::DefaultReviewModel => SettingName::DefaultReviewModel,
            Self::OpenCommand => SettingName::OpenCommand,
        }
    }
}

/// Manages user-configurable application settings.
pub struct SettingsManager {
    /// Default fast model used by fast-path workflows.
    pub default_fast_model: AgentModel,
    /// Default model used by review workflows.
    pub default_review_model: AgentModel,
    /// Default smart model used when creating new sessions.
    pub default_smart_model: AgentModel,
    /// Optional command run in tmux when opening a session worktree.
    pub open_command: String,
    /// Reasoning effort preference for models that support this setting.
    ///
    /// Currently applied to Codex turns.
    pub reasoning_level: ReasoningLevel,
    /// Table selection state for the settings page.
    pub table_state: TableState,
    editing_text_row: Option<SettingRow>,
    open_command_input: Option<InputState>,
    /// Active project identifier that owns these persisted settings.
    project_id: i64,
    use_last_used_model_as_default: bool,
}

impl SettingsManager {
    /// Creates a settings manager and loads persisted values from the database.
    pub async fn new(services: &AppServices, project_id: i64) -> Self {
        let default_smart_model = load_default_smart_model_setting(
            services,
            Some(project_id),
            AgentKind::Gemini.default_model(),
        )
        .await;

        let default_fast_model =
            load_default_fast_model_setting(services, Some(project_id), default_smart_model).await;

        let default_review_model =
            load_model_setting(services, Some(project_id), SettingName::DefaultReviewModel)
                .await
                .unwrap_or(default_smart_model);
        let reasoning_level = load_reasoning_level_setting(services, Some(project_id)).await;

        let open_command = services
            .db()
            .get_project_setting(project_id, SettingName::OpenCommand.as_str())
            .await
            .unwrap_or(None)
            .unwrap_or_default();

        let use_last_used_model_as_default = services
            .db()
            .get_project_setting(project_id, SettingName::LastUsedModelAsDefault.as_str())
            .await
            .unwrap_or(None)
            .and_then(|setting| setting.parse::<bool>().ok())
            .unwrap_or(false);

        let mut table_state = TableState::default();
        table_state.select(Some(0));

        Self {
            default_fast_model,
            default_review_model,
            default_smart_model,
            open_command,
            reasoning_level,
            table_state,
            editing_text_row: None,
            open_command_input: None,
            project_id,
            use_last_used_model_as_default,
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

    /// Returns whether the `Open Commands` multiline editor is currently
    /// active.
    #[must_use]
    pub fn is_editing_open_commands(&self) -> bool {
        self.is_editing_text_input_for(SettingRow::OpenCommand)
    }

    /// Exits settings text input editing mode and clears editor cursor state.
    pub fn stop_text_input_editing(&mut self) {
        self.finish_text_input_editing();
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

    /// Moves the active text editor cursor one character to the left.
    pub fn move_selected_text_cursor_left(&mut self) {
        self.move_selected_text_cursor(TextCursorDirection::Left);
    }

    /// Moves the active text editor cursor one character to the right.
    pub fn move_selected_text_cursor_right(&mut self) {
        self.move_selected_text_cursor(TextCursorDirection::Right);
    }

    /// Moves the active text editor cursor to the previous line.
    pub fn move_selected_text_cursor_up(&mut self) {
        self.move_selected_text_cursor(TextCursorDirection::Up);
    }

    /// Moves the active text editor cursor to the next line.
    pub fn move_selected_text_cursor_down(&mut self) {
        self.move_selected_text_cursor(TextCursorDirection::Down);
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
        if self.is_editing_text_input_for(SettingRow::OpenCommand) {
            "Editing open commands: one command per line, Alt+Enter/Shift+Enter inserts newline, \
             Enter/Esc finish"
        } else if self.is_editing_text_input() {
            "Editing setting value: type text, Enter to finish, Esc to cancel"
        } else {
            "Settings: Enter cycles selector values or starts text editing"
        }
    }

    /// Returns configured open commands in persisted order.
    ///
    /// Commands are split by newlines and trimmed.
    #[must_use]
    pub fn open_commands(&self) -> Vec<String> {
        parse_open_commands(self.open_command.as_str())
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
            self.finish_text_input_editing();

            return;
        }

        self.start_text_input_editing(row);
    }

    /// Starts text editing mode for the requested row.
    fn start_text_input_editing(&mut self, row: SettingRow) {
        self.editing_text_row = Some(row);
        if row == SettingRow::OpenCommand {
            self.open_command_input = Some(InputState::with_text(self.open_command.clone()));
        }
    }

    /// Finalizes the active text editing session and synchronizes cached text.
    fn finish_text_input_editing(&mut self) {
        if self.is_editing_text_input_for(SettingRow::OpenCommand) {
            self.sync_open_command_from_input();
            self.open_command_input = None;
        }

        self.editing_text_row = None;
    }

    /// Appends text to the selected setting row.
    fn append_text_character(&mut self, row: SettingRow, character: char) -> bool {
        match row.control() {
            SettingControl::Selector => false,
            SettingControl::TextInput => {
                let open_command_input = self.open_command_input_mut();
                open_command_input.insert_char(character);
                self.sync_open_command_from_input();

                true
            }
        }
    }

    /// Removes one character from the selected setting row.
    fn remove_text_character(&mut self, row: SettingRow) -> bool {
        match row.control() {
            SettingControl::Selector => false,
            SettingControl::TextInput => {
                let open_command_input = self.open_command_input_mut();
                let previous_text = open_command_input.text().to_string();
                open_command_input.delete_backward();

                let is_changed = open_command_input.text() != previous_text;
                if is_changed {
                    self.sync_open_command_from_input();
                }

                is_changed
            }
        }
    }

    /// Moves the active text editor cursor for the selected row.
    fn move_selected_text_cursor(&mut self, direction: TextCursorDirection) {
        if let Some(editing_row) = self.editing_text_row {
            self.move_text_cursor(editing_row, direction);
        }
    }

    /// Moves the text editor cursor for the given row.
    fn move_text_cursor(&mut self, row: SettingRow, direction: TextCursorDirection) {
        if !matches!(row.control(), SettingControl::TextInput) {
            return;
        }

        let open_command_input = self.open_command_input_mut();
        match direction {
            TextCursorDirection::Down => open_command_input.move_down(),
            TextCursorDirection::Left => open_command_input.move_left(),
            TextCursorDirection::Right => open_command_input.move_right(),
            TextCursorDirection::Up => open_command_input.move_up(),
        }
    }

    /// Returns mutable access to the `Open Commands` editor state.
    fn open_command_input_mut(&mut self) -> &mut InputState {
        let open_command_value = self.open_command.clone();
        self.open_command_input
            .get_or_insert_with(|| InputState::with_text(open_command_value))
    }

    /// Synchronizes the persisted `open_command` value from editor state.
    fn sync_open_command_from_input(&mut self) {
        if let Some(open_command_input) = &self.open_command_input {
            self.open_command = open_command_input.text().to_string();
        }
    }

    /// Returns the text displayed for a row value.
    fn display_value_for_row(&self, row: SettingRow) -> String {
        match row {
            SettingRow::ReasoningLevel => self.reasoning_level.codex().to_string(),
            SettingRow::DefaultSmartModel => {
                if self.use_last_used_model_as_default {
                    "Last used model as default".to_string()
                } else {
                    self.default_smart_model.as_str().to_string()
                }
            }
            SettingRow::DefaultFastModel => self.default_fast_model.as_str().to_string(),
            SettingRow::DefaultReviewModel => self.default_review_model.as_str().to_string(),
            SettingRow::OpenCommand => {
                if self.is_editing_text_input_for(row) {
                    display_open_command_with_cursor(&self.open_command, self.open_command_cursor())
                } else if self.open_command.is_empty() {
                    "<empty>".to_string()
                } else {
                    self.open_command.clone()
                }
            }
        }
    }

    /// Returns the active `Open Commands` editor cursor position.
    fn open_command_cursor(&self) -> usize {
        self.open_command_input
            .as_ref()
            .map_or_else(|| self.open_command.chars().count(), |input| input.cursor)
    }

    /// Cycles selector-type rows and persists their updated values.
    async fn cycle_selector_row(&mut self, services: &AppServices, row: SettingRow) {
        if matches!(row.control(), SettingControl::TextInput) {
            return;
        }

        match row.setting_name() {
            SettingName::ReasoningLevel => {
                self.cycle_reasoning_level_selector(services).await;
            }
            SettingName::DefaultSmartModel => {
                self.cycle_default_smart_model_selector(services).await;
            }
            SettingName::DefaultFastModel => {
                self.cycle_default_fast_model_selector(services).await;
            }
            SettingName::DefaultReviewModel => {
                self.cycle_default_review_model_selector(services).await;
            }
            SettingName::OpenCommand | SettingName::LastUsedModelAsDefault => {}
        }
    }

    /// Persists the current value for a text-input row.
    async fn persist_text_setting(&self, services: &AppServices, row: SettingRow) {
        if !matches!(row.control(), SettingControl::TextInput) {
            return;
        }

        match row.setting_name() {
            SettingName::OpenCommand => {
                let _ = services
                    .db()
                    .upsert_project_setting(
                        self.project_id,
                        SettingName::OpenCommand.as_str(),
                        &self.open_command,
                    )
                    .await;
            }
            SettingName::ReasoningLevel
            | SettingName::DefaultFastModel
            | SettingName::DefaultReviewModel
            | SettingName::DefaultSmartModel
            | SettingName::LastUsedModelAsDefault => {}
        }
    }

    /// Cycles the reasoning-level selector through all supported values.
    async fn cycle_reasoning_level_selector(&mut self, services: &AppServices) {
        let current_index = ReasoningLevel::ALL
            .iter()
            .position(|level| *level == self.reasoning_level)
            .unwrap_or(0);
        let next_index = (current_index + 1) % ReasoningLevel::ALL.len();
        self.reasoning_level = ReasoningLevel::ALL[next_index];

        self.persist_reasoning_level_setting(services).await;
    }

    /// Cycles the smart-model selector through all explicit models and the
    /// `Last used model as default` option.
    async fn cycle_default_smart_model_selector(&mut self, services: &AppServices) {
        let all_models = all_models();
        let explicit_model_count = all_models.len();
        let current_index = if self.use_last_used_model_as_default {
            explicit_model_count
        } else {
            all_models
                .iter()
                .position(|model| *model == self.default_smart_model)
                .unwrap_or(0)
        };
        let next_index = (current_index + 1) % (explicit_model_count + 1);

        if next_index == explicit_model_count {
            self.use_last_used_model_as_default = true;
        } else {
            self.default_smart_model = all_models[next_index];
            self.use_last_used_model_as_default = false;
        }

        self.persist_default_smart_model_settings(services).await;
    }

    /// Cycles the fast-model selector through all explicit models.
    async fn cycle_default_fast_model_selector(&mut self, services: &AppServices) {
        self.default_fast_model = next_model(self.default_fast_model);

        self.persist_default_fast_model_setting(services).await;
    }

    /// Cycles the review-model selector through all explicit models.
    async fn cycle_default_review_model_selector(&mut self, services: &AppServices) {
        self.default_review_model = next_model(self.default_review_model);

        self.persist_default_review_model_setting(services).await;
    }

    /// Persists smart-model selector values (`DefaultSmartModel` and
    /// `LastUsedModelAsDefault`).
    async fn persist_default_smart_model_settings(&self, services: &AppServices) {
        let last_used_model_as_default_value = self.use_last_used_model_as_default.to_string();

        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::DefaultSmartModel.as_str(),
                self.default_smart_model.as_str(),
            )
            .await;
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::LastUsedModelAsDefault.as_str(),
                &last_used_model_as_default_value,
            )
            .await;
    }

    /// Persists the reasoning-level selector value (`ReasoningLevel`).
    async fn persist_reasoning_level_setting(&self, services: &AppServices) {
        let _ = services
            .db()
            .set_project_reasoning_level(self.project_id, self.reasoning_level)
            .await;
    }

    /// Persists the fast-model selector value (`DefaultFastModel`).
    async fn persist_default_fast_model_setting(&self, services: &AppServices) {
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::DefaultFastModel.as_str(),
                self.default_fast_model.as_str(),
            )
            .await;
    }

    /// Persists the review-model selector value (`DefaultReviewModel`).
    async fn persist_default_review_model_setting(&self, services: &AppServices) {
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::DefaultReviewModel.as_str(),
                self.default_review_model.as_str(),
            )
            .await;
    }
}

/// Cursor movement direction for text-input settings rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextCursorDirection {
    Down,
    Left,
    Right,
    Up,
}

/// Parses the settings text field into executable open-command entries.
fn parse_open_commands(open_command_setting: &str) -> Vec<String> {
    open_command_setting
        .lines()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

/// Renders `text` with a `|` cursor marker at `cursor_char_index`.
fn display_open_command_with_cursor(text: &str, cursor_char_index: usize) -> String {
    let mut rendered_text = String::with_capacity(text.len() + 1);
    let char_count = text.chars().count();
    let clamped_cursor_index = cursor_char_index.min(char_count);

    for (char_index, character) in text.chars().enumerate() {
        if char_index == clamped_cursor_index {
            rendered_text.push('|');
        }

        rendered_text.push(character);
    }

    if clamped_cursor_index == char_count {
        rendered_text.push('|');
    }

    rendered_text
}

/// Returns all selectable models in settings display order.
fn all_models() -> Vec<AgentModel> {
    AgentKind::ALL
        .iter()
        .flat_map(|kind| kind.models())
        .copied()
        .collect()
}

/// Returns the next model from the explicit selectable model list.
fn next_model(current_model: AgentModel) -> AgentModel {
    let models = all_models();
    let current_index = models
        .iter()
        .position(|model| *model == current_model)
        .unwrap_or(0);
    let next_index = (current_index + 1) % models.len();

    models[next_index]
}

/// Loads a model setting and parses it into an [`AgentModel`].
async fn load_model_setting(
    services: &AppServices,
    project_id: Option<i64>,
    setting_name: SettingName,
) -> Option<AgentModel> {
    let project_id = project_id?;

    services
        .db()
        .get_project_setting(project_id, setting_name.as_str())
        .await
        .unwrap_or(None)
        .and_then(|setting_value| setting_value.parse().ok())
}

/// Loads the persisted reasoning-level setting.
///
/// Falls back to [`ReasoningLevel::default`] when the setting is missing
/// or cannot be parsed.
async fn load_reasoning_level_setting(
    services: &AppServices,
    project_id: Option<i64>,
) -> ReasoningLevel {
    let Some(project_id) = project_id else {
        return ReasoningLevel::default();
    };

    services
        .db()
        .load_project_reasoning_level(project_id)
        .await
        .unwrap_or_default()
}

/// Loads the legacy smart-model setting from the previous key name.
async fn load_legacy_default_smart_model_setting(services: &AppServices) -> Option<AgentModel> {
    services
        .db()
        .get_setting("DefaultModel")
        .await
        .unwrap_or(None)
        .and_then(|setting_value| setting_value.parse().ok())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use ag_forge as forge;
    use ratatui::widgets::TableState;
    use tokio::sync::mpsc;

    use super::*;
    use crate::db::Database;
    use crate::infra::{app_server, fs, git};

    /// Builds app services backed by an in-memory database for settings tests.
    async fn test_services() -> (AppServices, i64) {
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to create project");
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let services = AppServices::new(
            PathBuf::from("/tmp/agentty-settings-tests"),
            database,
            event_tx,
            Arc::new(fs::MockFsClient::new()),
            Arc::new(git::MockGitClient::new()),
            Arc::new(forge::MockReviewRequestClient::new()),
            Arc::new(app_server::MockAppServerClient::new()),
        );

        (services, project_id)
    }

    /// Selects one settings row by its table index.
    fn select_row(manager: &mut SettingsManager, row_index: usize) {
        manager.table_state.select(Some(row_index));
    }

    fn new_settings_manager() -> SettingsManager {
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        SettingsManager {
            default_fast_model: AgentKind::Gemini.default_model(),
            default_review_model: AgentKind::Gemini.default_model(),
            default_smart_model: AgentKind::Gemini.default_model(),
            open_command: String::new(),
            reasoning_level: ReasoningLevel::High,
            table_state,
            editing_text_row: None,
            open_command_input: None,
            project_id: 1,
            use_last_used_model_as_default: false,
        }
    }

    #[test]
    fn setting_name_as_str_returns_default_fast_model() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultFastModel.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultFastModel");
    }

    #[test]
    fn setting_name_as_str_returns_reasoning_level() {
        // Arrange

        // Act
        let setting_name = SettingName::ReasoningLevel.as_str();

        // Assert
        assert_eq!(setting_name, "ReasoningLevel");
    }

    #[test]
    fn setting_name_as_str_returns_default_smart_model() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultSmartModel.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultSmartModel");
    }

    #[test]
    fn setting_name_as_str_returns_default_review_model() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultReviewModel.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultReviewModel");
    }

    #[test]
    fn setting_name_as_str_returns_open_command() {
        // Arrange

        // Act
        let setting_name = SettingName::OpenCommand.as_str();

        // Assert
        assert_eq!(setting_name, "OpenCommand");
    }

    #[test]
    fn setting_name_as_str_returns_last_used_model_as_default() {
        // Arrange

        // Act
        let setting_name = SettingName::LastUsedModelAsDefault.as_str();

        // Assert
        assert_eq!(setting_name, "LastUsedModelAsDefault");
    }

    #[tokio::test]
    async fn load_default_smart_model_setting_prefers_project_override() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel.as_str(),
                AgentModel::Gpt53Codex.as_str(),
            )
            .await
            .expect("failed to persist smart model");
        services
            .db()
            .upsert_setting("DefaultModel", AgentModel::ClaudeOpus46.as_str())
            .await
            .expect("failed to persist legacy model");

        // Act
        let loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentModel::ClaudeHaiku4520251001,
        )
        .await;

        // Assert
        assert_eq!(loaded_model, AgentModel::Gpt53Codex);
    }

    #[tokio::test]
    async fn load_default_smart_model_setting_falls_back_to_legacy_and_default() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel.as_str(),
                "not-a-valid-model",
            )
            .await
            .expect("failed to persist invalid smart model");
        services
            .db()
            .upsert_setting("DefaultModel", AgentModel::ClaudeOpus46.as_str())
            .await
            .expect("failed to persist legacy model");

        // Act
        let legacy_loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentModel::ClaudeHaiku4520251001,
        )
        .await;

        // Assert
        assert_eq!(legacy_loaded_model, AgentModel::ClaudeOpus46);

        // Arrange
        services
            .db()
            .upsert_setting("DefaultModel", "still-not-a-model")
            .await
            .expect("failed to overwrite legacy model");

        // Act
        let fallback_loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentModel::ClaudeHaiku4520251001,
        )
        .await;

        // Assert
        assert_eq!(fallback_loaded_model, AgentModel::ClaudeHaiku4520251001);
    }

    #[tokio::test]
    async fn load_default_fast_model_setting_falls_back_through_smart_model_resolution() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_setting("DefaultModel", AgentModel::ClaudeOpus46.as_str())
            .await
            .expect("failed to persist legacy smart model");

        // Act
        let fallback_fast_model = load_default_fast_model_setting(
            &services,
            Some(project_id),
            AgentModel::Gpt53CodexSpark,
        )
        .await;

        // Assert
        assert_eq!(fallback_fast_model, AgentModel::ClaudeOpus46);

        // Arrange
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultFastModel.as_str(),
                AgentModel::Gpt53Codex.as_str(),
            )
            .await
            .expect("failed to persist fast model");

        // Act
        let explicit_fast_model = load_default_fast_model_setting(
            &services,
            Some(project_id),
            AgentModel::Gpt53CodexSpark,
        )
        .await;

        // Assert
        assert_eq!(explicit_fast_model, AgentModel::Gpt53Codex);
    }

    #[tokio::test]
    async fn settings_manager_new_loads_project_scoped_values() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_setting("DefaultModel", AgentModel::ClaudeHaiku4520251001.as_str())
            .await
            .expect("failed to persist legacy smart model");
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel.as_str(),
                AgentModel::Gpt53Codex.as_str(),
            )
            .await
            .expect("failed to persist project smart model");
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultFastModel.as_str(),
                AgentModel::Gpt53CodexSpark.as_str(),
            )
            .await
            .expect("failed to persist project fast model");
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultReviewModel.as_str(),
                AgentModel::ClaudeOpus46.as_str(),
            )
            .await
            .expect("failed to persist review model");
        services
            .db()
            .upsert_project_setting(project_id, SettingName::OpenCommand.as_str(), "nvim .")
            .await
            .expect("failed to persist open command");
        services
            .db()
            .set_project_reasoning_level(project_id, ReasoningLevel::Low)
            .await
            .expect("failed to persist reasoning level");
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::LastUsedModelAsDefault.as_str(),
                "true",
            )
            .await
            .expect("failed to persist last-used-model flag");

        // Act
        let manager = SettingsManager::new(&services, project_id).await;

        // Assert
        assert_eq!(manager.default_smart_model, AgentModel::Gpt53Codex);
        assert_eq!(manager.default_fast_model, AgentModel::Gpt53CodexSpark);
        assert_eq!(manager.default_review_model, AgentModel::ClaudeOpus46);
        assert_eq!(manager.open_command, "nvim .");
        assert_eq!(manager.reasoning_level, ReasoningLevel::Low);
        assert!(manager.use_last_used_model_as_default);
    }

    #[tokio::test]
    async fn settings_manager_new_defaults_invalid_last_used_model_flag_to_false() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::LastUsedModelAsDefault.as_str(),
                "invalid-bool",
            )
            .await
            .expect("failed to persist invalid flag");

        // Act
        let manager = SettingsManager::new(&services, project_id).await;

        // Assert
        assert!(!manager.use_last_used_model_as_default);
    }

    #[test]
    fn next_moves_selection_to_default_smart_model_row() {
        // Arrange
        let mut manager = new_settings_manager();

        // Act
        manager.next();

        // Assert
        assert_eq!(manager.table_state.selected(), Some(1));
    }

    #[test]
    fn previous_wraps_to_open_command_row_from_reasoning_level_row() {
        // Arrange
        let mut manager = new_settings_manager();

        // Act
        manager.previous();

        // Assert
        assert_eq!(manager.table_state.selected(), Some(4));
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
    fn settings_rows_include_reasoning_smart_fast_review_model_and_open_commands() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].0, "Reasoning Level");
        assert_eq!(rows[1].0, "Default Smart Model");
        assert_eq!(rows[2].0, "Default Fast Model");
        assert_eq!(rows[3].0, "Default Review Model");
        assert_eq!(rows[4].0, "Open Commands");
    }

    #[test]
    fn footer_hint_returns_editing_text_when_text_input_is_active() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.editing_text_row = Some(SettingRow::OpenCommand);

        // Act
        let footer_hint = manager.footer_hint();

        // Assert
        assert_eq!(
            footer_hint,
            "Editing open commands: one command per line, Alt+Enter/Shift+Enter inserts newline, \
             Enter/Esc finish"
        );
    }

    #[test]
    fn open_commands_returns_single_trimmed_command() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.open_command = "  cargo test  ".to_string();

        // Act
        let open_commands = manager.open_commands();

        // Assert
        assert_eq!(open_commands, vec!["cargo test".to_string()]);
    }

    #[test]
    fn open_commands_splits_newline_entries() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.open_command = " cargo test \n npm run dev \n".to_string();

        // Act
        let open_commands = manager.open_commands();

        // Assert
        assert_eq!(
            open_commands,
            vec!["cargo test".to_string(), "npm run dev".to_string()]
        );
    }

    #[test]
    fn open_commands_does_not_split_double_pipe_entries() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.open_command = "cargo test || npm run dev".to_string();

        // Act
        let open_commands = manager.open_commands();

        // Assert
        assert_eq!(open_commands, vec!["cargo test || npm run dev".to_string()]);
    }

    #[test]
    fn settings_rows_show_empty_placeholder_for_open_command() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[4].1, "<empty>");
    }

    #[test]
    fn settings_rows_show_cursor_for_open_command_while_editing() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.open_command = "http://localhost:5173".to_string();
        manager.editing_text_row = Some(SettingRow::OpenCommand);

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[4].1, "http://localhost:5173|");
    }

    #[test]
    fn settings_rows_show_cursor_within_open_command_while_editing() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.open_command = "abc".to_string();
        manager.editing_text_row = Some(SettingRow::OpenCommand);
        manager.open_command_input = Some(InputState::with_text(manager.open_command.clone()));
        manager.move_selected_text_cursor_left();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[4].1, "ab|c");
    }

    #[test]
    fn settings_rows_show_last_used_model_as_default_value_when_enabled() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.use_last_used_model_as_default = true;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[1].1, "Last used model as default");
    }

    #[test]
    fn settings_rows_show_default_fast_model_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.default_fast_model = AgentModel::Gpt53Codex;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[2].1, AgentModel::Gpt53Codex.as_str());
    }

    #[test]
    fn settings_rows_show_default_review_model_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.default_review_model = AgentModel::ClaudeOpus46;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[3].1, AgentModel::ClaudeOpus46.as_str());
    }

    #[test]
    fn settings_rows_show_reasoning_level_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.reasoning_level = ReasoningLevel::XHigh;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[0].1, "xhigh");
    }

    #[tokio::test]
    async fn handle_enter_toggles_open_command_editing_state() {
        // Arrange
        let (services, _) = test_services().await;
        let mut manager = new_settings_manager();
        manager.open_command = "nvim .".to_string();
        select_row(&mut manager, 4);

        // Act
        manager.handle_enter(&services).await;

        // Assert
        assert!(manager.is_editing_open_commands());
        assert!(manager.open_command_input.is_some());

        // Act
        manager.handle_enter(&services).await;

        // Assert
        assert!(!manager.is_editing_open_commands());
        assert!(manager.open_command_input.is_none());
    }

    #[tokio::test]
    async fn next_and_previous_do_not_move_selection_while_editing_open_commands() {
        // Arrange
        let (services, _) = test_services().await;
        let mut manager = new_settings_manager();
        select_row(&mut manager, 4);
        manager.handle_enter(&services).await;

        // Act
        manager.next();
        manager.previous();

        // Assert
        assert_eq!(manager.table_state.selected(), Some(4));
    }

    #[tokio::test]
    async fn stop_text_input_editing_syncs_cached_open_command_text() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.open_command = "old command".to_string();
        manager.editing_text_row = Some(SettingRow::OpenCommand);
        manager.open_command_input = Some(InputState::with_text("new command".to_string()));

        // Act
        manager.stop_text_input_editing();

        // Assert
        assert_eq!(manager.open_command, "new command");
        assert!(manager.editing_text_row.is_none());
        assert!(manager.open_command_input.is_none());
    }

    #[tokio::test]
    async fn append_and_remove_selected_text_character_persist_open_command() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = SettingsManager::new(&services, project_id).await;
        select_row(&mut manager, 4);
        manager.handle_enter(&services).await;

        // Act
        manager.append_selected_text_character(&services, 'n').await;
        manager.append_selected_text_character(&services, 'v').await;
        manager.remove_selected_text_character(&services).await;

        // Assert
        assert_eq!(manager.open_command, "n");
        assert_eq!(
            services
                .db()
                .get_project_setting(project_id, SettingName::OpenCommand.as_str())
                .await
                .expect("failed to load open command"),
            Some("n".to_string())
        );
    }

    #[tokio::test]
    async fn text_editing_apis_are_noops_without_active_text_row() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = SettingsManager::new(&services, project_id).await;

        // Act
        manager.append_selected_text_character(&services, 'n').await;
        manager.remove_selected_text_character(&services).await;
        manager.move_selected_text_cursor_left();
        manager.move_selected_text_cursor_right();
        manager.move_selected_text_cursor_up();
        manager.move_selected_text_cursor_down();

        // Assert
        assert!(manager.open_command.is_empty());
        assert_eq!(
            services
                .db()
                .get_project_setting(project_id, SettingName::OpenCommand.as_str())
                .await
                .expect("failed to load open command"),
            None
        );
    }

    #[tokio::test]
    async fn cycling_default_smart_model_persists_last_used_flag_and_wraps_back() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = SettingsManager::new(&services, project_id).await;
        let models = all_models();
        manager.default_smart_model = *models.last().expect("models should not be empty");
        manager.use_last_used_model_as_default = false;
        select_row(&mut manager, 1);

        // Act
        manager.handle_enter(&services).await;

        // Assert
        assert!(manager.use_last_used_model_as_default);
        assert_eq!(
            services
                .db()
                .get_project_setting(project_id, SettingName::LastUsedModelAsDefault.as_str())
                .await
                .expect("failed to load last-used flag"),
            Some("true".to_string())
        );

        // Act
        manager.handle_enter(&services).await;

        // Assert
        assert!(!manager.use_last_used_model_as_default);
        assert_eq!(manager.default_smart_model, models[0]);
        assert_eq!(
            services
                .db()
                .get_project_setting(project_id, SettingName::DefaultSmartModel.as_str())
                .await
                .expect("failed to load smart model"),
            Some(models[0].as_str().to_string())
        );
        assert_eq!(
            services
                .db()
                .get_project_setting(project_id, SettingName::LastUsedModelAsDefault.as_str())
                .await
                .expect("failed to load last-used flag"),
            Some("false".to_string())
        );
    }
}
