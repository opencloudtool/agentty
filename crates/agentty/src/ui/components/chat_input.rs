use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::Component;
use crate::ui::util::{
    CHAT_INPUT_MAX_VISIBLE_LINES, calculate_input_viewport, compute_input_layout,
};

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
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let description_style = if is_selected {
                    Style::default().fg(Color::Gray)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                Line::from(vec![
                    Span::styled(format!("{prefix} {}", option.label), label_style),
                    Span::styled(format!("  {}", option.description), description_style),
                ])
            })
            .collect::<Vec<_>>();

        let dropdown = Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Span::styled(
                    slash_menu.title,
                    Style::default().fg(Color::Cyan),
                )),
        );

        f.render_widget(Clear, area);
        f.render_widget(dropdown, area);
    }

    /// Render the prompt input with an internally scrollable viewport.
    fn render_input(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(self.title, Style::default().fg(Color::Cyan)));

        if self.input.is_empty() {
            let prefix = " › ";
            let display_lines = vec![Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(self.placeholder, Style::default().fg(Color::DarkGray)),
            ])];

            let widget = Paragraph::new(display_lines).block(block);
            f.render_widget(Clear, area);
            f.render_widget(widget, area);

            f.set_cursor_position((area.x.saturating_add(1 + 3), area.y.saturating_add(1)));

            return;
        }

        let (display_lines, cursor_x, cursor_y) =
            compute_input_layout(self.input, area.width, self.cursor);
        let viewport_height = area
            .height
            .saturating_sub(2)
            .min(CHAT_INPUT_MAX_VISIBLE_LINES);
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(display_lines.len(), cursor_y, viewport_height);
        let widget = Paragraph::new(display_lines)
            .scroll((scroll_offset, 0))
            .block(block);

        f.render_widget(Clear, area);
        f.render_widget(widget, area);
        f.set_cursor_position((
            area.x.saturating_add(1).saturating_add(cursor_x),
            area.y.saturating_add(1).saturating_add(cursor_row),
        ));
    }
}

impl Component for ChatInput<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        if let Some(slash_menu) = &self.slash_menu {
            let dropdown_height = u16::try_from(slash_menu.options.len())
                .unwrap_or(u16::MAX)
                .saturating_add(2);
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
}
