use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::ui::util::{
    CHAT_INPUT_MAX_VISIBLE_LINES, calculate_input_viewport, compute_input_layout,
    input_cursor_position, placeholder_cursor_position, slash_menu_dropdown_height,
};
use crate::ui::{Component, style};

/// A single slash-command dropdown option.
pub struct SlashMenuOption {
    pub description: String,
    pub label: String,
}

/// Slash-command dropdown rendered above the prompt input block.
pub struct SlashMenu<'a> {
    pub options: Vec<SlashMenuOption>,
    pub selected_index: usize,
    pub title: &'a str,
}

/// Prompt input component with optional slash-command dropdown.
pub struct ChatInput<'a> {
    pub placeholder: &'a str,
    cursor: usize,
    input: &'a str,
    slash_menu: Option<SlashMenu<'a>>,
    title: &'a str,
}

impl<'a> ChatInput<'a> {
    /// Creates a new prompt input component.
    pub fn new(title: &'a str, input: &'a str, cursor: usize) -> Self {
        Self {
            placeholder: "",
            cursor,
            input,
            slash_menu: None,
            title,
        }
    }

    /// Sets the input placeholder text.
    #[must_use]
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// Sets the slash command menu.
    #[must_use]
    pub fn slash_menu(mut self, slash_menu: SlashMenu<'a>) -> Self {
        self.slash_menu = Some(slash_menu);
        self
    }

    /// Returns the shared block styling for the focused prompt input frame.
    fn input_block(&self) -> Block<'a> {
        let title = format!(" {} ", self.title);

        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Self::focused_border_style())
            .title(Span::styled(title, Self::focused_title_style()))
    }

    /// Returns the border style used to keep the active prompt field visually
    /// prominent.
    fn focused_border_style() -> Style {
        Style::default()
            .fg(style::palette::ACCENT)
            .add_modifier(Modifier::BOLD)
    }

    /// Returns the title style used by the focused prompt input frame.
    fn focused_title_style() -> Style {
        Style::default()
            .fg(style::palette::ACCENT)
            .add_modifier(Modifier::BOLD)
    }

    /// Returns the shared block styling for slash-command and file suggestion
    /// dropdowns.
    fn dropdown_block(title: &str) -> Block<'_> {
        let title = format!(" {title} ");

        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(style::palette::ACCENT_SOFT))
            .title(Span::styled(
                title,
                Style::default().fg(style::palette::ACCENT_SOFT),
            ))
    }

    /// Renders the slash-command dropdown using the shared chat input chrome.
    fn render_slash_dropdown(f: &mut Frame, area: Rect, slash_menu: &SlashMenu<'_>) {
        let rows = slash_menu
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let is_selected = index == slash_menu.selected_index;
                let prefix = if is_selected { ">" } else { " " };
                let label_style = if is_selected {
                    Style::default()
                        .fg(style::palette::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(style::palette::TEXT_MUTED)
                };
                let description_style = if is_selected {
                    Style::default().fg(style::palette::TEXT_MUTED)
                } else {
                    Style::default().fg(style::palette::TEXT_SUBTLE)
                };

                Line::from(vec![
                    Span::styled(format!("{prefix} {}", option.label), label_style),
                    Span::styled(format!("  {}", option.description), description_style),
                ])
            })
            .collect::<Vec<_>>();

        let dropdown = Paragraph::new(rows).block(Self::dropdown_block(slash_menu.title));

        f.render_widget(Clear, area);
        f.render_widget(dropdown, area);
    }

    /// Render the prompt input with an internally scrollable viewport.
    fn render_input(&self, f: &mut Frame, area: Rect) {
        let block = self.input_block();

        if self.input.is_empty() {
            let prefix = " › ";
            let display_lines = vec![Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(style::palette::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    self.placeholder,
                    Style::default().fg(style::palette::TEXT_SUBTLE),
                ),
            ])];

            let widget = Paragraph::new(display_lines).block(block);
            f.render_widget(Clear, area);
            f.render_widget(widget, area);
            f.set_cursor_position(placeholder_cursor_position(area));

            return;
        }

        let (display_lines, cursor_x, cursor_y) =
            compute_input_layout(self.input, area.width, self.cursor);
        let viewport_height = area
            .height
            .saturating_sub(2)
            .min(CHAT_INPUT_MAX_VISIBLE_LINES);
        let total_line_count = Self::total_viewport_line_count(display_lines.len(), cursor_y);
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);
        let widget = Paragraph::new(display_lines)
            .scroll((scroll_offset, 0))
            .block(block);

        f.render_widget(Clear, area);
        f.render_widget(widget, area);
        f.set_cursor_position(input_cursor_position(area, cursor_x, cursor_row));
    }

    /// Computes the total line count used by input viewport scrolling.
    ///
    /// The cursor can legally point to a trailing wrapped line that has no
    /// visible characters yet (exact line-fit case), so viewport calculations
    /// must account for whichever line index is greater.
    fn total_viewport_line_count(display_line_count: usize, cursor_y: u16) -> usize {
        display_line_count.max(usize::from(cursor_y).saturating_add(1))
    }
}

