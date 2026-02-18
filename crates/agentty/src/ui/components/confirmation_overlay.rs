use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::ui::Component;

const MIN_OVERLAY_HEIGHT: u16 = 7;
const MIN_OVERLAY_WIDTH: u16 = 44;
const OVERLAY_HEIGHT_PERCENT: u16 = 30;
const OVERLAY_WIDTH_PERCENT: u16 = 55;

/// Centered confirmation popup used for destructive actions.
pub struct ConfirmationOverlay<'a> {
    message: &'a str,
    title: &'a str,
}

impl<'a> ConfirmationOverlay<'a> {
    /// Creates a confirmation popup with title and body message.
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self { message, title }
    }
}

impl Component for ConfirmationOverlay<'_> {
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

        let paragraph = Paragraph::new(vec![
            Line::from(Span::styled(
                self.message,
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Enter / y",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": confirm  ", Style::default().fg(Color::White)),
                Span::styled(
                    "n / Esc",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": cancel", Style::default().fg(Color::White)),
            ]),
        ])
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
    fn test_confirmation_overlay_new_stores_fields() {
        // Arrange
        let message = "Delete session?";
        let title = "Confirm";

        // Act
        let overlay = ConfirmationOverlay::new(title, message);

        // Assert
        assert_eq!(overlay.message, message);
        assert_eq!(overlay.title, title);
    }
}
