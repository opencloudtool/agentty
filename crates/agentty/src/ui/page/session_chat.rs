use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::agent::{AgentKind, AgentSelectionMetadata};
use crate::domain::input::{self, extract_at_mention_query};
use crate::domain::session::{Session, Status};
use crate::infra::agent::protocol::QuestionItem;
use crate::infra::file_index;
use crate::runtime::mode::question;
use crate::ui::component::chat_input::{ChatInput, SlashMenu, SlashMenuOption};
use crate::ui::component::session_output::SessionOutput;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode};
use crate::ui::state::help_action::{self, ViewHelpState, ViewSessionState};
use crate::ui::state::prompt::{PromptAtMentionState, PromptSlashStage};
use crate::ui::util::{
    calculate_input_height, question_panel_layout, truncate_with_ellipsis, wrap_lines,
};
use crate::ui::{Component, Page};

/// Maximum rendered height of the prompt input panel, including borders.
const CHAT_INPUT_MAX_PANEL_HEIGHT: u16 = 10;

/// Chat page renderer for a single session.
pub struct SessionChatPage<'a> {
    pub active_progress: Option<&'a str>,
    pub mode: &'a AppMode,
    pub scroll_offset: Option<u16>,
    pub session_index: usize,
    pub sessions: &'a [Session],
}

impl<'a> SessionChatPage<'a> {
    /// Creates a session chat page renderer.
    pub fn new(
        sessions: &'a [Session],
        session_index: usize,
        scroll_offset: Option<u16>,
        mode: &'a AppMode,
        active_progress: Option<&'a str>,
    ) -> Self {
        Self {
            active_progress,
            mode,
            scroll_offset,
            session_index,
            sessions,
        }
    }

    /// Returns the rendered output line count for chat content at a given
    /// width.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering, including review text, so scroll math can stay in
    /// sync with what users see.
    pub(crate) fn rendered_output_line_count(
        session: &Session,
        output_width: u16,
        done_session_output_mode: DoneSessionOutputMode,
        focused_review_status_message: Option<&str>,
        focused_review_text: Option<&str>,
        active_progress: Option<&str>,
    ) -> u16 {
        SessionOutput::rendered_line_count(
            session,
            output_width,
            done_session_output_mode,
            focused_review_status_message,
            focused_review_text,
            active_progress,
        )
    }

    /// Returns the selected `Done`-session output mode for the active page
    /// mode.
    fn done_session_output_mode(&self) -> DoneSessionOutputMode {
        match self.mode {
            AppMode::View {
                done_session_output_mode,
                ..
            } => *done_session_output_mode,
            AppMode::OpenCommandSelector { restore_view, .. } => {
                restore_view.done_session_output_mode
            }
            AppMode::ViewInfoPopup { restore_view, .. } => restore_view.done_session_output_mode,
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Prompt { .. }
            | AppMode::Question { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. } => DoneSessionOutputMode::Summary,
        }
    }

    /// Returns review status text for the active view mode.
    fn focused_review_status_message(&self) -> Option<&str> {
        match self.mode {
            AppMode::View {
                focused_review_status_message,
                ..
            } => focused_review_status_message.as_deref(),
            AppMode::OpenCommandSelector { restore_view, .. }
            | AppMode::ViewInfoPopup { restore_view, .. } => {
                restore_view.focused_review_status_message.as_deref()
            }
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Prompt { .. }
            | AppMode::Question { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. } => None,
        }
    }

    /// Returns review assist text for the active view mode.
    fn focused_review_text(&self) -> Option<&str> {
        match self.mode {
            AppMode::View {
                focused_review_text,
                ..
            } => focused_review_text.as_deref(),
            AppMode::OpenCommandSelector { restore_view, .. }
            | AppMode::ViewInfoPopup { restore_view, .. } => {
                restore_view.focused_review_text.as_deref()
            }
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Prompt { .. }
            | AppMode::Question { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. } => None,
        }
    }

    fn build_slash_menu(
        input: &str,
        stage: PromptSlashStage,
        selected_agent: Option<AgentKind>,
        session: &Session,
    ) -> Option<SlashMenu<'static>> {
        if !input.starts_with('/') {
            return None;
        }

