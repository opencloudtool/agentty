//! Shared prompt-composer state and pure helpers for slash suggestions,
//! inline attachment placeholders, and prompt submission payloads.

use std::path::PathBuf;

use crate::domain::agent::{self, AgentKind, AgentModel, AgentSelectionMetadata};
use crate::domain::input::{InputState, is_at_mention_boundary, is_at_mention_query_character};

/// One selectable row in the prompt slash-command menu.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSuggestionItem {
    /// Optional compact badge rendered before the main label.
    pub badge: Option<String>,
    /// Optional explanatory text rendered after the label.
    pub detail: Option<String>,
    /// Primary row label used for selection and insertion.
    pub label: String,
    /// Optional trailing metadata rendered with subdued styling.
    pub metadata: Option<String>,
}

/// Render-ready prompt suggestion dropdown state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSuggestionList {
    /// Dropdown rows in display order.
    pub items: Vec<PromptSuggestionItem>,
    /// Highlighted row index in `items`.
    pub selected_index: usize,
    /// Dropdown title shown in the border chrome.
    pub title: String,
}

/// Semantic action represented by the currently highlighted prompt slash item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptSuggestionSelection {
    /// Slash command selected from the first stage.
    Command(&'static str),
    /// Agent selected during `/model` agent selection.
    Agent(AgentKind),
    /// Model selected during `/model` model selection.
    Model(AgentModel),
}

/// Inline attachment metadata for one pasted local image placeholder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAttachment {
    /// Stable display number shown inside the inline `[Image #n]` token.
    pub attachment_number: usize,
    /// Local image path that will later be handed off to runtime transport.
    pub local_image_path: PathBuf,
    /// Placeholder token inserted into the prompt composer text.
    pub placeholder: String,
}

impl PromptAttachment {
    /// Creates attachment metadata for one pasted local image.
    #[must_use]
    pub fn new(attachment_number: usize, local_image_path: PathBuf) -> Self {
        Self {
            attachment_number,
            local_image_path,
            placeholder: Self::placeholder_for(attachment_number),
        }
    }

    /// Builds the inline placeholder token for one attachment number.
    #[must_use]
    pub fn placeholder_for(attachment_number: usize) -> String {
        format!("[Image #{attachment_number}]")
    }
}

/// Attachment-only snapshot drained from the prompt composer during submit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptComposerSubmission {
    /// Attachments that still appear in the submitted prompt text.
    pub attachments: Vec<PromptAttachment>,
    /// Submitted prompt text after draining the input buffer.
    pub text: String,
}

impl PromptComposerSubmission {
    /// Returns whether both the text and attachment list are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }
}

/// UI state for pasted local-image attachments in prompt mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAttachmentState {
    /// Attachments in the same order their placeholders were inserted.
    pub attachments: Vec<PromptAttachment>,
    /// Next placeholder number that should be assigned to a pasted image.
    pub next_attachment_number: usize,
}

impl PromptAttachmentState {
    /// Creates empty prompt attachment state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            attachments: Vec::new(),
            next_attachment_number: 1,
        }
    }

    /// Registers a pasted local image and returns the placeholder inserted
    /// into the prompt input text.
    pub fn register_local_image(&mut self, local_image_path: PathBuf) -> String {
        let attachment = PromptAttachment::new(self.next_attachment_number, local_image_path);
        let placeholder = attachment.placeholder.clone();

        self.attachments.push(attachment);
        self.refresh_next_attachment_number();

        placeholder
    }

    /// Returns attachment metadata for the given inline placeholder token.
    #[must_use]
    pub fn attachment_for_placeholder(&self, placeholder: &str) -> Option<&PromptAttachment> {
        self.attachments
            .iter()
            .find(|attachment| attachment.placeholder == placeholder)
    }

    /// Recomputes the next placeholder number by reusing the smallest missing
    /// positive attachment number.
    pub fn refresh_next_attachment_number(&mut self) {
        let mut next_attachment_number = 1;

        while self
            .attachments
            .iter()
            .any(|attachment| attachment.attachment_number == next_attachment_number)
        {
            next_attachment_number += 1;
        }

        self.next_attachment_number = next_attachment_number;
    }

    /// Clears all tracked attachments and resets numbering back to the first
    /// placeholder.
    pub fn reset(&mut self) {
        self.attachments.clear();
        self.next_attachment_number = 1;
    }
}

