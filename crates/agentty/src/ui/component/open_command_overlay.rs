use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::ui::style::palette;
use crate::ui::text_util::truncate_with_ellipsis;
use crate::ui::{Component, overlay};

const MIN_OVERLAY_HEIGHT: u16 = 9;
const MIN_OVERLAY_WIDTH: u16 = 50;
const OVERLAY_HEIGHT_PERCENT: u16 = 38;
const OVERLAY_WIDTH_PERCENT: u16 = 62;

/// Centered popup that allows selecting one configured open command.
pub struct OpenCommandOverlay<'a> {
    commands: &'a [String],
    selected_command_index: usize,
}

impl<'a> OpenCommandOverlay<'a> {
    /// Creates an open-command selector popup from configured command values.
    pub fn new(commands: &'a [String]) -> Self {
        Self {
            commands,
            selected_command_index: 0,
        }
    }

    /// Sets which command row is currently highlighted.
    #[must_use]
    pub fn selected_command_index(mut self, selected_command_index: usize) -> Self {
        self.selected_command_index = selected_command_index;
        self
    }

    /// Returns all render lines for this popup.
    ///
    /// The header and bottom help hint rows are centered, while the selected
    /// command row is emphasized by background color only (no prefix marker
    /// glyph).
    fn lines(&self, command_width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        lines.push(
            Line::from(vec![Span::styled(
                "Select open command",
                Style::default()
                    .fg(palette::WARNING)
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center),
        );
        lines.push(Line::from(""));

        for (index, command) in self.commands.iter().enumerate() {
            let is_selected = index == self.selected_command_index;
            let command_label = truncate_with_ellipsis(command, command_width);

            let line = if is_selected {
                let selected_label = format!(" {command_label:<command_width$}");
                Line::from(Span::styled(
                    selected_label,
                    Style::default()
                        .fg(palette::SURFACE_OVERLAY)
                        .bg(palette::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(" ", Style::default().fg(palette::TEXT_SUBTLE)),
                    Span::styled(command_label, Style::default().fg(palette::TEXT)),
                ])
            };

            lines.push(line);
        }

        lines.push(Line::from(""));
        lines.push(
            Line::from(vec![Span::styled(
                "j/k: move | Enter: open | Esc: cancel",
                Style::default().fg(palette::TEXT_MUTED),
            )])
            .alignment(Alignment::Center),
        );

        lines
    }

    /// Returns the centered popup rectangle constrained to terminal bounds.
    fn popup_area(area: Rect) -> Rect {
        overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            OVERLAY_HEIGHT_PERCENT,
            MIN_OVERLAY_WIDTH,
            MIN_OVERLAY_HEIGHT,
        )
    }
}

impl Component for OpenCommandOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = Self::popup_area(area);
        let command_width = overlay::overlay_content_width(popup_area.width)
            .saturating_sub(1)
            .max(1);
        let lines = self.lines(command_width);

        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(overlay::overlay_block("Open Command", palette::ACCENT));

        f.render_widget(Clear, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::style::palette;

    #[test]
    fn test_open_command_overlay_new_stores_default_selection() {
        // Arrange
        let commands = vec!["nvim .".to_string(), "npm run dev".to_string()];

        // Act
        let overlay = OpenCommandOverlay::new(&commands);

        // Assert
        assert_eq!(overlay.commands, commands.as_slice());
        assert_eq!(overlay.selected_command_index, 0);
    }

    #[test]
    fn test_open_command_overlay_popup_area_is_centered() {
        // Arrange
        let area = Rect::new(0, 0, 120, 40);

        // Act
        let popup_area = OpenCommandOverlay::popup_area(area);

        // Assert
        assert_eq!(popup_area.width, 74);
        assert_eq!(popup_area.height, 15);
        assert_eq!(popup_area.x, 23);
        assert_eq!(popup_area.y, 12);
    }

    #[test]
    fn test_open_command_overlay_render_contains_hint_text() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let commands = vec!["nvim .".to_string(), "npm run dev".to_string()];
        let overlay = OpenCommandOverlay::new(&commands).selected_command_index(1);

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                overlay.render(frame, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Select open command"));
        assert!(text.contains("j/k: move | Enter: open | Esc: cancel"));
    }

    #[test]
    fn test_open_command_overlay_lines_selected_row_uses_background_without_marker() {
        // Arrange
        let commands = vec!["nvim .".to_string(), "npm run dev".to_string()];
        let overlay = OpenCommandOverlay::new(&commands).selected_command_index(1);

        // Act
        let lines = overlay.lines(24);
        let selected_line = &lines[3];
        let selected_text: String = selected_line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        // Assert
        assert!(!selected_text.contains('>'));
        assert!(
            selected_line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(palette::ACCENT))
        );
    }

    #[test]
    fn test_open_command_overlay_lines_center_bottom_help_text() {
        // Arrange
        let commands = vec!["nvim .".to_string()];
        let overlay = OpenCommandOverlay::new(&commands);

        // Act
        let lines = overlay.lines(24);
        let help_line = lines
            .last()
            .expect("overlay should include a bottom help line");

        // Assert
        assert_eq!(help_line.alignment, Some(Alignment::Center));
    }

    #[test]
    fn test_open_command_overlay_lines_center_header_text() {
        // Arrange
        let commands = vec!["nvim .".to_string()];
        let overlay = OpenCommandOverlay::new(&commands);

        // Act
        let lines = overlay.lines(24);
        let header_line = lines.first().expect("overlay should include a header line");

        // Assert
        assert_eq!(header_line.alignment, Some(Alignment::Center));
    }
}
