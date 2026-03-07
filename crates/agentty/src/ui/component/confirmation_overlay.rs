use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::ui::style::palette;
use crate::ui::text_util::truncate_with_ellipsis;
use crate::ui::{Component, overlay};

const MIN_OVERLAY_HEIGHT: u16 = 7;
const MIN_OVERLAY_WIDTH: u16 = 30;
const OVERLAY_HEIGHT_PERCENT: u16 = 20;
const OVERLAY_WIDTH_PERCENT: u16 = 40;

/// Centered confirmation popup used for destructive actions.
///
/// The message body is truncated to a single visible line so confirmation
/// choices remain visible even when session titles are very long.
pub struct ConfirmationOverlay<'a> {
    message: &'a str,
    selected_yes: bool,
    title: &'a str,
}

impl<'a> ConfirmationOverlay<'a> {
    /// Creates a confirmation popup with title and body message.
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self {
            message,
            selected_yes: false,
            title,
        }
    }

    /// Sets whether the "Yes" option is currently selected.
    #[must_use]
    pub fn selected_yes(mut self, yes: bool) -> Self {
        self.selected_yes = yes;
        self
    }
}

impl Component for ConfirmationOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            OVERLAY_HEIGHT_PERCENT,
            MIN_OVERLAY_WIDTH,
            MIN_OVERLAY_HEIGHT,
        );
        let message_width = overlay::overlay_content_width(popup_area.width);
        let message = truncate_with_ellipsis(self.message, message_width);

        let selected_option_style = Style::default()
            .fg(palette::SURFACE_OVERLAY)
            .bg(palette::ACCENT)
            .add_modifier(Modifier::BOLD);
        let unselected_option_style = Style::default().fg(palette::TEXT);
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
            Line::from(Span::styled(message, Style::default().fg(palette::TEXT))),
            Line::from(""),
            Line::from(vec![
                Span::styled(" Yes ", yes_option_style),
                Span::styled("   ", Style::default()),
                Span::styled(" No ", no_option_style),
            ]),
        ])
        .alignment(Alignment::Center)
        .block(overlay::overlay_block(self.title, palette::WARNING));

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
        let selected_yes = false;
        let title = "Confirm";

        // Act
        let overlay = ConfirmationOverlay::new(title, message).selected_yes(selected_yes);

        // Assert
        assert_eq!(overlay.message, message);
        assert_eq!(overlay.selected_yes, selected_yes);
        assert_eq!(overlay.title, title);
    }

    #[test]
    fn test_confirmation_overlay_render_hides_bottom_navigation_hints() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let overlay =
            ConfirmationOverlay::new("Confirm Delete", "Delete session?").selected_yes(false);

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&overlay, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("Yes"));
        assert!(text.contains("No"));
        assert!(!text.contains("Left/Right"));
        assert!(!text.contains(": choose"));
        assert!(!text.contains(": select"));
    }

    #[test]
    fn test_confirmation_overlay_render_preserves_choices_for_long_message() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let message = "Delete session \"session with a very long name that keeps going and would \
                       otherwise hide choices in the confirmation popup\"?";
        let overlay = ConfirmationOverlay::new("Confirm Delete", message).selected_yes(false);

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&overlay, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("Yes"));
        assert!(text.contains("No"));
        assert!(text.contains("..."));
    }
}