        let (title, options): (&'static str, Vec<SlashMenuOption>) = match stage {
            PromptSlashStage::Command => {
                let lowered = input.to_lowercase();
                let commands = ["/model", "/stats"]
                    .iter()
                    .copied()
                    .filter(|command| command.starts_with(&lowered))
                    .map(|command| SlashMenuOption {
                        description: Self::command_description(command).to_string(),
                        label: command.to_string(),
                    })
                    .collect::<Vec<_>>();

                ("Slash Command (j/k move, Enter select)", commands)
            }
            PromptSlashStage::Agent => (
                "/model Agent (j/k move, Enter select)",
                AgentKind::ALL
                    .iter()
                    .map(|agent| SlashMenuOption {
                        description: agent.description().to_string(),
                        label: agent.name().to_string(),
                    })
                    .collect(),
            ),
            PromptSlashStage::Model => {
                let session_agent = selected_agent.unwrap_or_else(|| session.model.kind());
                let models = session_agent
                    .models()
                    .iter()
                    .map(|model| SlashMenuOption {
                        description: model.description().to_string(),
                        label: model.name().to_string(),
                    })
                    .collect::<Vec<_>>();

                ("/model Model (j/k move, Enter select)", models)
            }
        };
        if options.is_empty() {
            return None;
        }