impl Default for PromptAttachmentState {
    fn default() -> Self {
        Self::new()
    }
}

/// UI state for navigating previously sent prompts with `Up` and `Down`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptHistoryState {
    /// Draft input captured before entering history navigation.
    pub draft_text: Option<String>,
    /// Previously sent user prompts in chronological order.
    pub entries: Vec<String>,
    /// Currently selected history entry index, if any.
    pub selected_index: Option<usize>,
}

impl PromptHistoryState {
    /// Creates history state from prior prompt entries.
    #[must_use]
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            draft_text: None,
            entries,
            selected_index: None,
        }
    }

    /// Clears active history navigation and stored draft text.
    pub fn reset_navigation(&mut self) {
        self.draft_text = None;
        self.selected_index = None;
    }
}

/// Steps in prompt slash command selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptSlashStage {
    /// Selecting the agent for the current slash command.
    Agent,
    /// Selecting the slash command itself.
    Command,
    /// Selecting a model after choosing an agent.
    Model,
}

/// UI state for prompt-only slash command selection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PromptSlashState {
    /// Agent kinds currently runnable on this machine for `/model`.
    pub available_agent_kinds: Vec<AgentKind>,
    /// Agent selected for the current slash workflow, when applicable.
    pub selected_agent: Option<AgentKind>,
    /// Highlighted option inside the active slash menu.
    pub selected_index: usize,
    /// Active slash-command selection stage.
    pub stage: PromptSlashStage,
}

impl PromptSlashState {
    /// Creates a new slash state at command selection.
    #[must_use]
    pub fn new() -> Self {
        Self::with_available_agent_kinds(AgentKind::ALL.to_vec())
    }

    /// Creates a new slash state scoped to the provided locally available
    /// agent kinds.
    #[must_use]
    pub fn with_available_agent_kinds(available_agent_kinds: Vec<AgentKind>) -> Self {
        Self {
            available_agent_kinds,
            selected_agent: None,
            selected_index: 0,
            stage: PromptSlashStage::Command,
        }
    }

    /// Replaces the locally available agent kinds while keeping prompt slash
    /// selection state coherent.
    pub fn replace_available_agent_kinds(&mut self, available_agent_kinds: Vec<AgentKind>) {
        self.available_agent_kinds = available_agent_kinds;

        if self
            .selected_agent
            .is_some_and(|selected_agent| !self.available_agent_kinds.contains(&selected_agent))
        {
            self.selected_agent = None;
            self.selected_index = 0;

            if matches!(self.stage, PromptSlashStage::Model) {
                self.stage = PromptSlashStage::Agent;
            }
        }
    }

    /// Resets slash state back to command selection.
    pub fn reset(&mut self) {
        self.selected_agent = None;
        self.selected_index = 0;
        self.stage = PromptSlashStage::Command;
    }
}

impl Default for PromptSlashState {
    fn default() -> Self {
        Self::new()
    }
}

/// Full prompt composer state for one session prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptComposerState {
    /// Ordered local image attachments referenced by inline placeholders in
    /// `input`.
    pub attachment_state: PromptAttachmentState,
    /// Prompt-history navigation state for `Up` and `Down`.
    pub history_state: PromptHistoryState,
    /// Editable prompt text, including inline attachment placeholders.
    pub input: InputState,
    /// Slash-command selection state for the current prompt input.
    pub slash_state: PromptSlashState,
}

impl PromptComposerState {
    /// Creates a prompt composer with empty input and prompt history.
    #[must_use]
    pub fn new(available_agent_kinds: Vec<AgentKind>) -> Self {
        Self::with_input_and_history(InputState::new(), available_agent_kinds, Vec::new())
    }

