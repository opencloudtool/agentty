use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::health::{HealthEntry, HealthStatus};
use crate::ui::Page;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct HealthPage<'a> {
    health_checks: &'a Arc<Mutex<Vec<HealthEntry>>>,
}

impl<'a> HealthPage<'a> {
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

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let frame_idx = (now / 100) as usize % SPINNER_FRAMES.len();

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(""));

        for entry in &entries {
            let (icon, icon_color) = match entry.status {
                HealthStatus::Pending => ("·", Color::DarkGray),
                HealthStatus::Running => (SPINNER_FRAMES[frame_idx], Color::Cyan),
                HealthStatus::Pass => ("✓", Color::Green),
                HealthStatus::Warn => ("!", Color::Yellow),
                HealthStatus::Fail => ("✗", Color::Red),
            };

            let label = format!("{:<18}", entry.kind.label());
            let message = if entry.message.is_empty() {
                String::new()
            } else {
                entry.message.clone()
            };

            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::raw("  "),
                Span::styled(label, Style::default().fg(Color::White)),
                Span::styled(message, Style::default().fg(Color::Gray)),
            ]));
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