        Some(SlashMenu {
            options,
            selected_index: 0,
            title,
        })
    }

    fn command_description(command: &str) -> &'static str {
        match command {
            "/model" => "Choose an agent and model for this session.",
            "/stats" => "Check session stats.",
            _ => "Prompt slash command.",
        }
    }

    fn build_at_mention_menu(
        input_text: &str,
        cursor: usize,
        at_mention_state: &PromptAtMentionState,
    ) -> Option<SlashMenu<'static>> {
        let (_, query) = extract_at_mention_query(input_text, cursor)?;
        let filtered = file_index::filter_entries(&at_mention_state.all_entries, &query);

        if filtered.is_empty() {
            return None;
        }

        let max_visible = 10;
        let window_start = at_mention_state
            .selected_index
            .saturating_sub(max_visible / 2);
        let window_end = filtered.len().min(window_start + max_visible);
        let window_start = window_end.saturating_sub(max_visible);

        let options: Vec<SlashMenuOption> = filtered[window_start..window_end]
            .iter()
            .map(|entry| {
                let label = if entry.is_dir {
                    format!("{}/", entry.path)
                } else {
                    entry.path.clone()
                };

                SlashMenuOption {
                    description: if entry.is_dir {
                        "folder".to_string()
                    } else {
                        String::new()
                    },
                    label,
                }
            })
            .collect();

        let display_index = at_mention_state
            .selected_index
            .min(filtered.len().saturating_sub(1))
            .saturating_sub(window_start);

        Some(SlashMenu {
            options,
            selected_index: display_index,
            title: "Files (\u{2191}\u{2193} move, Enter select, Esc dismiss)",
        })
    }

    /// Renders the session header, output panel, and context-aware bottom
    /// panel.
    fn render_session(&self, f: &mut Frame, area: Rect, session: &Session) {
        let bottom_height = self.bottom_height(area, session);
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
            .margin(1)
            .split(area);
        let output_chunks = Layout::default()
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(chunks[0]);

        let mut output =
            SessionOutput::new(session).done_session_output_mode(self.done_session_output_mode());
        output = output.focused_review_status_message(self.focused_review_status_message());
        output = output.focused_review_text(self.focused_review_text());
        if let Some(scroll_offset) = self.scroll_offset {
            output = output.scroll_offset(scroll_offset);
        }
        if let Some(active_progress) = self.active_progress {
            output = output.active_progress(active_progress);
        }
        Self::render_session_header(f, output_chunks[0], session);
        output.render(f, output_chunks[1]);
        self.render_bottom_panel(f, chunks[1], session);
    }

    /// Renders a standalone status/title row above the output panel border.
    fn render_session_header(f: &mut Frame, header_area: Rect, session: &Session) {
        let header_text = Self::session_header_text(session, header_area.width);
        let header = Paragraph::new(header_text).style(Style::default().fg(session.status.color()));

        f.render_widget(header, header_area);
    }

    /// Formats the left-aligned session header text for the available width.
    fn session_header_text(session: &Session, header_width: u16) -> String {
        let status_label = session.status.to_string();
        let status_width = u16::try_from(status_label.len()).unwrap_or(u16::MAX);
        let reserved_width = status_width.saturating_add(5);
        let max_title_width = usize::from(header_width.saturating_sub(reserved_width));
        let header_title = truncate_with_ellipsis(session.display_title(), max_title_width);
        format!(" {status_label} - {header_title} ")
    }

    fn bottom_height(&self, area: Rect, session: &Session) -> u16 {
        if let AppMode::Prompt {
            at_mention_state,
            input,
            slash_state,
            ..
        } = self.mode
        {
            let dropdown_option_count = if input.text().starts_with('/') {
                Self::build_slash_menu(
                    input.text(),
                    slash_state.stage,
                    slash_state.selected_agent,
                    session,
                )
                .map_or(0, |menu| menu.options.len().saturating_add(2))
            } else if let Some(at_state) = at_mention_state {
                Self::build_at_mention_menu(input.text(), input.cursor, at_state)
                    .map_or(0, |menu| menu.options.len().saturating_add(2))
            } else {
                0
            };

            let input_height = calculate_input_height(area.width.saturating_sub(2), input.text())
                .min(CHAT_INPUT_MAX_PANEL_HEIGHT);
            let desired_bottom_height = input_height
                .saturating_add(u16::try_from(dropdown_option_count).unwrap_or(u16::MAX));
            let max_bottom_height = area.height.saturating_sub(1);

            return desired_bottom_height.min(max_bottom_height);
        }

        if let AppMode::Question {
            questions,
            current_index,
            input,
            selected_option_index,
            ..
        } = self.mode
        {
            let question_item = questions.get(*current_index);
            let question = question_item.map_or("", |item| item.text.as_str());
            let options = question_item
                .map(|item| item.options.as_slice())
                .unwrap_or_default();
            let is_free_text_mode = selected_option_index.is_none();
            let options_height =
                question_options_height(options, is_free_text_mode, area.height.saturating_sub(1));
            let input_text = if is_free_text_mode { input.text() } else { "" };

            let layout_available_height =
                area.height.saturating_sub(1).saturating_sub(options_height);
            let panel_layout = question_panel_layout(
                area.width,
                layout_available_height,
                question,
                input_text,
                CHAT_INPUT_MAX_PANEL_HEIGHT,
            );

            return panel_layout
                .question_height
                .saturating_add(options_height)
                .saturating_add(panel_layout.spacer_height)
                .saturating_add(panel_layout.input_height)
                .saturating_add(panel_layout.help_height);
        }

        1
    }

    fn render_bottom_panel(&self, f: &mut Frame, bottom_area: Rect, session: &Session) {
        if let AppMode::Prompt {
            at_mention_state,
            input,
            slash_state,
            ..
        } = self.mode
        {
            let title = format!(" [{}] ", session.model.as_str());

            let menu = if input.text().starts_with('/') {
                Self::build_slash_menu(
                    input.text(),
                    slash_state.stage,
                    slash_state.selected_agent,
                    session,
                )
                .map(|mut menu| {
                    let max_index = menu.options.len().saturating_sub(1);
                    menu.selected_index = slash_state.selected_index.min(max_index);
                    menu
                })
            } else if let Some(at_state) = at_mention_state {
                Self::build_at_mention_menu(input.text(), input.cursor, at_state)
            } else {
                None
            };

            let mut chat_input =
                ChatInput::new(&title, input.text(), input.cursor).placeholder("Type your message");

            if let Some(menu) = menu {
                chat_input = chat_input.slash_menu(menu);
            }

            chat_input.render(f, bottom_area);

            return;
        }

        if let AppMode::Question {
            questions,
            current_index,
            input,
            selected_option_index,
            ..
        } = self.mode
        {
            render_question_panel(
                f,
                bottom_area,
                questions,
                *current_index,
                input,
                *selected_option_index,
            );

            return;
        }

        let help_actions = Self::view_footer_actions(session, self.done_session_output_mode());
        let help_message = Paragraph::new(help_action::footer_line(&help_actions));
        f.render_widget(help_message, bottom_area);
    }

    /// Returns the footer action list for a given session in view mode.
    ///
    /// `InProgress` sessions keep worktree access while hiding edit and diff
    /// actions. `Rebasing` sessions keep worktree access but hide edit and
    /// diff shortcuts. `Merging` and `Queued` sessions hide worktree shortcuts
    /// while the merge queue is active. `Review` sessions expose review
    /// shortcuts with read-only assist generation (`m` opens merge
    /// confirmation before queueing), and `Done` sessions expose only
    /// read-only shortcuts. `Canceled` sessions expose only `back`, `scroll`,
    /// and `help`.
    fn view_footer_actions(
        session: &Session,
        done_session_output_mode: DoneSessionOutputMode,
    ) -> Vec<help_action::HelpAction> {
        let session_state = match session.status {
            Status::Done => ViewSessionState::Done,
            Status::Canceled => ViewSessionState::Canceled,
            Status::InProgress => ViewSessionState::InProgress,
            Status::Rebasing => ViewSessionState::Rebasing,
            Status::Merging | Status::Queued => ViewSessionState::MergeQueue,
            Status::Review => ViewSessionState::Review,
            _ => ViewSessionState::Interactive,
        };

        let mut actions = help_action::view_footer_actions(ViewHelpState {
            publish_branch_action: session.publish_branch_action(),
            session_state,
        });

        if session_state == ViewSessionState::Done {
            let toggle_action_label = Self::done_toggle_action_label(done_session_output_mode);
            if let Some(toggle_action_index) = actions.iter().position(|action| action.key == "t") {
                actions[toggle_action_index] =
                    help_action::HelpAction::new(toggle_action_label, "t", "Switch summary/output");
            }
        }

        actions
    }

    /// Returns the `t` footer label for `Status::Done` output mode toggling.
    fn done_toggle_action_label(done_session_output_mode: DoneSessionOutputMode) -> &'static str {
        match done_session_output_mode {
            DoneSessionOutputMode::Summary => "output",
            DoneSessionOutputMode::Output | DoneSessionOutputMode::FocusedReview => "summary",
        }
    }
}

