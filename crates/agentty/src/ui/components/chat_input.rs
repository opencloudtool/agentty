use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::Component;
use crate::ui::util::compute_input_layout;

pub struct ChatInput<'a> {
    title: &'a str,
    input: &'a str,
}

impl<'a> ChatInput<'a> {
    pub fn new(title: &'a str, input: &'a str) -> Self {
        Self { title, input }
    }
}

impl Component for ChatInput<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let (display_lines, cursor_x, cursor_y) = compute_input_layout(self.input, area.width);

        let input_widget = Paragraph::new(display_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(self.title, Style::default().fg(Color::Cyan))),
        );
        f.render_widget(Clear, area);
        f.render_widget(input_widget, area);

        // Set cursor position
        f.set_cursor_position((
            area.x.saturating_add(1).saturating_add(cursor_x),
            area.y.saturating_add(1).saturating_add(cursor_y),
        ));
    }
}