    /// Creates a prompt composer with explicit input and history snapshots.
    #[must_use]
    pub fn with_input_and_history(
        input: InputState,
        available_agent_kinds: Vec<AgentKind>,
        history_entries: Vec<String>,
    ) -> Self {
        Self {
            attachment_state: PromptAttachmentState::new(),
            history_state: PromptHistoryState::new(history_entries),
            input,
            slash_state: PromptSlashState::with_available_agent_kinds(available_agent_kinds),
        }
    }

    /// Returns whether the composer currently starts with a slash command.
    #[must_use]
    pub fn is_slash_command(&self) -> bool {
        self.input.text().starts_with('/')
    }

    /// Builds the render-ready prompt slash suggestion list for the current
    /// input and slash stage.
    #[must_use]
    pub fn slash_suggestion_list(
        &self,
        session_agent_kind: AgentKind,
    ) -> Option<PromptSuggestionList> {
        build_prompt_slash_suggestion_list(self.input.text(), &self.slash_state, session_agent_kind)
    }

    /// Resolves the semantic action behind the currently highlighted prompt
    /// slash item.
    #[must_use]
    pub fn selected_slash_action(
        &self,
        session_agent_kind: AgentKind,
    ) -> Option<PromptSuggestionSelection> {
        resolve_prompt_slash_selection(self.input.text(), &self.slash_state, session_agent_kind)
    }

    /// Inserts pasted prompt text by delegating to the canonical field-level
    /// helper and clears any transient slash/history navigation state.
    pub fn insert_text(&mut self, text: &str) {
        insert_prompt_text(
            &mut self.input,
            &mut self.history_state,
            &mut self.slash_state,
            text,
        );
    }

    /// Inserts one typed character by delegating to the canonical field-level
    /// helper and clears transient slash/history state.
    pub fn insert_char(&mut self, character: char) {
        insert_prompt_character(
            &mut self.input,
            &mut self.history_state,
            &mut self.slash_state,
            character,
        );
    }

    /// Registers one pasted image by delegating to the canonical field-level
    /// helper, inserts its placeholder into the prompt, and clears transient
    /// slash/history state.
    pub fn insert_local_image(&mut self, local_image_path: PathBuf) {
        insert_prompt_local_image(
            &mut self.attachment_state,
            &mut self.history_state,
            &mut self.input,
            &mut self.slash_state,
            local_image_path,
        );
    }

    /// Applies a prompt deletion range by delegating to the canonical
    /// field-level helper, expanding it to whole image placeholders and
    /// pruning orphaned attachment metadata.
    pub fn delete_range(&mut self, start: usize, end: usize) {
        apply_prompt_delete_range(
            &mut self.attachment_state,
            &mut self.history_state,
            &mut self.input,
            &mut self.slash_state,
            start,
            end,
        );
    }

    /// Drains the prompt composer by delegating to the canonical field-level
    /// helper, returning text plus attachment metadata suitable for runtime
    /// turn submission.
    pub fn take_submission(&mut self) -> PromptComposerSubmission {
        drain_prompt_submission(&mut self.attachment_state, &mut self.input)
    }
}

impl Default for PromptComposerState {
    fn default() -> Self {
        Self::new(AgentKind::ALL.to_vec())
    }
}

/// Returns the number of selectable options in the active slash stage.
#[must_use]
pub fn prompt_slash_option_count(
    input: &str,
    stage: PromptSlashStage,
    selected_agent: Option<AgentKind>,
    available_agent_kinds: &[AgentKind],
    session_agent_kind: AgentKind,
) -> usize {
    build_prompt_slash_suggestion_list(
        input,
        &PromptSlashState {
            available_agent_kinds: available_agent_kinds.to_vec(),
            selected_agent,
            selected_index: 0,
            stage,
        },
        session_agent_kind,
    )
    .map_or(0, |suggestion_list| suggestion_list.items.len())
}

