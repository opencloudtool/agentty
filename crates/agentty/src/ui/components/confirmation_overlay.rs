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
    selected_yes: bool,
    title: &'a str,
}

impl<'a> ConfirmationOverlay<'a> {
    /// Creates a confirmation popup with title and body message.
    pub fn new(title: &'a str, message: &'a str, selected_yes: bool) -> Self {
        Self {
            message,
            selected_yes,
            title,
        }
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
        let selected_option_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let unselected_option_style = Style::default().fg(Color::White);
        let yes_option_style = if self.selected_yes {
            selected_option_style
        } else {
            unselected_option_style
        };
        let no_option_style = if self.selected_yes {
            unselected_option_style
        } else {
            selected_option_style
        };

        let paragraph = Paragraph::new(vec![
            Line::from(Span::styled(
                self.message,
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(" Yes ", yes_option_style),
                Span::styled("   ", Style::default()),
                Span::styled(" No ", no_option_style),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "y",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": yes  ", Style::default().fg(Color::White)),
                Span::styled(
                    "n",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": no  ", Style::default().fg(Color::White)),
                Span::styled(
                    "Left/Right",
                    Style::default()
                        .fg(Color::LightMagenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": choose  ", Style::default().fg(Color::White)),
                Span::styled(
                    "Enter",
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": select", Style::default().fg(Color::White)),
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
        let selected_yes = true;
        let title = "Confirm";

        // Act
        let overlay = ConfirmationOverlay::new(title, message, selected_yes);

        // Assert
        assert_eq!(overlay.message, message);
        assert_eq!(overlay.selected_yes, selected_yes);
        assert_eq!(overlay.title, title);
    }
}
