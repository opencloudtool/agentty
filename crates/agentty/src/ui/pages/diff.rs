use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::model::Session;
use crate::ui::Page;
use crate::ui::util::wrap_lines;

pub struct DiffPage<'a> {
    pub diff: String,
    pub scroll_offset: u16,
    pub session: &'a Session,
}

impl<'a> DiffPage<'a> {
    pub fn new(session: &'a Session, diff: String, scroll_offset: u16) -> Self {
        Self {
            diff,
            scroll_offset,
            session,
        }
    }
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let output_area = chunks[0];
        let footer_area = chunks[1];

        let title = format!(" Diff — {} ", self.session.display_title());

        let inner_width = output_area.width.saturating_sub(2) as usize;
        let mut lines = Vec::new();

        for line in self.diff.lines() {
            let style = if line.starts_with('+') && !line.starts_with("+++") {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') && !line.starts_with("---") {
                Style::default().fg(Color::Red)
            } else if line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else if line.starts_with("diff")
                || line.starts_with("index")
                || line.starts_with("---")
                || line.starts_with("+++")
            {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Gray)
            };

            let wrapped = wrap_lines(line, inner_width);
            for w in wrapped {
                lines.push(Line::from(vec![Span::styled(w.to_string(), style)]));
            }
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(title, Style::default().fg(Color::Yellow))),
            )
            .scroll((self.scroll_offset, 0));

        f.render_widget(paragraph, output_area);

        let help_message =
            Paragraph::new("q: back | j/k: scroll").style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}
