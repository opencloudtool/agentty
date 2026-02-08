use std::sync::{Arc, Mutex};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::health::{HealthEntry, HealthStatus};
use crate::icon::Icon;
use crate::ui::Page;

/// Health page renderer for showing current health checks and statuses.
pub struct HealthPage<'a> {
    health_checks: &'a Arc<Mutex<Vec<HealthEntry>>>,
}

impl<'a> HealthPage<'a> {
    /// Creates a new health page bound to shared health-check entries.
    pub fn new(health_checks: &'a Arc<Mutex<Vec<HealthEntry>>>) -> Self {
        Self { health_checks }
    }
}

impl Page for HealthPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        let entries = self
            .health_checks
            .lock()
            .ok()
            .map(|lock| lock.clone())
            .unwrap_or_default();

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(""));

        for entry in &entries {
            lines.push(render_entry_line(entry, "   "));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Health ")
            .style(Style::default().fg(Color::White));

        let paragraph = Paragraph::new(lines).block(block);
        f.render_widget(paragraph, main_area);

        let help_message =
            Paragraph::new("q: back | r: rerun").style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

fn render_entry_line<'a>(entry: &HealthEntry, indent: &'a str) -> Line<'a> {
    let (icon, icon_color) = match entry.status {
        HealthStatus::Pending => (Icon::Pending, Color::DarkGray),
        HealthStatus::Running => (Icon::current_spinner(), Color::Cyan),
        HealthStatus::Pass => (Icon::Check, Color::Green),
        HealthStatus::Fail => (Icon::Cross, Color::Red),
    };

    let label = format!("{:<18}", entry.label);
    let message = if entry.message.is_empty() {
        String::new()
    } else {
        entry.message.clone()
    };

    Line::from(vec![
        Span::raw(indent),
        Span::styled(icon.as_str(), Style::default().fg(icon_color)),
        Span::raw("  "),
        Span::styled(label, Style::default().fg(Color::White)),
        Span::styled(message, Style::default().fg(Color::Gray)),
    ])
}