impl Component for ChatInput<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        if let Some(slash_menu) = &self.slash_menu {
            let dropdown_height = slash_menu_dropdown_height(slash_menu.options.len());
            let sections = Layout::default()
                .constraints([Constraint::Length(dropdown_height), Constraint::Min(0)])
                .split(area);

            Self::render_slash_dropdown(f, sections[0], slash_menu);
            self.render_input(f, sections[1]);

            return;
        }

        self.render_input(f, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        let start = usize::from(row) * usize::from(width);
        let end = start + usize::from(width);

        buffer.content()[start..end]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_builder_methods() {
        // Arrange
        let title = "Chat";
        let input = "Hello";
        let cursor = 5;
        let placeholder = "Start typing...";
        let slash_menu = SlashMenu {
            options: vec![],
            selected_index: 0,
            title: "Menu",
        };

        // Act
        let chat_input = ChatInput::new(title, input, cursor)
            .placeholder(placeholder)
            .slash_menu(slash_menu);

        // Assert
        assert_eq!(chat_input.title, title);
        assert_eq!(chat_input.input, input);
        assert_eq!(chat_input.cursor, cursor);
        assert_eq!(chat_input.placeholder, placeholder);
        assert!(chat_input.slash_menu.is_some());
        assert_eq!(
            chat_input
                .slash_menu
                .expect("slash menu should be set")
                .title,
            "Menu"
        );
    }

    #[test]
    fn test_total_viewport_line_count_uses_cursor_row_when_cursor_is_below_last_display_line() {
        // Arrange
        let display_line_count = 1;
        let cursor_y = 1;

        // Act
        let total_line_count = ChatInput::total_viewport_line_count(display_line_count, cursor_y);

        // Assert
        assert_eq!(total_line_count, 2);
    }

    #[test]
    fn test_render_uses_rounded_focused_frame_for_prompt_input() {
        // Arrange
        let width = 32;
        let backend = ratatui::backend::TestBackend::new(width, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let chat_input = ChatInput::new("Prompt", "", 0).placeholder("Type your message");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                chat_input.render(frame, area);
            })
            .expect("failed to draw prompt input");

        // Assert
        let top_row = buffer_row_text(terminal.backend().buffer(), 0, width);
        assert!(top_row.starts_with("╭"));
        assert!(top_row.contains(" Prompt "));
        assert!(top_row.contains("╮"));
    }

    #[test]
    fn test_render_uses_matching_rounded_dropdown_frame() {
        // Arrange
        let width = 40;
        let backend = ratatui::backend::TestBackend::new(width, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let slash_menu = SlashMenu {
            options: vec![SlashMenuOption {
                description: "Choose a model".to_string(),
                label: "/model".to_string(),
            }],
            selected_index: 0,
            title: "Slash Command",
        };
        let chat_input = ChatInput::new("Prompt", "/", 1).slash_menu(slash_menu);

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                chat_input.render(frame, area);
            })
            .expect("failed to draw prompt input with dropdown");

        // Assert
        let top_row = buffer_row_text(terminal.backend().buffer(), 0, width);
        assert!(top_row.starts_with("╭"));
        assert!(top_row.contains(" Slash Command "));
        assert!(top_row.contains("╮"));
    }
}
