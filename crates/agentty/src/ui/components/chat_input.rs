use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::Component;
use crate::ui::util::compute_input_layout;

pub struct ChatInput<'a> {
    pub placeholder: &'a str,
    cursor: usize,
    input: &'a str,
    title: &'a str,
}

impl<'a> ChatInput<'a> {
    pub fn new(title: &'a str, input: &'a str, cursor: usize, placeholder: &'a str) -> Self {
        Self {
            placeholder,
            cursor,
            input,
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
