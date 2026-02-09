use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::Component;
use crate::ui::util::compute_input_layout;

/// Inline slash-command menu rendered inside the prompt input block.
pub struct SlashMenu<'a> {
    pub options: Vec<String>,
    pub selected_index: usize,
    pub title: &'a str,
}

pub struct ChatInput<'a> {
    pub placeholder: &'a str,
    cursor: usize,
    input: &'a str,
    slash_menu: Option<SlashMenu<'a>>,
    title: &'a str,
}

impl<'a> ChatInput<'a> {
    pub fn new(
        title: &'a str,
        input: &'a str,
        cursor: usize,
        placeholder: &'a str,
        slash_menu: Option<SlashMenu<'a>>,
    ) -> Self {
        Self {
            placeholder,
            cursor,
            input,
            slash_menu,
            title,
        }
    }
}

impl Component for ChatInput<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(self.title, Style::default().fg(Color::Cyan)));

        if let Some(slash_menu) = &self.slash_menu {
            let title_line_count = usize::from(!slash_menu.title.is_empty());
            let mut display_lines = Vec::new();

            if !slash_menu.title.is_empty() {
                display_lines.push(Line::from(Span::styled(
                    slash_menu.title,
                    Style::default().fg(Color::DarkGray),
                )));
            }

            display_lines.extend(
                slash_menu
                    .options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| {
                        let is_selected = index == slash_menu.selected_index;
                        let prefix = if is_selected { ">" } else { " " };
                        let style = if is_selected {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        };

                        Line::from(Span::styled(format!("{prefix} {option}"), style))
                    }),
            );

            display_lines.push(Line::from(vec![
                Span::styled(
                    " › ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(self.input),
            ]));

            let widget = Paragraph::new(display_lines).block(block);
            f.render_widget(Clear, area);
            f.render_widget(widget, area);

            let cursor_x = area
                .x
                .saturating_add(1 + 3)
                .saturating_add(u16::try_from(self.input.chars().count()).unwrap_or(0));
            let cursor_y = area
                .y
                .saturating_add(u16::try_from(slash_menu.options.len()).unwrap_or(0))
                .saturating_add(u16::try_from(title_line_count).unwrap_or(0))
                .saturating_add(1);
            f.set_cursor_position((cursor_x, cursor_y));

            return;
        }

        if self.input.is_empty() {
            let prefix = " › ";
            let display_lines = vec![Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(self.placeholder, Style::default().fg(Color::DarkGray)),
            ])];

            let widget = Paragraph::new(display_lines).block(block);
            f.render_widget(Clear, area);
            f.render_widget(widget, area);

            f.set_cursor_position((area.x.saturating_add(1 + 3), area.y.saturating_add(1)));
        } else {
            let (display_lines, cursor_x, cursor_y) =
                compute_input_layout(self.input, area.width, self.cursor);

            let widget = Paragraph::new(display_lines).block(block);
            f.render_widget(Clear, area);
            f.render_widget(widget, area);

            f.set_cursor_position((
                area.x.saturating_add(1).saturating_add(cursor_x),
                area.y.saturating_add(1).saturating_add(cursor_y),
            ));
        }
    }
}
