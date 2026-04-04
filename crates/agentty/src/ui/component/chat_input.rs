use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::ui::util::{
    CHAT_INPUT_MAX_VISIBLE_LINES, calculate_input_viewport, compute_input_layout,
    input_cursor_position, placeholder_cursor_position, suggestion_dropdown_height,
};
use crate::ui::{Component, style};

/// One row rendered inside a prompt suggestion dropdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuggestionItem {
    /// Optional compact badge rendered before the main label.
    pub badge: Option<String>,
    /// Optional explanatory text rendered after the label.
    pub detail: Option<String>,
    /// Primary row label used for selection and insertion.
    pub label: String,
    /// Optional trailing metadata rendered with subdued styling.
    pub metadata: Option<String>,
}

/// Suggestion dropdown rendered above or alongside the prompt input block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuggestionList {
    /// Dropdown rows in display order.
    pub items: Vec<SuggestionItem>,
    /// Highlighted row index in `items`.
    pub selected_index: usize,
    /// Dropdown title shown in the rounded border chrome.
    pub title: String,
}

/// Prompt input component with optional rich suggestion dropdown.
pub struct ChatInput<'a> {
    pub placeholder: &'a str,
    active: bool,
    cursor: usize,
    input: &'a str,
    suggestion_list: Option<&'a SuggestionList>,
    title: &'a str,
}

impl<'a> ChatInput<'a> {
    /// Creates a new prompt input component.
    pub fn new(title: &'a str, input: &'a str, cursor: usize) -> Self {
        Self {
            placeholder: "",
            active: true,
            cursor,
            input,
            suggestion_list: None,
            title,
        }
    }

    /// Sets the input placeholder text.
    #[must_use]
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// Marks the input as inactive (dimmed border, no cursor).
    ///
    /// When `false`, the border uses a muted color and the terminal cursor
    /// is not rendered. Defaults to `true`.
    #[must_use]
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Sets the suggestion dropdown shown next to the prompt input.
    #[must_use]
    pub fn suggestion_list(mut self, suggestion_list: &'a SuggestionList) -> Self {
        self.suggestion_list = Some(suggestion_list);
        self
    }

    /// Returns the shared block styling for the prompt input frame.
    ///
    /// Uses accent styling when active and muted styling when inactive.
    fn input_block(&self) -> Block<'a> {
        let title = format!(" {} ", self.title);
        let (border_style, title_style) = if self.active {
            (Self::focused_border_style(), Self::focused_title_style())
        } else {
            (Self::inactive_border_style(), Self::inactive_title_style())
        };

        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(Span::styled(title, title_style))
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

    /// Returns the border style for an inactive (dimmed) prompt input frame.
    fn inactive_border_style() -> Style {
        Style::default().fg(style::palette::BORDER)
    }

    /// Returns the title style for an inactive (dimmed) prompt input frame.
    fn inactive_title_style() -> Style {
        Style::default().fg(style::palette::BORDER)
    }

    /// Returns the shared block styling for prompt suggestion dropdowns.
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

