use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::Component;

pub struct StatusBar;

impl Component for StatusBar {
    fn render(&self, f: &mut Frame, area: Rect) {
        let version = env!("CARGO_PKG_VERSION");
        let status_bar = Paragraph::new(Line::from(Span::styled(
            format!(" Agentty v{version}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        f.render_widget(status_bar, area);
    }
}
