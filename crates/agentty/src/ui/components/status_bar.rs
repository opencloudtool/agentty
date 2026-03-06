use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::{Component, style};

/// Top status bar showing current version and update availability.
pub struct StatusBar {
    current_version: String,
    latest_available_version: Option<String>,
}

impl StatusBar {
    /// Creates a status bar with the current version.
    pub fn new(current_version: String) -> Self {
        Self {
            current_version,
            latest_available_version: None,
        }
    }

    /// Sets the latest available version for update notification.
    #[must_use]
    pub fn latest_available_version(mut self, version: Option<String>) -> Self {
        self.latest_available_version = version;
        self
    }
}

impl Component for StatusBar {
    fn render(&self, f: &mut Frame, area: Rect) {
        let mut version_spans = vec![Span::styled(
            format!(" Agentty {}", self.current_version),
            Style::default()
                .fg(style::palette::ACCENT)
                .add_modifier(Modifier::BOLD),
        )];
        if let Some(latest_available_version) = &self.latest_available_version {
            version_spans.push(Span::raw(" | "));
            version_spans.push(Span::styled(
                format!(
                    "{latest_available_version} version available update with npm i -g \
                     agentty@latest"
                ),
                Style::default()
                    .fg(style::palette::WARNING)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let status_bar = Paragraph::new(Line::from(version_spans)).style(
            Style::default()
                .bg(style::palette::SURFACE)
                .fg(style::palette::TEXT),
        );
        f.render_widget(status_bar, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_bar_new_stores_versions() {
        // Arrange
        let current_version = "v0.1.12".to_string();
        let latest_available_version = Some("v0.1.13".to_string());

        // Act
        let status_bar = StatusBar::new(current_version.clone())
            .latest_available_version(latest_available_version.clone());

        // Assert
        assert_eq!(status_bar.current_version, current_version);
        assert_eq!(
            status_bar.latest_available_version,
            latest_available_version
        );
    }

    #[test]
    fn test_status_bar_render_shows_current_version_without_update() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string());

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Agentty v0.1.12"));
        assert!(!text.contains("version available update"));
    }

    #[test]
    fn test_status_bar_render_shows_update_notice_when_available() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let status_bar = StatusBar::new("v0.1.12".to_string())
            .latest_available_version(Some("v0.1.13".to_string()));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(&status_bar, frame, area);
            })
            .expect("failed to draw");

        // Assert
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Agentty v0.1.12"));
        assert!(text.contains("v0.1.13 version available update with npm i -g agentty@latest"));
    }
}
