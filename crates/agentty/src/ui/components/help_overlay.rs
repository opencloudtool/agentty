use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::model::HelpContext;
use crate::ui::Component;

const OVERLAY_WIDTH_PERCENT: u16 = 60;
const OVERLAY_HEIGHT_PERCENT: u16 = 60;
const MIN_OVERLAY_WIDTH: u16 = 30;
const MIN_OVERLAY_HEIGHT: u16 = 10;
const BORDER_SIZE: u16 = 2;
const SCROLL_X_OFFSET: u16 = 0;

/// Centered popup overlay showing keybindings for the current page.
pub struct HelpOverlay<'a> {
    context: &'a HelpContext,
    scroll_offset: u16,
}

impl<'a> HelpOverlay<'a> {
    /// Creates a help overlay for the given context and scroll position.
    pub fn new(context: &'a HelpContext, scroll_offset: u16) -> Self {
        Self {
            context,
            scroll_offset,
        }
    }
}

impl Component for HelpOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = centered_rect(area);

        f.render_widget(Clear, popup_area);

        let title = format!(" {} ", self.context.title());
        let bindings = self.context.keybindings();

        let key_width = bindings.iter().map(|(key, _)| key.len()).max().unwrap_or(0);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(bindings.len() + BORDER_SIZE as usize);
        lines.push(Line::from(""));

        for (key, description) in bindings {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{key:>key_width$}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": ", Style::default().fg(Color::White)),
                Span::styled(*description, Style::default().fg(Color::White)),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Press ? / q / Esc to close",
            Style::default().fg(Color::DarkGray),
        )));

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(Span::styled(title, Style::default().fg(Color::Cyan))),
            )
            .scroll((self.scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, popup_area);
    }
}

/// Computes a centered rectangle within the given `area`.
fn centered_rect(area: Rect) -> Rect {
    let popup_width = (area.width * OVERLAY_WIDTH_PERCENT / 100).max(MIN_OVERLAY_WIDTH);
    let popup_height = (area.height * OVERLAY_HEIGHT_PERCENT / 100).max(MIN_OVERLAY_HEIGHT);

    let width = popup_width.min(area.width);
    let height = popup_height.min(area.height);

    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;

    Rect::new(x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_rect_centers_within_area() {
        // Arrange
        let area = Rect::new(0, 0, 100, 50);

        // Act
        let popup = centered_rect(area);

        // Assert
        assert_eq!(popup.width, 60);
        assert_eq!(popup.height, 30);
        assert_eq!(popup.x, 20);
        assert_eq!(popup.y, 10);
    }

    #[test]
    fn test_centered_rect_clamps_to_area_when_small() {
        // Arrange
        let area = Rect::new(0, 0, 20, 8);

        // Act
        let popup = centered_rect(area);

        // Assert — min sizes clamped to area
        assert_eq!(popup.width, 20);
        assert_eq!(popup.height, 8);
    }

    #[test]
    fn test_centered_rect_respects_minimum_dimensions() {
        // Arrange
        let area = Rect::new(0, 0, 40, 20);

        // Act
        let popup = centered_rect(area);

        // Assert — 60% of 40=24 < MIN 30, so width = 30; 60% of 20=12 >= MIN 10
        assert_eq!(popup.width, 30);
        assert_eq!(popup.height, 12);
    }

    #[test]
    fn test_help_overlay_new_stores_fields() {
        // Arrange
        let context = HelpContext::List;

        // Act
        let overlay = HelpOverlay::new(&context, 5);

        // Assert
        assert_eq!(overlay.scroll_offset, 5);
    }
}