    /// Renders the suggestion dropdown using the shared chat input chrome.
    ///
    /// This method is also used by the question-mode panel to render the
    /// at-mention file dropdown as an overlay above the input area.
    pub(crate) fn render_suggestion_dropdown(
        f: &mut Frame,
        area: Rect,
        suggestion_list: &SuggestionList,
    ) {
        let rows = suggestion_list
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let is_selected = index == suggestion_list.selected_index;
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

                let mut spans = Vec::new();
                spans.push(Span::styled(format!("{prefix} "), label_style));

                if let Some(badge) = &item.badge {
                    spans.push(Span::styled(format!("[{badge}] "), description_style));
                }

                spans.push(Span::styled(item.label.as_str(), label_style));

                if let Some(metadata) = &item.metadata {
                    spans.push(Span::styled(format!("  {metadata}"), description_style));
                }

                if let Some(detail) = &item.detail {
                    spans.push(Span::styled(format!("  {detail}"), description_style));
                }

                Line::from(spans)
            })
            .collect::<Vec<_>>();

        let dropdown = Paragraph::new(rows).block(Self::dropdown_block(&suggestion_list.title));

        f.render_widget(Clear, area);
        f.render_widget(dropdown, area);
    }

    /// Render the prompt input with an internally scrollable viewport.
    fn render_input(&self, f: &mut Frame, area: Rect) {
        let block = self.input_block();

        if self.input.is_empty() {
            let prefix_style = if self.active {
                Style::default()
                    .fg(style::palette::ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(style::palette::BORDER)
            };
            let prefix = " › ";
            let display_lines = vec![Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::raw("  "),
                Span::styled(
                    self.placeholder,
                    Style::default().fg(style::palette::TEXT_SUBTLE),
                ),
            ])];

            let widget = Paragraph::new(display_lines).block(block);
            f.render_widget(Clear, area);
            f.render_widget(widget, area);
            if self.active {
                f.set_cursor_position(placeholder_cursor_position(area));
            }

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
        if self.active {
            f.set_cursor_position(input_cursor_position(area, cursor_x, cursor_row));
        }
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
        if let Some(suggestion_list) = &self.suggestion_list {
            let dropdown_height = suggestion_dropdown_height(suggestion_list.items.len());
            let sections = Layout::default()
                .constraints([Constraint::Length(dropdown_height), Constraint::Min(0)])
                .split(area);

            Self::render_suggestion_dropdown(f, sections[0], suggestion_list);
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
        let suggestion_list = SuggestionList {
            items: vec![],
            selected_index: 0,
            title: "Menu".to_string(),
        };

        // Act
        let chat_input = ChatInput::new(title, input, cursor)
            .placeholder(placeholder)
            .suggestion_list(&suggestion_list);

        // Assert
        assert_eq!(chat_input.title, title);
        assert_eq!(chat_input.input, input);
        assert_eq!(chat_input.cursor, cursor);
        assert_eq!(chat_input.placeholder, placeholder);
        assert!(chat_input.suggestion_list.is_some());
        assert_eq!(
            chat_input
                .suggestion_list
                .expect("suggestion list should be set")
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
    fn test_render_inactive_uses_dimmed_border_style() {
        // Arrange
        let width = 32;
        let backend = ratatui::backend::TestBackend::new(width, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let chat_input = ChatInput::new("Prompt", "", 0)
            .placeholder("Type your message")
            .active(false);

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                chat_input.render(frame, area);
            })
            .expect("failed to draw inactive prompt input");

        // Assert — border is rendered with the muted BORDER color, not ACCENT.
        let buffer = terminal.backend().buffer();
        let top_left_cell = &buffer.content()[0];
        assert_eq!(top_left_cell.fg, style::palette::BORDER);
    }

    #[test]
    fn test_render_inactive_with_text_uses_dimmed_border() {
        // Arrange — inactive input with text still uses the muted border.
        let width = 32;
        let backend = ratatui::backend::TestBackend::new(width, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let chat_input = ChatInput::new("Prompt", "hello", 5).active(false);

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                chat_input.render(frame, area);
            })
            .expect("failed to draw inactive prompt input with text");

        // Assert — border still uses muted BORDER color.
        let buffer = terminal.backend().buffer();
        let top_left_cell = &buffer.content()[0];
        assert_eq!(top_left_cell.fg, style::palette::BORDER);
    }

    #[test]
    fn test_render_uses_matching_rounded_dropdown_frame() {
        // Arrange
        let width = 40;
        let backend = ratatui::backend::TestBackend::new(width, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let suggestion_list = SuggestionList {
            items: vec![SuggestionItem {
                badge: Some("cmd".to_string()),
                detail: Some("Choose a model".to_string()),
                label: "/model".to_string(),
                metadata: Some("Enter".to_string()),
            }],
            selected_index: 0,
            title: "Prompt Suggestion".to_string(),
        };
        let chat_input = ChatInput::new("Prompt", "/", 1).suggestion_list(&suggestion_list);

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
        assert!(top_row.contains(" Prompt Suggestion "));
        assert!(top_row.contains("╮"));
    }

    #[test]
    fn test_render_keeps_raw_at_lookup_text_visible_in_input() {
        // Arrange
        let width = 48;
        let backend = ratatui::backend::TestBackend::new(width, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let chat_input = ChatInput::new("Prompt", "@src/main.rs", "@src/main.rs".chars().count());

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                chat_input.render(frame, area);
            })
            .expect("failed to draw prompt input with at-lookup");

        // Assert
        let visible_text = (0..5)
            .map(|row| buffer_row_text(terminal.backend().buffer(), row, width))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(visible_text.contains("@src/main.rs"));
    }
}