/// Renders the question-mode bottom panel with question text, options, input,
/// and help footer.
fn render_question_panel(
    f: &mut Frame,
    bottom_area: Rect,
    questions: &[QuestionItem],
    current_index: usize,
    input: &input::InputState,
    selected_option_index: Option<usize>,
) {
    let question_item = questions.get(current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item
        .map(|item| item.options.as_slice())
        .unwrap_or_default();
    let is_free_text_mode = selected_option_index.is_none();
    let show_input = is_free_text_mode;

    let options_height = question_options_height(options, is_free_text_mode, bottom_area.height);

    let layout_available_height = bottom_area.height.saturating_sub(options_height);
    let input_text = if show_input { input.text() } else { "" };
    let panel_layout = question_panel_layout(
        bottom_area.width,
        layout_available_height,
        question,
        input_text,
        CHAT_INPUT_MAX_PANEL_HEIGHT,
    );

    let (input_height, spacer_height) = if show_input {
        (panel_layout.input_height, panel_layout.spacer_height)
    } else {
        (0, 0)
    };

    let chunks = Layout::default()
        .constraints([
            Constraint::Length(panel_layout.question_height),
            Constraint::Length(options_height),
            Constraint::Length(spacer_height),
            Constraint::Length(input_height),
            Constraint::Length(panel_layout.help_height),
        ])
        .split(bottom_area);

    let question_title = format!("Question {}/{}", current_index + 1, questions.len());
    if panel_layout.question_height > 0 {
        let question_para =
            Paragraph::new(wrap_lines(question, usize::from(bottom_area.width.max(1))))
                .style(Style::default().fg(Color::Yellow));
        f.render_widget(question_para, chunks[0]);
    }

    if options_height > 0 {
        render_question_options(f, chunks[1], options, selected_option_index);
    }

    if show_input {
        let input_placeholder = "Type custom answer (Enter: send, Esc: skip)";
        let input_title = format!(" [{question_title}] ");
        let chat_input =
            ChatInput::new(&input_title, input.text(), input.cursor).placeholder(input_placeholder);
        if input_height > 0 {
            chat_input.render(f, chunks[3]);
        }
    }

    let mut help_actions = Vec::new();
    if is_free_text_mode {
        help_actions.push(help_action::HelpAction::new(
            "send",
            "Enter",
            "Send response",
        ));
    } else {
        help_actions.push(help_action::HelpAction::new(
            "navigate",
            "j/k/Up/Down",
            "Select option",
        ));
        help_actions.push(help_action::HelpAction::new(
            "choose",
            "Enter",
            "Choose option",
        ));
    }
    help_actions.push(help_action::HelpAction::new(
        "skip",
        "Esc",
        "Skip (no answer)",
    ));
    if panel_layout.help_height > 0 {
        let help_para = Paragraph::new(help_action::footer_line(&help_actions))
            .alignment(ratatui::layout::Alignment::Right);
        f.render_widget(help_para, chunks[4]);
    }
}

/// Returns the total height for the question options section.
///
/// Includes the header line, predefined options, and the virtual "Type custom
/// answer" entry when navigating. Returns zero when there are no predefined
/// options and the user is in free-text mode.
fn question_options_height(options: &[String], is_free_text_mode: bool, max_height: u16) -> u16 {
    let has_visible_options = !options.is_empty() || !is_free_text_mode;
    if !has_visible_options {
        return 0;
    }

    let virtual_entry_count = u16::from(!is_free_text_mode);

    u16::try_from(options.len())
        .unwrap_or(u16::MAX)
        .saturating_add(1) // +1 header
        .saturating_add(virtual_entry_count)
        .min(max_height)
}

/// Renders the answer option list for the active question.
///
/// Each predefined option is shown as a numbered line. A virtual "Type
/// custom answer" entry is always appended while navigating (i.e.
/// `selected_option_index` is `Some`). In free-text mode the virtual entry
/// is omitted because the text input appears directly below instead.
fn render_question_options(
    f: &mut Frame,
    area: Rect,
    options: &[String],
    selected_option_index: Option<usize>,
) {
    let is_navigating = selected_option_index.is_some();
    let capacity = options.len() + 1 + usize::from(is_navigating);
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(capacity);
    lines.push(Line::from(Span::styled(
        "Options:",
        Style::default().fg(Color::Yellow),
    )));

    for (option_index, option_text) in options.iter().enumerate() {
        let is_selected = selected_option_index == Some(option_index);
        let prefix = if is_selected { "▸ " } else { "  " };
        let label = format!("{prefix}{}. {option_text}", option_index + 1);
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(Span::styled(label, style)));
    }

    // Virtual "Type custom answer" entry — only shown while navigating.
    if is_navigating {
        let type_custom_index = options.len();
        let is_type_custom_selected = selected_option_index == Some(type_custom_index);
        let type_custom_prefix = if is_type_custom_selected {
            "▸ "
        } else {
            "  "
        };
        let type_custom_label = format!(
            "{type_custom_prefix}{}. {}",
            type_custom_index + 1,
            question::TYPE_CUSTOM_ANSWER,
        );
        let type_custom_style = if is_type_custom_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            type_custom_label,
            type_custom_style,
        )));
    }

    f.render_widget(Paragraph::new(lines), area);
}

