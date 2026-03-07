use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::ui::state::app_mode::HelpContext;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const OVERLAY_WIDTH_PERCENT: u16 = 40;

const OVERLAY_HEIGHT_PERCENT: u16 = 60;

const MIN_OVERLAY_WIDTH: u16 = 30;

const MIN_OVERLAY_HEIGHT: u16 = 10;

const SCROLL_X_OFFSET: u16 = 0;

/// Centered popup overlay showing keybindings for the current page.
pub struct HelpOverlay<'a> {
    context: &'a HelpContext,

    scroll_offset: u16,
}

impl<'a> HelpOverlay<'a> {
    /// Creates a help overlay for the given context.
    pub fn new(context: &'a HelpContext) -> Self {
        Self {
            context,
            scroll_offset: 0,
        }
    }

    /// Sets the vertical scroll offset.
    #[must_use]
    pub fn scroll_offset(mut self, offset: u16) -> Self {
        self.scroll_offset = offset;
        self
    }
}

impl Component for HelpOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = popup_area(area);

        f.render_widget(Clear, popup_area);

        let bindings = self.context.keybindings();

        let key_width = bindings
            .iter()
            .map(|binding| binding.key.len())
            .max()
            .unwrap_or(0);

        let max_content_width = bindings
            .iter()
            .map(|binding| 1 + key_width + 2 + binding.popup_label.len())
            .max()
            .unwrap_or(0);

        let content_width = overlay::overlay_content_width(popup_area.width);
        let left_padding = content_width.saturating_sub(max_content_width) / 2;

        let indent = " ".repeat(left_padding);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(bindings.len());

        for binding in bindings {
            lines.push(Line::from(vec![
                Span::raw(indent.clone()),
                Span::raw(" "),
                Span::styled(
                    format!("{:>key_width$}", binding.key),
                    Style::default()
                        .fg(palette::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": ", Style::default().fg(palette::TEXT)),
                Span::styled(binding.popup_label, Style::default().fg(palette::TEXT)),
            ]));
        }

        let paragraph = Paragraph::new(lines)
            .block(overlay::overlay_block(
                self.context.title(),
                palette::ACCENT,
            ))
            .scroll((self.scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, popup_area);
    }
}

/// Computes a centered rectangle within the given `area`.
fn popup_area(area: Rect) -> Rect {
    overlay::centered_popup_area(
        area,
        OVERLAY_WIDTH_PERCENT,
        OVERLAY_HEIGHT_PERCENT,
        MIN_OVERLAY_WIDTH,
        MIN_OVERLAY_HEIGHT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::help_action::HelpAction;

    #[test]

    fn test_popup_area_centers_within_area() {
        // Arrange

        let area = Rect::new(0, 0, 100, 50);

        // Act

        let popup = popup_area(area);

        // Assert

        assert_eq!(popup.width, 40);

        assert_eq!(popup.height, 30);

        assert_eq!(popup.x, 30);

        assert_eq!(popup.y, 10);
    }

    #[test]

    fn test_popup_area_clamps_to_area_when_small() {
        // Arrange

        let area = Rect::new(0, 0, 20, 8);

        // Act

        let popup = popup_area(area);

        // Assert — min sizes clamped to area

        assert_eq!(popup.width, 20);

        assert_eq!(popup.height, 8);
    }

    #[test]

    fn test_popup_area_respects_minimum_dimensions() {
        // Arrange

        let area = Rect::new(0, 0, 40, 20);

        // Act

        let popup = popup_area(area);

        // Assert — 40% of 40=16 < MIN 30, so width = 30; 60% of 20=12 >= MIN 10

        assert_eq!(popup.width, 30);

        assert_eq!(popup.height, 12);
    }

    #[test]
    fn test_help_overlay_new_stores_fields() {
        // Arrange
        let context = HelpContext::List {
            keybindings: vec![HelpAction::new("quit", "q", "Quit")],
        };

        // Act
        let overlay = HelpOverlay::new(&context).scroll_offset(5);

        // Assert
        assert_eq!(overlay.scroll_offset, 5);
    }
}
