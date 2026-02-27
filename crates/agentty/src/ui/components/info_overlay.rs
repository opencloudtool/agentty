use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::ui::Component;
use crate::ui::icon::Icon;

const MIN_OVERLAY_HEIGHT: u16 = 7;
const MIN_OVERLAY_WIDTH: u16 = 38;
const OVERLAY_HEIGHT_PERCENT: u16 = 20;
const OVERLAY_WIDTH_PERCENT: u16 = 45;

/// Centered informational popup used for non-destructive workflow guidance.
pub struct InfoOverlay<'a> {
    is_loading: bool,
    message: &'a str,
    title: &'a str,
}

impl<'a> InfoOverlay<'a> {
    /// Creates an informational popup with title and body message.
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self {
            is_loading: false,
            message,
            title,
        }
    }

    /// Sets whether the overlay should display a loading indicator.
    #[must_use]
    pub fn is_loading(mut self, loading: bool) -> Self {
        self.is_loading = loading;
        self
    }

    /// Splits the body message into styled lines and preserves explicit
    /// newline breaks.
    fn message_lines(&self) -> Vec<Line<'a>> {
        let mut message_lines = self
            .message
            .lines()
            .map(|message_line| {
                Line::from(Span::styled(
                    message_line,
                    Style::default().fg(Color::White),
                ))
            })
            .collect::<Vec<_>>();

        if message_lines.is_empty() {
            message_lines.push(Line::from(""));
        }

        message_lines
    }
}

impl Component for InfoOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let width = (area.width * OVERLAY_WIDTH_PERCENT / 100)
            .max(MIN_OVERLAY_WIDTH)
            .min(area.width);
        let height = (area.height * OVERLAY_HEIGHT_PERCENT / 100)
            .max(MIN_OVERLAY_HEIGHT)
            .min(area.height);
        let popup_area = Rect::new(
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        );

        let title = format!(" {} ", self.title);
        let mut paragraph_lines = self.message_lines();

        if self.is_loading {
            let loading_line = format!("{} Sync in progress...", Icon::current_spinner());

            paragraph_lines.push(Line::from(""));
            paragraph_lines.push(Line::from(Span::styled(
                loading_line,
                Style::default().fg(Color::Cyan),
            )));
        } else {
            let ok_style = Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD);

            paragraph_lines.push(Line::from(""));
            paragraph_lines.push(Line::from(Span::styled(" OK ", ok_style)));
        }

        let paragraph = Paragraph::new(paragraph_lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow))
                    .title(Span::styled(title, Style::default().fg(Color::Yellow))),
            );

        f.render_widget(Clear, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_info_overlay_new_stores_fields() {
        // Arrange
        let message = "Sync is blocked";
        let title = "Sync blocked";

        // Act
        let overlay = InfoOverlay::new(title, message);

        // Assert
        assert!(!overlay.is_loading);
        assert_eq!(overlay.message, message);
        assert_eq!(overlay.title, title);
    }

    #[test]
    fn test_info_overlay_render_includes_ok_indicator() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let overlay = InfoOverlay::new("Sync blocked", "Main has uncommitted changes");

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                crate::ui::Component::render(&overlay, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("OK"));
        assert!(text.contains("Main has uncommitted changes"));
    }

    #[test]
    fn test_info_overlay_render_includes_loading_indicator_when_loading() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let overlay = InfoOverlay::new("Sync in progress", "Synchronizing branch").is_loading(true);

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                crate::ui::Component::render(&overlay, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("Sync in progress..."));
        assert!(!text.contains("OK"));
    }

    #[test]
    fn test_message_lines_keeps_each_sentence_on_its_own_line() {
        // Arrange
        let overlay = InfoOverlay::new(
            "Sync blocked",
            "Sync cannot run on this branch.\nCommit or stash, then retry.",
        );

        // Act
        let message_lines = overlay.message_lines();

        // Assert
        assert_eq!(message_lines.len(), 2);
        assert_eq!(
            message_lines[0].spans[0].content.as_ref(),
            "Sync cannot run on this branch."
        );
        assert_eq!(
            message_lines[1].spans[0].content.as_ref(),
            "Commit or stash, then retry."
        );
    }
}