impl Page for SessionChatPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        if let Some(session) = self.sessions.get(self.session_index) {
            self.render_session(f, area, session);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::input::InputState;
    use crate::domain::session::{SessionSize, SessionStats};
    use crate::infra::agent::protocol::QuestionItem;
    use crate::infra::file_index::FileEntry;
    use crate::ui::state::prompt::{PromptHistoryState, PromptSlashState};

    fn session_fixture() -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::New,
            summary: None,
            title: None,
            updated_at: 0,
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        let start = usize::from(row) * usize::from(width);
        let end = start + usize::from(width);

        buffer.content()[start..end]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn view_help_text(
        session: &Session,
        done_session_output_mode: DoneSessionOutputMode,
    ) -> String {
        help_action::footer_line(&SessionChatPage::view_footer_actions(
            session,
            done_session_output_mode,
        ))
        .to_string()
    }

    #[test]
    fn test_build_slash_menu_for_command_stage_has_description() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu =
            SessionChatPage::build_slash_menu("/m", PromptSlashStage::Command, None, &session)
                .expect("expected slash menu");

        // Assert
        assert_eq!(menu.options.len(), 1);
        assert_eq!(menu.options[0].label, "/model");
        assert_eq!(
            menu.options[0].description,
            "Choose an agent and model for this session."
        );
    }

    #[test]
    fn test_build_slash_menu_for_agent_stage_has_agent_descriptions() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu =
            SessionChatPage::build_slash_menu("/model", PromptSlashStage::Agent, None, &session)
                .expect("expected slash menu");

        // Assert
        assert_eq!(menu.options.len(), AgentKind::ALL.len());
        assert_eq!(menu.options[0].label, "gemini");
        assert_eq!(menu.options[0].description, "Google Gemini CLI agent.");
    }

    #[test]
    fn test_build_slash_menu_for_model_stage_has_model_descriptions() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu = SessionChatPage::build_slash_menu(
            "/model",
            PromptSlashStage::Model,
            Some(AgentKind::Codex),
            &session,
        )
        .expect("expected slash menu");

        // Assert
        assert_eq!(menu.options.len(), AgentKind::Codex.models().len());
        assert_eq!(menu.options[0].label, "gpt-5.4");
        assert_eq!(
            menu.options[0].description,
            "Latest Codex model for coding quality."
        );
    }

    fn file_entries_fixture() -> Vec<FileEntry> {
        vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: true,
                path: "tests".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "Cargo.toml".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "README.md".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "tests/integration.rs".to_string(),
            },
        ]
    }

    #[test]
    fn test_build_at_mention_menu_with_matches() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu =
            SessionChatPage::build_at_mention_menu("@src", 4, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options.len(), 3);
        assert_eq!(menu.options[0].label, "src/");
        assert_eq!(menu.options[0].description, "folder");
        assert_eq!(menu.options[1].label, "src/lib.rs");
        assert_eq!(menu.options[2].label, "src/main.rs");
    }

    #[test]
    fn test_build_at_mention_menu_with_trailing_slash_includes_exact_directory() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu =
            SessionChatPage::build_at_mention_menu("@src/", 5, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options[0].label, "src/");
        assert_eq!(menu.options[0].description, "folder");
        assert_eq!(menu.options[1].label, "src/lib.rs");
        assert_eq!(menu.options[2].label, "src/main.rs");
    }

    #[test]
    fn test_build_at_mention_menu_no_matches() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu = SessionChatPage::build_at_mention_menu("@nonexistent", 12, &state);

        // Assert
        assert!(menu.is_none());
    }

    #[test]
    fn test_build_at_mention_menu_empty_query_returns_all() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu = SessionChatPage::build_at_mention_menu("@", 1, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options.len(), 7);
    }

    #[test]
    fn test_build_at_mention_menu_caps_at_10() {
        // Arrange
        let entries: Vec<FileEntry> = (0..20)
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:02}.rs"),
            })
            .collect();
        let state = PromptAtMentionState::new(entries);

        // Act
        let menu = SessionChatPage::build_at_mention_menu("@", 1, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options.len(), 10);
    }

    #[test]
    fn test_build_at_mention_menu_clamps_selected_index() {
        // Arrange
        let mut state = PromptAtMentionState::new(file_entries_fixture());
        state.selected_index = 100; // Way beyond bounds

        // Act
        let menu =
            SessionChatPage::build_at_mention_menu("@src", 4, &state).expect("expected menu");

        // Assert — should clamp to last visible item
        assert_eq!(menu.selected_index, 2);
    }

    #[test]
    fn test_build_at_mention_menu_scroll_window() {
        // Arrange
        let entries: Vec<FileEntry> = (0..20)
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:02}.rs"),
            })
            .collect();
        let mut state = PromptAtMentionState::new(entries);
        state.selected_index = 15;

        // Act
        let menu = SessionChatPage::build_at_mention_menu("@", 1, &state).expect("expected menu");

        // Assert — window should be centered around index 15
        assert_eq!(menu.options.len(), 10);
        assert_eq!(menu.options[0].label, "file_10.rs");
        assert_eq!(menu.options[9].label, "file_19.rs");
        assert_eq!(menu.selected_index, 5); // 15 - 10 = 5
    }

    #[test]
    fn test_build_at_mention_menu_directory_has_trailing_slash() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
        ];
        let state = PromptAtMentionState::new(entries);

        // Act
        let menu =
            SessionChatPage::build_at_mention_menu("@src", 4, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options[0].label, "src/");
        assert_eq!(menu.options[0].description, "folder");
        assert_eq!(menu.options[1].label, "src/main.rs");
        assert_eq!(menu.options[1].description, "");
    }

    #[test]
    fn test_build_slash_menu_for_command_stage_includes_commands() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu =
            SessionChatPage::build_slash_menu("/", PromptSlashStage::Command, None, &session)
                .expect("expected slash menu");

        // Assert
        let labels: Vec<&str> = menu.options.iter().map(|opt| opt.label.as_str()).collect();
        assert!(labels.contains(&"/model"));
        assert!(labels.contains(&"/stats"));
    }

    #[test]
    fn test_command_description_stats() {
        // Arrange & Act
        let description = SessionChatPage::command_description("/stats");

        // Assert
        assert_eq!(description, "Check session stats.");
    }

    #[test]
    fn test_rendered_output_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        session.output = "word ".repeat(40);
        let raw_line_count = u16::try_from(session.output.lines().count()).unwrap_or(u16::MAX);

        // Act
        let rendered_line_count = SessionChatPage::rendered_output_line_count(
            &session,
            20,
            DoneSessionOutputMode::Summary,
            None,
            None,
            None,
        );

        // Assert
        assert!(rendered_line_count > raw_line_count);
    }

    #[test]
    fn test_view_help_text_in_progress_shows_open_and_hides_diff() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Summary);

        // Assert
        assert!(help_text.contains("q: back"));
        assert!(!help_text.contains("Ctrl+c: stop"));
        assert!(help_text.contains("j/k: scroll"));
        assert!(help_text.contains("o: open"));
        assert!(!help_text.contains("d: diff"));
        assert!(!help_text.contains("Enter: reply"));
    }

    #[test]
    fn test_view_help_text_rebasing_keeps_open() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Rebasing;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Summary);

        // Assert
        assert!(help_text.contains("q: back"));
        assert!(help_text.contains("j/k: scroll"));
        assert!(help_text.contains("o: open"));
        assert!(!help_text.contains("Ctrl+c: stop"));
        assert!(!help_text.contains("Enter: reply"));
        assert!(!help_text.contains("d: diff"));
    }

    #[test]
    fn test_view_help_text_merge_queue_statuses_hide_worktree_open_hint() {
        // Arrange
        let merge_queue_statuses = [Status::Queued, Status::Merging];

        // Act
        let help_texts: Vec<String> = merge_queue_statuses
            .iter()
            .map(|session_status| {
                let mut session = session_fixture();
                session.status = *session_status;

                view_help_text(&session, DoneSessionOutputMode::Summary)
            })
            .collect();

        // Assert
        for help_text in help_texts {
            assert!(help_text.contains("q: back"));
            assert!(help_text.contains("j/k: scroll"));
            assert!(!help_text.contains("o: open"));
            assert!(!help_text.contains("Ctrl+c: stop"));
        }
    }

    #[test]
    fn test_view_help_text_canceled_shows_only_back_scroll_and_help() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Canceled;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Summary);

        // Assert
        assert_eq!(help_text, "q: back | j/k: scroll | ?: help");
    }

    #[test]
    fn test_view_help_text_includes_reply_open_and_git_actions() {
        // Arrange
        let session = session_fixture();

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Summary);

        // Assert
        assert!(help_text.contains("Enter: reply"));
        assert!(help_text.contains("o: open"));
        assert!(help_text.contains("m: add to merge queue"));
        assert!(help_text.contains("r: rebase"));
        assert!(!help_text.contains("d: diff"));
    }

    #[test]
    fn test_view_help_text_review_shows_focused_review_and_hides_diff_hint() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Summary);

        // Assert
        assert!(!help_text.contains("d: diff"));
        assert!(help_text.contains("f: review"));
        assert!(help_text.contains("Enter: reply"));
    }

    #[test]
    fn test_view_help_text_review_without_link_shows_push_branch_hint() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Summary);

        // Assert
        assert!(help_text.contains("p: push branch"));
    }

    #[test]
    fn test_view_help_text_done_hides_open_hint() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Done;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Summary);

        // Assert
        assert!(!help_text.contains("o: open"));
        assert!(help_text.contains("t: output"));
        assert!(help_text.contains("j/k: scroll"));
    }

    #[test]
    fn test_view_help_text_done_output_mode_shows_summary_toggle_hint() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Done;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::Output);

        // Assert
        assert!(help_text.contains("t: summary"));
    }

    #[test]
    fn test_view_help_text_done_focused_review_mode_shows_summary_toggle_hint() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Done;

        // Act
        let help_text = view_help_text(&session, DoneSessionOutputMode::FocusedReview);

        // Assert
        assert!(help_text.contains("t: summary"));
    }

    #[test]
    fn test_bottom_height_caps_prompt_input_panel_to_ten_lines() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Prompt {
            at_mention_state: None,
            history_state: PromptHistoryState::default(),
            slash_state: PromptSlashState::new(),
            session_id: "session-id".to_string(),
            input: InputState::with_text("line\n".repeat(80)),
            scroll_offset: None,
        };
        let page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let area = Rect::new(0, 0, 120, 30);

        // Act
        let bottom_height = page.bottom_height(area, &session);

        // Assert
        assert_eq!(bottom_height, CHAT_INPUT_MAX_PANEL_HEIGHT);
    }

    #[test]
    fn test_bottom_height_preserves_space_for_output_area() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Prompt {
            at_mention_state: None,
            history_state: PromptHistoryState::default(),
            slash_state: PromptSlashState::new(),
            session_id: "session-id".to_string(),
            input: InputState::with_text("line\n".repeat(80)),
            scroll_offset: None,
        };
        let page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let area = Rect::new(0, 0, 120, 8);

        // Act
        let bottom_height = page.bottom_height(area, &session);

        // Assert
        assert_eq!(bottom_height, 7);
    }

    #[test]
    fn test_bottom_height_question_mode_includes_question_input_and_help_rows() {
        // Arrange
        let session = session_fixture();
        let question = "Need an explicit migration plan?".to_string();
        let answer = "Use two phases: schema and runtime.";
        let mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: question.clone(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::with_text(answer.to_string()),
            selected_option_index: None,
        };
        let page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let area = Rect::new(0, 0, 120, 30);
        let options_height = question_options_height(&[], true, area.height.saturating_sub(1));
        let layout_available = area.height.saturating_sub(1).saturating_sub(options_height);
        let panel_layout = question_panel_layout(
            area.width,
            layout_available,
            &question,
            answer,
            CHAT_INPUT_MAX_PANEL_HEIGHT,
        );
        let expected_height = panel_layout
            .question_height
            .saturating_add(options_height)
            .saturating_add(panel_layout.spacer_height)
            .saturating_add(panel_layout.input_height)
            .saturating_add(panel_layout.help_height);

        // Act
        let bottom_height = page.bottom_height(area, &session);

        // Assert
        assert_eq!(bottom_height, expected_height);
    }

    #[test]
    fn test_bottom_height_question_mode_preserves_space_for_output_area() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Need details?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::with_text("answer\n".repeat(50)),
            selected_option_index: None,
        };
        let page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let area = Rect::new(0, 0, 120, 8);

        // Act
        let bottom_height = page.bottom_height(area, &session);

        // Assert
        assert_eq!(bottom_height, 7);
    }

    #[test]
    fn test_bottom_height_question_mode_includes_options_height() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Continue?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::default(),
            selected_option_index: None,
        };
        let page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let area = Rect::new(0, 0, 80, 20);

        // Act
        let bottom_height = page.bottom_height(area, &session);

        // Assert — options_height = 2 options + 1 header = 3
        let options_height: u16 = 3;
        let layout_available = area.height.saturating_sub(1).saturating_sub(options_height);
        let panel_layout = question_panel_layout(
            area.width,
            layout_available,
            "Continue?",
            "",
            CHAT_INPUT_MAX_PANEL_HEIGHT,
        );
        let expected = panel_layout
            .question_height
            .saturating_add(options_height)
            .saturating_add(panel_layout.spacer_height)
            .saturating_add(panel_layout.input_height)
            .saturating_add(panel_layout.help_height);
        assert_eq!(bottom_height, expected);
        assert!(
            bottom_height > 3,
            "should have room for question, options, input and help"
        );
    }

    #[test]
    fn test_render_places_session_header_above_output_border() {
        // Arrange
        let mut session = session_fixture();
        session.title = Some("Header Session".to_string());
        let mode = AppMode::List;
        let mut page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let width = 80;
        let backend = ratatui::backend::TestBackend::new(width, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw session chat page");

        // Assert
        let header_row = buffer_row_text(terminal.backend().buffer(), 1, width);
        let output_border_row = buffer_row_text(terminal.backend().buffer(), 2, width);
        assert!(header_row.contains("New - Header Session"));
        assert!(!output_border_row.contains("New - Header Session"));
    }

    #[test]
    fn test_render_truncates_long_session_header_title() {
        // Arrange
        let mut session = session_fixture();
        let long_title = "This is a very long session title for truncation behavior validation";
        session.title = Some(long_title.to_string());
        let mode = AppMode::List;
        let mut page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let backend = ratatui::backend::TestBackend::new(28, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw session chat page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains(long_title));
        assert!(text.contains("..."));
    }

    #[test]
    fn test_render_keeps_session_header_title_without_review_request_metadata() {
        // Arrange
        let mut session = session_fixture();
        session.title = Some("Header Session".to_string());
        let mode = AppMode::List;
        let mut page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let width = 90;
        let backend = ratatui::backend::TestBackend::new(width, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw session chat page");

        // Assert
        let header_row = buffer_row_text(terminal.backend().buffer(), 1, width);
        assert!(header_row.contains("Header Session"));
        assert!(!header_row.contains("GitHub"));
    }

    #[test]
    fn test_render_question_mode_keeps_typed_answer_visible_in_tight_layout() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Need a detailed migration plan with rollback guidance?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::with_text("typed answer".to_string()),
            selected_option_index: None,
        };
        let mut page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let backend = ratatui::backend::TestBackend::new(32, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("typed answer"));
    }

    #[test]
    fn test_render_question_mode_includes_blank_row_between_question_and_input() {
        // Arrange
        let session = session_fixture();
        let question = "Need a detailed migration plan with rollback guidance?".to_string();
        let mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: question.clone(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::with_text("typed answer".to_string()),
            selected_option_index: None,
        };
        let mut page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let width = 40;
        let height = 12;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode");

        // Assert
        let area = Rect::new(0, 0, width, height);
        let bottom_height = page.bottom_height(area, &session);
        let bottom_top = 1 + height.saturating_sub(2).saturating_sub(bottom_height);
        let panel_layout = question_panel_layout(
            width.saturating_sub(2),
            bottom_height,
            &question,
            "typed answer",
            CHAT_INPUT_MAX_PANEL_HEIGHT,
        );
        let options_height = question_options_height(&[], true, bottom_height);
        let spacer_row = bottom_top + panel_layout.question_height + options_height;
        let spacer_text = buffer_row_text(terminal.backend().buffer(), spacer_row, width);
        assert!(spacer_text.trim().is_empty());
    }

    #[test]
    fn test_render_question_mode_with_options_shows_option_text() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Continue?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::default(),
            selected_option_index: Some(0),
        };
        let mut page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let width = 50;
        let height = 14;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode with options");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Options:"), "should render options header");
        assert!(text.contains("Yes"), "should render first option");
        assert!(text.contains("No"), "should render second option");
    }

    #[test]
    fn test_render_question_mode_with_options_in_small_terminal_does_not_panic() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: vec![
                    "A".to_string(),
                    "B".to_string(),
                    "C".to_string(),
                    "D".to_string(),
                    "E".to_string(),
                ],
                text: "Pick one of the many options?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::default(),
            selected_option_index: None,
        };
        let mut page = SessionChatPage::new(std::slice::from_ref(&session), 0, None, &mode, None);
        let backend = ratatui::backend::TestBackend::new(30, 6);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act + Assert (should not panic)
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode with many options in small terminal");
    }
}
