use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::agent::AgentKind;
use crate::ui::Component;

pub struct StatusBar {
    agent_kind: AgentKind,
}

impl StatusBar {
    pub fn new(agent_kind: AgentKind) -> Self {
        Self { agent_kind }
    }
}

impl Component for StatusBar {
    fn render(&self, f: &mut Frame, area: Rect) {
        let version = env!("CARGO_PKG_VERSION");
        let left_text = Span::styled(
            format!(" Agentty v{version}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        let right_text = format!("Agent: {} ", self.agent_kind);
        let left_width = u16::try_from(left_text.width()).unwrap_or(u16::MAX);
        let right_width = u16::try_from(right_text.len()).unwrap_or(u16::MAX);
        let padding = area
            .width
            .saturating_sub(left_width.saturating_add(right_width));
        let status_bar = Paragraph::new(Line::from(vec![
            left_text,
            Span::raw(" ".repeat(padding as usize)),
            Span::styled(right_text, Style::default().fg(Color::Gray)),
        ]))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        f.render_widget(status_bar, area);
    }
}
