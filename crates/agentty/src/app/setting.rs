use ratatui::widgets::TableState;

use crate::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::app::AppServices;
use crate::domain::input::InputState;
use crate::domain::setting::SettingName;

/// Loads the persisted smart-model default used for new sessions.
///
/// This prefers the project-scoped `DefaultSmartModel` key and otherwise falls
/// back to `fallback_model`.
pub(crate) async fn load_default_smart_model_setting(
    services: &AppServices,
    project_id: Option<i64>,
    fallback_model: AgentModel,
) -> AgentModel {
    let available_agent_kinds = services.available_agent_kinds();
    let fallback_model =
        resolve_available_model(fallback_model, &available_agent_kinds, fallback_model);

    if let Some(model) =
        load_model_setting(services, project_id, SettingName::DefaultSmartModel).await
    {
        return resolve_available_model(model, &available_agent_kinds, fallback_model);
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
    let available_agent_kinds = services.available_agent_kinds();

    if let Some(model) =
        load_model_setting(services, project_id, SettingName::DefaultFastModel).await
    {
        return resolve_available_model(model, &available_agent_kinds, fallback_model);
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
    IncludeCoauthoredByAgentty,
    OpenCommand,
}

impl SettingRow {
    const ALL: [Self; 6] = [
        Self::ReasoningLevel,
        Self::DefaultSmartModel,
        Self::DefaultFastModel,
        Self::DefaultReviewModel,
        Self::IncludeCoauthoredByAgentty,
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
            Self::ReasoningLevel => "Default Reasoning Level",
            Self::DefaultSmartModel => "Default Smart Model",
            Self::DefaultFastModel => "Default Fast Model",
            Self::DefaultReviewModel => "Default Review Model",
            Self::IncludeCoauthoredByAgentty => "Coauthored by Agentty",
            Self::OpenCommand => "Open Commands",
        }
    }

    /// Returns how this row is edited.
    fn control(self) -> SettingControl {
        match self {
            Self::ReasoningLevel
            | Self::DefaultSmartModel
            | Self::DefaultFastModel
            | Self::DefaultReviewModel
            | Self::IncludeCoauthoredByAgentty => SettingControl::Selector,
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
            Self::IncludeCoauthoredByAgentty => SettingName::IncludeCoauthoredByAgentty,
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
    /// Default reasoning effort preference for models that support this
    /// setting.
    ///
    /// Currently applied to Codex and Claude turns.
    pub reasoning_level: ReasoningLevel,
    /// Table selection state for the settings page.
    pub table_state: TableState,
    available_agent_kinds: Vec<AgentKind>,
    editing_text_row: Option<SettingRow>,
    /// Whether generated session commit messages append the Agentty coauthor
    /// trailer for the active project.
    ///
    /// New projects start with this disabled until the user explicitly enables
    /// it.
    include_coauthored_by_agentty: bool,
    open_command_input: Option<InputState>,
    /// Active project identifier that owns these persisted settings.
    project_id: i64,
    use_last_used_model_as_default: bool,
}

impl SettingsManager {
    /// Creates a settings manager and loads persisted values from the database.
    pub async fn new(services: &AppServices, project_id: i64) -> Self {
        let available_agent_kinds = services.available_agent_kinds();
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
                .map_or(default_smart_model, |model| {
                    resolve_available_model(model, &available_agent_kinds, default_smart_model)
                });
        let reasoning_level = load_reasoning_level_setting(services, Some(project_id)).await;

        let open_command = services
            .db()
            .get_project_setting(project_id, SettingName::OpenCommand)
            .await
            .unwrap_or(None)
            .unwrap_or_default();

        let include_coauthored_by_agentty = load_project_bool_setting(
            services,
            Some(project_id),
            SettingName::IncludeCoauthoredByAgentty,
            false,
        )
        .await;
        let use_last_used_model_as_default = load_project_bool_setting(
            services,
            Some(project_id),
            SettingName::LastUsedModelAsDefault,
            false,
        )
        .await;

        let mut table_state = TableState::default();
        table_state.select(Some(0));

        Self {
            default_fast_model,
            default_review_model,
            default_smart_model,
            open_command,
            reasoning_level,
            table_state,
            available_agent_kinds,
            editing_text_row: None,
            include_coauthored_by_agentty,
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
            SettingRow::IncludeCoauthoredByAgentty => {
                bool_setting_display(self.include_coauthored_by_agentty)
            }
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
            SettingName::IncludeCoauthoredByAgentty => {
                self.toggle_include_coauthored_by_agentty_selector(services)
                    .await;
            }
            SettingName::ActiveProjectId
            | SettingName::OpenCommand
            | SettingName::LastUsedModelAsDefault => {}
        }
    }

    /// Persists the current value for a text-input row.
    async fn persist_text_setting(&self, services: &AppServices, row: SettingRow) {
        if !matches!(row.control(), SettingControl::TextInput) {
            return;
        }

        match row.setting_name() {
            SettingName::OpenCommand => {
                // Best-effort: settings persistence failure is non-critical.
                let _ = services
                    .db()
                    .upsert_project_setting(
                        self.project_id,
                        SettingName::OpenCommand,
                        &self.open_command,
                    )
                    .await;
            }
            SettingName::ReasoningLevel
            | SettingName::ActiveProjectId
            | SettingName::DefaultFastModel
            | SettingName::DefaultReviewModel
            | SettingName::DefaultSmartModel
            | SettingName::IncludeCoauthoredByAgentty
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
        let all_models = self.selectable_models();
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
        let Some(next_model) = next_model(self.default_fast_model, &self.available_agent_kinds)
        else {
            return;
        };
        self.default_fast_model = next_model;

        self.persist_default_fast_model_setting(services).await;
    }

    /// Cycles the review-model selector through all explicit models.
    async fn cycle_default_review_model_selector(&mut self, services: &AppServices) {
        let Some(next_model) = next_model(self.default_review_model, &self.available_agent_kinds)
        else {
            return;
        };
        self.default_review_model = next_model;

        self.persist_default_review_model_setting(services).await;
    }

    /// Returns all selectable models whose provider is locally runnable.
    fn selectable_models(&self) -> Vec<AgentModel> {
        selectable_models(&self.available_agent_kinds)
    }

    /// Toggles whether generated session commit messages include the
    /// `Co-Authored-By` trailer for the active project.
    async fn toggle_include_coauthored_by_agentty_selector(&mut self, services: &AppServices) {
        self.include_coauthored_by_agentty = !self.include_coauthored_by_agentty;

        self.persist_include_coauthored_by_agentty_setting(services)
            .await;
    }

    /// Persists smart-model selector values (`DefaultSmartModel` and
    /// `LastUsedModelAsDefault`).
    async fn persist_default_smart_model_settings(&self, services: &AppServices) {
        let last_used_model_as_default_value = self.use_last_used_model_as_default.to_string();

        // Best-effort: settings persistence failure is non-critical.
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::DefaultSmartModel,
                self.default_smart_model.as_str(),
            )
            .await;
        // Best-effort: settings persistence failure is non-critical.
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::LastUsedModelAsDefault,
                &last_used_model_as_default_value,
            )
            .await;
    }

    /// Persists the reasoning-level selector value (`ReasoningLevel`).
    async fn persist_reasoning_level_setting(&self, services: &AppServices) {
        // Best-effort: settings persistence failure is non-critical.
        let _ = services
            .db()
            .set_project_reasoning_level(self.project_id, self.reasoning_level)
            .await;
    }

    /// Persists the fast-model selector value (`DefaultFastModel`).
    async fn persist_default_fast_model_setting(&self, services: &AppServices) {
        // Best-effort: settings persistence failure is non-critical.
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::DefaultFastModel,
                self.default_fast_model.as_str(),
            )
            .await;
    }

    /// Persists the review-model selector value (`DefaultReviewModel`).
    async fn persist_default_review_model_setting(&self, services: &AppServices) {
        // Best-effort: settings persistence failure is non-critical.
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::DefaultReviewModel,
                self.default_review_model.as_str(),
            )
            .await;
    }

    /// Persists the coauthor-trailer toggle for generated session commit
    /// messages.
    async fn persist_include_coauthored_by_agentty_setting(&self, services: &AppServices) {
        let include_coauthored_by_agentty = self.include_coauthored_by_agentty.to_string();

        // Best-effort: settings persistence failure is non-critical.
        let _ = services
            .db()
            .upsert_project_setting(
                self.project_id,
                SettingName::IncludeCoauthoredByAgentty,
                &include_coauthored_by_agentty,
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

/// Loads one project-scoped boolean setting, falling back to
/// `default_value` when the project is missing or the persisted value is
/// absent or invalid.
async fn load_project_bool_setting(
    services: &AppServices,
    project_id: Option<i64>,
    setting_name: SettingName,
    default_value: bool,
) -> bool {
    let Some(project_id) = project_id else {
        return default_value;
    };

    services
        .db()
        .get_project_setting(project_id, setting_name)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| setting_value.parse::<bool>().ok())
        .unwrap_or(default_value)
}

/// Returns the human-readable value shown for one boolean selector row.
fn bool_setting_display(setting_value: bool) -> String {
    if setting_value {
        "Enabled".to_string()
    } else {
        "Disabled".to_string()
    }
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

/// Returns all selectable models in settings display order for the locally
/// available providers.
fn selectable_models(available_agent_kinds: &[AgentKind]) -> Vec<AgentModel> {
    crate::agent::selectable_models_for_agent_kinds(available_agent_kinds)
}

/// Returns the next model from the explicit selectable model list.
fn next_model(
    current_model: AgentModel,
    available_agent_kinds: &[AgentKind],
) -> Option<AgentModel> {
    let models = selectable_models(available_agent_kinds);
    if models.is_empty() {
        return None;
    }

    let current_index = models
        .iter()
        .position(|model| *model == current_model)
        .unwrap_or(0);
    let next_index = (current_index + 1) % models.len();

    Some(models[next_index])
}

/// Resolves one stored model against the currently available agent kinds.
fn resolve_available_model(
    model: AgentModel,
    available_agent_kinds: &[AgentKind],
    fallback_model: AgentModel,
) -> AgentModel {
    crate::agent::resolve_model_for_available_agent_kinds(
        model,
        available_agent_kinds,
        fallback_model,
    )
}

/// Loads a model setting and parses it into an [`AgentModel`].
///
/// Retired persisted model ids are upgraded to their current replacement
/// models before the value is returned.
async fn load_model_setting(
    services: &AppServices,
    project_id: Option<i64>,
    setting_name: SettingName,
) -> Option<AgentModel> {
    let project_id = project_id?;

    services
        .db()
        .get_project_setting(project_id, setting_name)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| AgentModel::parse_persisted(&setting_value).ok())
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
            Arc::new(crate::app::session::RealClock),
            database,
            event_tx,
            crate::app::service::AppServiceDeps {
                app_server_client_override: Some(Arc::new(app_server::MockAppServerClient::new())),
                available_agent_kinds: AgentKind::ALL.to_vec(),
                fs_client: Arc::new(fs::MockFsClient::new()),
                git_client: Arc::new(git::MockGitClient::new()),
                review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            },
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
            available_agent_kinds: AgentKind::ALL.to_vec(),
            editing_text_row: None,
            include_coauthored_by_agentty: false,
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
    fn setting_name_as_str_returns_include_coauthored_by_agentty() {
        // Arrange

        // Act
        let setting_name = SettingName::IncludeCoauthoredByAgentty.as_str();

        // Assert
        assert_eq!(setting_name, "IncludeCoauthoredByAgentty");
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
                SettingName::DefaultSmartModel,
                AgentModel::Gpt54.as_str(),
            )
            .await
            .expect("failed to persist smart model");

        // Act
        let loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentModel::ClaudeHaiku4520251001,
        )
        .await;

        // Assert
        assert_eq!(loaded_model, AgentModel::Gpt54);
    }

    #[tokio::test]
    async fn load_default_smart_model_setting_falls_back_to_default() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                "not-a-valid-model",
            )
            .await
            .expect("failed to persist invalid smart model");

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
    async fn load_default_fast_model_setting_migrates_retired_claude_opus_46_setting() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                "claude-opus-4-6",
            )
            .await
            .expect("failed to persist smart model");

        // Act
        let fallback_fast_model = load_default_fast_model_setting(
            &services,
            Some(project_id),
            AgentModel::Gpt53CodexSpark,
        )
        .await;

        // Assert
        assert_eq!(fallback_fast_model, AgentModel::ClaudeOpus47);

        // Arrange
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultFastModel,
                AgentModel::Gpt54.as_str(),
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
        assert_eq!(explicit_fast_model, AgentModel::Gpt54);
    }

    #[tokio::test]
    async fn settings_manager_new_loads_project_scoped_values() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                AgentModel::Gpt54.as_str(),
            )
            .await
            .expect("failed to persist project smart model");
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultFastModel,
                AgentModel::Gpt53CodexSpark.as_str(),
            )
            .await
            .expect("failed to persist project fast model");
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultReviewModel,
                "claude-opus-4-6",
            )
            .await
            .expect("failed to persist review model");
        services
            .db()
            .upsert_project_setting(project_id, SettingName::IncludeCoauthoredByAgentty, "false")
            .await
            .expect("failed to persist coauthor setting");
        services
            .db()
            .upsert_project_setting(project_id, SettingName::OpenCommand, "nvim .")
            .await
            .expect("failed to persist open command");
        services
            .db()
            .set_project_reasoning_level(project_id, ReasoningLevel::Low)
            .await
            .expect("failed to persist reasoning level");
        services
            .db()
            .upsert_project_setting(project_id, SettingName::LastUsedModelAsDefault, "true")
            .await
            .expect("failed to persist last-used-model flag");

        // Act
        let manager = SettingsManager::new(&services, project_id).await;

        // Assert
        assert_eq!(manager.default_smart_model, AgentModel::Gpt54);
        assert_eq!(manager.default_fast_model, AgentModel::Gpt53CodexSpark);
        assert_eq!(manager.default_review_model, AgentModel::ClaudeOpus47);
        assert_eq!(manager.open_command, "nvim .");
        assert_eq!(manager.reasoning_level, ReasoningLevel::Low);
        assert!(!manager.include_coauthored_by_agentty);
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
                SettingName::LastUsedModelAsDefault,
                "invalid-bool",
            )
            .await
            .expect("failed to persist invalid flag");

        // Act
        let manager = SettingsManager::new(&services, project_id).await;

        // Assert
        assert!(!manager.use_last_used_model_as_default);
    }

    #[tokio::test]
    async fn settings_manager_new_defaults_invalid_coauthor_flag_to_false() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::IncludeCoauthoredByAgentty,
                "invalid-bool",
            )
            .await
            .expect("failed to persist invalid coauthor flag");

        // Act
        let manager = SettingsManager::new(&services, project_id).await;

        // Assert
        assert!(!manager.include_coauthored_by_agentty);
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
        assert_eq!(manager.table_state.selected(), Some(5));
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
    fn settings_rows_include_reasoning_model_coauthor_and_open_command_options() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].0, "Default Reasoning Level");
        assert_eq!(rows[1].0, "Default Smart Model");
        assert_eq!(rows[2].0, "Default Fast Model");
        assert_eq!(rows[3].0, "Default Review Model");
        assert_eq!(rows[4].0, "Coauthored by Agentty");
        assert_eq!(rows[5].0, "Open Commands");
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
        assert_eq!(rows[5].1, "<empty>");
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
        assert_eq!(rows[5].1, "http://localhost:5173|");
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
        assert_eq!(rows[5].1, "ab|c");
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
        manager.default_fast_model = AgentModel::Gpt54;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[2].1, AgentModel::Gpt54.as_str());
    }

    #[test]
    fn settings_rows_show_default_review_model_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.default_review_model = AgentModel::ClaudeOpus47;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[3].1, AgentModel::ClaudeOpus47.as_str());
    }

    #[test]
    fn settings_rows_show_coauthored_by_agentty_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.include_coauthored_by_agentty = false;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[4].1, "Disabled");
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
        select_row(&mut manager, 5);

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
        select_row(&mut manager, 5);
        manager.handle_enter(&services).await;

        // Act
        manager.next();
        manager.previous();

        // Assert
        assert_eq!(manager.table_state.selected(), Some(5));
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
        select_row(&mut manager, 5);
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
                .get_project_setting(project_id, SettingName::OpenCommand)
                .await
                .expect("failed to load open command"),
            Some("n".to_string())
        );
    }

    #[tokio::test]
    async fn handle_enter_toggles_coauthor_setting_and_persists_value() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = SettingsManager::new(&services, project_id).await;
        select_row(&mut manager, 4);

        // Act
        manager.handle_enter(&services).await;

        // Assert
        assert!(manager.include_coauthored_by_agentty);
        assert_eq!(
            services
                .db()
                .get_project_setting(project_id, SettingName::IncludeCoauthoredByAgentty)
                .await
                .expect("failed to load coauthor setting"),
            Some("true".to_string())
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
                .get_project_setting(project_id, SettingName::OpenCommand)
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
        let models = selectable_models(AgentKind::ALL);
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
                .get_project_setting(project_id, SettingName::LastUsedModelAsDefault)
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
                .get_project_setting(project_id, SettingName::DefaultSmartModel)
                .await
                .expect("failed to load smart model"),
            Some(models[0].as_str().to_string())
        );
        assert_eq!(
            services
                .db()
                .get_project_setting(project_id, SettingName::LastUsedModelAsDefault)
                .await
                .expect("failed to load last-used flag"),
            Some("false".to_string())
        );
    }

    #[tokio::test]
    async fn load_default_smart_model_setting_falls_back_to_available_backend() {
        // Arrange
        let (mut services, project_id) = test_services().await;
        services = AppServices::new(
            services.base_path().to_path_buf(),
            services.clock(),
            services.db().clone(),
            services.event_sender(),
            crate::app::service::AppServiceDeps {
                app_server_client_override: services.app_server_client_override(),
                available_agent_kinds: vec![AgentKind::Codex],
                fs_client: services.fs_client(),
                git_client: services.git_client(),
                review_request_client: services.review_request_client(),
            },
        );
        services
            .db()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                AgentModel::Gemini31ProPreview.as_str(),
            )
            .await
            .expect("failed to persist unavailable smart model");

        // Act
        let loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentKind::Gemini.default_model(),
        )
        .await;

        // Assert
        assert_eq!(loaded_model, AgentKind::Codex.default_model());
    }
}