/// Returns the character range deleted by one current-line delete action.
#[must_use]
pub fn current_line_delete_range(input: &InputState) -> Option<(usize, usize)> {
    let characters: Vec<char> = input.text().chars().collect();
    if characters.is_empty() {
        return None;
    }

    let cursor = input.cursor.min(characters.len());
    let mut line_start = cursor;
    while line_start > 0 && characters[line_start - 1] != '\n' {
        line_start -= 1;
    }

    let mut line_end = cursor;
    while line_end < characters.len() && characters[line_end] != '\n' {
        line_end += 1;
    }

    let delete_range = if line_start > 0 {
        (line_start - 1, line_end)
    } else if line_end < characters.len() {
        (line_start, line_end + 1)
    } else {
        (line_start, line_end)
    };

    if delete_range.0 == delete_range.1 {
        return None;
    }

    Some(delete_range)
}

/// Expands one deletion range to cover any overlapping `[Image #n]`
/// placeholders so partial token edits remove the whole placeholder.
#[must_use]
pub fn expand_delete_range_to_image_tokens(text: &str, start: usize, end: usize) -> (usize, usize) {
    let mut expanded_start = start;
    let mut expanded_end = end;

    for (token_start, token_end, _) in image_token_ranges(text) {
        if token_start < expanded_end && expanded_start < token_end {
            expanded_start = expanded_start.min(token_start);
            expanded_end = expanded_end.max(token_end);
        }
    }

    (expanded_start, expanded_end)
}

/// Returns all valid `[Image #n]` placeholder token ranges in `text`.
#[must_use]
pub fn image_token_ranges(text: &str) -> Vec<(usize, usize, String)> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut index = 0;

    while index < characters.len() {
        if let Some(end_index) = image_token_end_index(&characters, index) {
            let placeholder = characters[index..end_index].iter().collect::<String>();
            ranges.push((index, end_index, placeholder));
            index = end_index;

            continue;
        }

        index += 1;
    }

    ranges
}

/// Inserts pasted prompt text and clears transient slash/history state.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::insert_text`] delegates here to keep behavior
/// centralized.
pub fn insert_prompt_text(
    input: &mut InputState,
    history_state: &mut PromptHistoryState,
    slash_state: &mut PromptSlashState,
    text: &str,
) {
    input.insert_text(text);
    history_state.reset_navigation();
    slash_state.reset();
}

/// Inserts one typed character and clears transient slash/history state.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::insert_char`] delegates here to keep behavior
/// centralized.
pub fn insert_prompt_character(
    input: &mut InputState,
    history_state: &mut PromptHistoryState,
    slash_state: &mut PromptSlashState,
    character: char,
) {
    input.insert_char(character);
    history_state.reset_navigation();
    slash_state.reset();
}

/// Inserts one pasted image placeholder and records the attachment metadata.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::insert_local_image`] delegates here to keep
/// behavior centralized.
pub fn insert_prompt_local_image(
    attachment_state: &mut PromptAttachmentState,
    history_state: &mut PromptHistoryState,
    input: &mut InputState,
    slash_state: &mut PromptSlashState,
    local_image_path: PathBuf,
) {
    let placeholder = attachment_state.register_local_image(local_image_path);
    input.insert_text(&placeholder);
    history_state.reset_navigation();
    slash_state.reset();
}

/// Applies one prompt deletion range, expanding it to whole image placeholders
/// and pruning orphaned attachment metadata.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::delete_range`] delegates here to keep behavior
/// centralized.
pub fn apply_prompt_delete_range(
    attachment_state: &mut PromptAttachmentState,
    history_state: &mut PromptHistoryState,
    input: &mut InputState,
    slash_state: &mut PromptSlashState,
    start: usize,
    end: usize,
) {
    let (delete_start, delete_end) = expand_delete_range_to_image_tokens(input.text(), start, end);
    if delete_start >= delete_end {
        return;
    }

    input.replace_range(delete_start, delete_end, "");
    attachment_state
        .attachments
        .retain(|attachment| input.text().contains(&attachment.placeholder));
    attachment_state.refresh_next_attachment_number();
    history_state.reset_navigation();
    slash_state.reset();
}

/// Drains the prompt composer into text plus attachment metadata suitable for
/// runtime turn submission.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::take_submission`] delegates here to keep behavior
/// centralized.
pub fn drain_prompt_submission(
    attachment_state: &mut PromptAttachmentState,
    input: &mut InputState,
) -> PromptComposerSubmission {
    let text = input.take_text();
    let mut attachments = attachment_state
        .attachments
        .iter()
        .filter(|attachment| text.contains(&attachment.placeholder))
        .cloned()
        .collect::<Vec<_>>();
    attachments.sort_by_key(|attachment| text.find(&attachment.placeholder).unwrap_or(usize::MAX));
    attachment_state.reset();

    PromptComposerSubmission { attachments, text }
}

/// Rewrites user-entered `@` lookups into quoted agent-facing path tokens.
///
/// File and directory lookups like `@path/to/file` are rewritten to
/// `"looked/up/path/to/file"`. Non-lookup uses of `@`, including email-like
/// tokens and lone `@`, are preserved so the UI and persisted transcript can
/// continue storing the original user input unchanged.
#[must_use]
pub fn render_prompt_text_for_agent(text: &str) -> String {
    let characters = text.chars().collect::<Vec<char>>();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;

    while let Some(&character) = characters.get(index) {
        if character != '@'
            || !is_at_mention_boundary(characters.get(index.wrapping_sub(1)).copied())
            || index + 1 >= characters.len()
        {
            output.push(character);
            index += 1;

            continue;
        }

        let mut scan_index = index + 1;
        while scan_index < characters.len() && is_at_mention_query_character(characters[scan_index])
        {
            scan_index += 1;
        }

        if scan_index == index + 1 {
            output.push(character);
            index += 1;

            continue;
        }

        output.push('"');
        output.push_str("looked/up/");
        output.extend(characters[index + 1..scan_index].iter());
        output.push('"');
        index = scan_index;
    }

    output
}

/// Builds the render-ready prompt slash suggestion list for the provided
/// input and slash state.
#[must_use]
pub fn build_prompt_slash_suggestion_list(
    input: &str,
    slash_state: &PromptSlashState,
    session_agent_kind: AgentKind,
) -> Option<PromptSuggestionList> {
    build_slash_suggestion_list(
        input,
        &slash_state.available_agent_kinds,
        slash_state.stage,
        slash_state.selected_agent,
        session_agent_kind,
        slash_state.selected_index,
    )
}

/// Resolves the semantic prompt slash action behind the current selection.
///
/// The resolved item is clamped to the same visible selection range used by
/// [`build_prompt_slash_suggestion_list`] so submit behavior stays aligned with
/// the currently rendered highlight even when `selected_index` is stale.
#[must_use]
pub fn resolve_prompt_slash_selection(
    input: &str,
    slash_state: &PromptSlashState,
    session_agent_kind: AgentKind,
) -> Option<PromptSuggestionSelection> {
    selected_slash_action(
        input,
        &slash_state.available_agent_kinds,
        slash_state.stage,
        slash_state.selected_agent,
        slash_state.selected_index,
        session_agent_kind,
    )
}

/// Builds one prompt slash suggestion list for the provided input state.
fn build_slash_suggestion_list(
    input: &str,
    available_agent_kinds: &[AgentKind],
    stage: PromptSlashStage,
    selected_agent: Option<AgentKind>,
    session_agent_kind: AgentKind,
    selected_index: usize,
) -> Option<PromptSuggestionList> {
    if !input.starts_with('/') {
        return None;
    }

    let (title, items): (&str, Vec<PromptSuggestionItem>) = match stage {
        PromptSlashStage::Command => {
            let commands = prompt_slash_commands(input)
                .into_iter()
                .map(|command| PromptSuggestionItem {
                    badge: None,
                    detail: Some(command_description(command).to_string()),
                    label: command.to_string(),
                    metadata: None,
                })
                .collect::<Vec<_>>();

            ("Slash Command (j/k move, Enter select)", commands)
        }
        PromptSlashStage::Agent => (
            "/model Agent (j/k move, Enter select)",
            available_agent_kinds
                .iter()
                .map(|agent_kind| PromptSuggestionItem {
                    badge: None,
                    detail: Some(agent_kind.description().to_string()),
                    label: agent_kind.name().to_string(),
                    metadata: None,
                })
                .collect(),
        ),
        PromptSlashStage::Model => {
            let selected_agent_kind = resolve_model_stage_agent(
                session_agent_kind,
                available_agent_kinds,
                selected_agent,
            )?;
            let models = selected_agent_kind
                .models()
                .iter()
                .map(|model| PromptSuggestionItem {
                    badge: None,
                    detail: Some(model.description().to_string()),
                    label: model.name().to_string(),
                    metadata: None,
                })
                .collect::<Vec<_>>();

            ("/model Model (j/k move, Enter select)", models)
        }
    };

    if items.is_empty() {
        return None;
    }

    let max_index = items.len().saturating_sub(1);

    Some(PromptSuggestionList {
        items,
        selected_index: selected_index.min(max_index),
        title: title.to_string(),
    })
}

/// Returns the semantic slash action mapped to the current selection state.
fn selected_slash_action(
    input: &str,
    available_agent_kinds: &[AgentKind],
    stage: PromptSlashStage,
    selected_agent: Option<AgentKind>,
    selected_index: usize,
    session_agent_kind: AgentKind,
) -> Option<PromptSuggestionSelection> {
    match stage {
        PromptSlashStage::Command => {
            let commands = prompt_slash_commands(input);
            let selected_command = commands
                .get(clamp_selected_index(selected_index, commands.len()))
                .copied()?;

            Some(PromptSuggestionSelection::Command(selected_command))
        }
        PromptSlashStage::Agent => available_agent_kinds
            .get(clamp_selected_index(
                selected_index,
                available_agent_kinds.len(),
            ))
            .copied()
            .map(PromptSuggestionSelection::Agent),
        PromptSlashStage::Model => {
            let selected_agent_kind = resolve_model_stage_agent(
                session_agent_kind,
                available_agent_kinds,
                selected_agent,
            )?;
            let models = selected_agent_kind.models();
            let selected_model = models
                .get(clamp_selected_index(selected_index, models.len()))
                .copied()?;

            Some(PromptSuggestionSelection::Model(selected_model))
        }
    }
}

/// Clamps one slash-menu selection index to the highest visible row index.
fn clamp_selected_index(selected_index: usize, option_count: usize) -> usize {
    selected_index.min(option_count.saturating_sub(1))
}

/// Resolves the agent shown for `/model` model selection while preserving the
/// current session agent when it is still locally runnable.
///
/// When `selected_agent` is absent, this intentionally prefers
/// `session_agent_kind` over the first available agent so the model list stays
/// aligned with the current session backend.
fn resolve_model_stage_agent(
    session_agent_kind: AgentKind,
    available_agent_kinds: &[AgentKind],
    selected_agent: Option<AgentKind>,
) -> Option<AgentKind> {
    selected_agent.or_else(|| {
        agent::resolve_prompt_model_agent_kind(session_agent_kind, available_agent_kinds)
    })
}

/// Returns the fixed description text for one slash command label.
fn command_description(command: &str) -> &'static str {
    match command {
        "/model" => "Choose an agent and model for this session.",
        "/stats" => "Check session stats.",
        _ => "Prompt slash command.",
    }
}

/// Returns all slash commands whose prefixes match the current input.
fn prompt_slash_commands(input: &str) -> Vec<&'static str> {
    let lowered = input.to_lowercase();
    let mut commands = vec!["/model", "/stats"];
    commands.retain(|command| command.starts_with(&lowered));

    commands
}

/// Returns the exclusive end index for an `[Image #n]` placeholder token that
/// starts at `start_index`.
fn image_token_end_index(characters: &[char], start_index: usize) -> Option<usize> {
    let token_body = characters.get(start_index..)?;
    if token_body.len() < "[Image #1]".chars().count() || token_body.first() != Some(&'[') {
        return None;
    }

    let image_prefix = ['[', 'I', 'm', 'a', 'g', 'e', ' ', '#'];
    if token_body.get(..image_prefix.len())? != image_prefix {
        return None;
    }

    let mut scan_index = start_index + image_prefix.len();
    let mut saw_digit = false;
    while let Some(character) = characters.get(scan_index) {
        if character.is_ascii_digit() {
            saw_digit = true;
            scan_index += 1;

            continue;
        }

        if *character == ']' && saw_digit {
            return Some(scan_index + 1);
        }

        return None;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_attachment_state_registers_images_in_placeholder_order() {
        // Arrange
        let mut attachment_state = PromptAttachmentState::new();

        // Act
        let first_placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"));
        let second_placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"));

        // Assert
        assert_eq!(first_placeholder, "[Image #1]");
        assert_eq!(second_placeholder, "[Image #2]");
        assert_eq!(attachment_state.attachments.len(), 2);
        assert_eq!(
            attachment_state.attachment_for_placeholder("[Image #2]"),
            Some(&PromptAttachment {
                attachment_number: 2,
                local_image_path: PathBuf::from("/tmp/second-image.png"),
                placeholder: "[Image #2]".to_string(),
            })
        );
    }

    #[test]
    fn test_prompt_attachment_state_reset_clears_attachments_and_restarts_numbering() {
        // Arrange
        let mut attachment_state = PromptAttachmentState::new();
        let _ = attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"));

        // Act
        attachment_state.reset();
        let placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"));

        // Assert
        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(attachment_state.next_attachment_number, 2);
        assert_eq!(placeholder, "[Image #1]");
    }

    #[test]
    fn test_prompt_attachment_state_refresh_next_attachment_number_reuses_gaps() {
        // Arrange
        let mut attachment_state = PromptAttachmentState {
            attachments: vec![
                PromptAttachment::new(1, PathBuf::from("/tmp/first-image.png")),
                PromptAttachment::new(3, PathBuf::from("/tmp/third-image.png")),
            ],
            next_attachment_number: 99,
        };

        // Act
        attachment_state.refresh_next_attachment_number();

        // Assert
        assert_eq!(attachment_state.next_attachment_number, 2);
    }

    #[test]
    fn test_prompt_slash_state_replace_available_agent_kinds_clears_unavailable_selection() {
        // Arrange
        let mut slash_state =
            PromptSlashState::with_available_agent_kinds(vec![AgentKind::Claude, AgentKind::Codex]);
        slash_state.selected_agent = Some(AgentKind::Claude);
        slash_state.selected_index = 2;
        slash_state.stage = PromptSlashStage::Model;

        // Act
        slash_state.replace_available_agent_kinds(vec![AgentKind::Codex]);

        // Assert
        assert_eq!(slash_state.available_agent_kinds, vec![AgentKind::Codex]);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 0);
        assert_eq!(slash_state.stage, PromptSlashStage::Agent);
    }

    #[test]
    fn test_slash_suggestion_list_for_command_stage_has_description() {
        // Arrange
        let composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/m".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(
            suggestion_list,
            PromptSuggestionList {
                items: vec![PromptSuggestionItem {
                    badge: None,
                    detail: Some("Choose an agent and model for this session.".to_string()),
                    label: "/model".to_string(),
                    metadata: None,
                }],
                selected_index: 0,
                title: "Slash Command (j/k move, Enter select)".to_string(),
            }
        );
    }

    #[test]
    fn test_slash_suggestion_list_for_agent_stage_uses_available_agent_kinds() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/model".to_string()),
            vec![AgentKind::Claude],
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Agent;

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(suggestion_list.items.len(), 1);
        assert_eq!(suggestion_list.items[0].label, "claude");
    }

    #[test]
    fn test_selected_slash_action_returns_selected_model() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/model".to_string()),
            vec![AgentKind::Claude],
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Model;
        composer.slash_state.selected_agent = Some(AgentKind::Claude);

        // Act
        let selection = composer.selected_slash_action(AgentKind::Codex);

        // Assert
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Model(AgentModel::ClaudeOpus46))
        );
    }

    #[test]
    fn test_selected_slash_action_clamps_stale_command_index() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/s".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );
        composer.slash_state.selected_index = 9;

        // Act
        let selection = composer.selected_slash_action(AgentKind::Codex);

        // Assert
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Command("/stats"))
        );
    }

    #[test]
    fn test_model_stage_suggestion_list_prefers_available_session_agent_when_unset() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/model".to_string()),
            vec![AgentKind::Gemini, AgentKind::Codex],
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Model;

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        let labels = suggestion_list
            .items
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "gpt-5.4".to_string(),
                "gpt-5.3-codex".to_string(),
                "gpt-5.3-codex-spark".to_string(),
            ]
        );
    }

    #[test]
    fn test_prompt_composer_delete_range_removes_whole_image_token() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.insert_text("Review [Image #1] now");
        composer.attachment_state.attachments =
            vec![PromptAttachment::new(1, PathBuf::from("/tmp/image.png"))];
        composer.attachment_state.next_attachment_number = 2;

        // Act
        composer.delete_range(10, 11);

        // Assert
        assert_eq!(composer.input.text(), "Review  now");
        assert!(composer.attachment_state.attachments.is_empty());
        assert_eq!(composer.attachment_state.next_attachment_number, 1);
    }

    #[test]
    fn test_take_submission_filters_deleted_attachment_placeholders() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.insert_text("One [Image #1] two [Image #2]");
        composer.attachment_state.attachments = vec![
            PromptAttachment::new(1, PathBuf::from("/tmp/one.png")),
            PromptAttachment::new(2, PathBuf::from("/tmp/two.png")),
        ];
        composer.delete_range(4, 15);

        // Act
        let submission = composer.take_submission();

        // Assert
        assert_eq!(submission.text, "One two [Image #2]");
        assert_eq!(submission.attachments.len(), 1);
        assert_eq!(submission.attachments[0].placeholder, "[Image #2]");
    }

    #[test]
    fn test_drain_prompt_submission_keeps_raw_at_lookup_text() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.input =
            InputState::with_text("Check @src/main.rs and @docs/guide.md before @".to_string());

        // Act
        let submission = composer.take_submission();

        // Assert
        assert_eq!(
            submission.text,
            "Check @src/main.rs and @docs/guide.md before @"
        );
        assert_eq!(submission.attachments.len(), 0);
    }

    #[test]
    fn test_drain_prompt_submission_preserves_email_lookalikes() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.input = InputState::with_text("Notify user@example.com and @!".to_string());

        // Act
        let submission = composer.take_submission();

        // Assert
        assert_eq!(submission.text, "Notify user@example.com and @!");
        assert!(submission.attachments.is_empty());
    }

    #[test]
    fn test_render_prompt_text_for_agent_quotes_user_at_lookups() {
        // Arrange
        let prompt_text = "Check @src/main.rs and (@docs/guide.md)";

        // Act
        let rendered_text = render_prompt_text_for_agent(prompt_text);

        // Assert
        assert_eq!(
            rendered_text,
            "Check \"looked/up/src/main.rs\" and (\"looked/up/docs/guide.md\")"
        );
    }

    #[test]
    fn test_render_prompt_text_for_agent_preserves_non_lookup_at_tokens() {
        // Arrange
        let prompt_text = "Notify user@example.com and leave @ alone";

        // Act
        let rendered_text = render_prompt_text_for_agent(prompt_text);

        // Assert
        assert_eq!(rendered_text, "Notify user@example.com and leave @ alone");
    }

    #[test]
    fn test_current_line_delete_range_returns_first_line_range() {
        // Arrange
        let mut input = InputState::with_text("first line\nsecond line".to_string());
        input.cursor = 0;

        // Act
        let delete_range = current_line_delete_range(&input);

        // Assert
        assert_eq!(delete_range, Some((0, 11)));
    }
}
