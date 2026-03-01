use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use crate::ui::Component;
use crate::ui::icon::Icon;
use crate::ui::text_util::wrap_lines;

const BODY_HORIZONTAL_PADDING: u16 = 2;
const BODY_VERTICAL_PADDING: u16 = 1;
const MIN_OVERLAY_HEIGHT: u16 = 9;
const MIN_OVERLAY_WIDTH: u16 = 44;
const OVERLAY_HEIGHT_PERCENT: u16 = 26;
const OVERLAY_WIDTH_PERCENT: u16 = 52;

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

    /// Returns popup width constrained by overlay defaults and frame bounds.
    fn popup_width(area: Rect) -> u16 {
        (area.width * OVERLAY_WIDTH_PERCENT / 100)
            .max(MIN_OVERLAY_WIDTH)
            .min(area.width)
    }

    /// Returns popup height sized to keep wrapped body content and the action
    /// row visible.
    fn popup_height(&self, area: Rect, width: u16) -> u16 {
        let horizontal_chrome = 2 + (BODY_HORIZONTAL_PADDING * 2);
        let vertical_chrome = 2 + (BODY_VERTICAL_PADDING * 2);
        let inner_width = width.saturating_sub(horizontal_chrome).max(1);
        let min_height = (area.height * OVERLAY_HEIGHT_PERCENT / 100)
            .max(MIN_OVERLAY_HEIGHT)
            .min(area.height);
        let action_row = if self.is_loading {
            format!("{} Sync in progress...", Icon::current_spinner())
        } else {
            "OK".to_string()
        };
        let body_with_action = format!("{}\n\n{action_row}", self.message);
        let required_inner_lines = wrap_lines(&body_with_action, usize::from(inner_width)).len();
        let required_height =
            u16::try_from(required_inner_lines.saturating_add(usize::from(vertical_chrome)))
                .unwrap_or(area.height)
                .min(area.height);

        required_height.max(min_height)
    }
}

impl Component for InfoOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let width = Self::popup_width(area);
        let title = format!(" {} ", self.title);
        let mut paragraph_lines = self.message_lines();
        let paragraph_alignment = if self.is_loading {
            Alignment::Center
        } else {
            Alignment::Left
        };
        let border_color = if self.is_loading {
            Color::Cyan
        } else {
            Color::Yellow
        };
        let title_style = Style::default()
            .fg(border_color)
            .add_modifier(Modifier::BOLD);

        if self.is_loading {
            let loading_line = format!("{} Sync in progress...", Icon::current_spinner());

            paragraph_lines.push(Line::from(""));
            paragraph_lines.push(
                Line::from(vec![Span::styled(
                    loading_line,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )])
                .alignment(Alignment::Center),
            );
        } else {
            let ok_style = Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD);

            paragraph_lines.push(Line::from(""));
            paragraph_lines.push(
                Line::from(vec![Span::styled(" OK ", ok_style)]).alignment(Alignment::Center),
            );
        }

        let paragraph = Paragraph::new(paragraph_lines)
            .alignment(paragraph_alignment)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(border_color))
                    .padding(Padding::new(
                        BODY_HORIZONTAL_PADDING,
                        BODY_HORIZONTAL_PADDING,
                        BODY_VERTICAL_PADDING,
                        BODY_VERTICAL_PADDING,
                    ))
                    .title(Span::styled(title, title_style))
                    .title_alignment(Alignment::Center),
            );
        let height = self.popup_height(area, width);
        let popup_area = Rect::new(
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
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
        assert!(!text.contains("Please wait."));
        assert!(!text.contains("OK"));
    }

    #[test]
    fn test_info_overlay_render_keeps_ok_indicator_for_multiline_message() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let overlay = InfoOverlay::new(
            "Sync failed",
            "Project `agentty` on main branch `main`.\n\nGit push requires authentication for \
             this repository.\nAuthorize git access, then run sync again.\nRun `gh auth login`, \
             or configure credentials with a PAT/SSH key.",
        );

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

    #[test]
    fn test_loading_state_centers_branch_header_and_loader() {
        // Arrange
        let message =
            "Project `agentty` on main branch `main`.\n\nSynchronizing with its upstream.";
        let loading_lines =
            render_overlay_lines(InfoOverlay::new("Sync in progress", message).is_loading(true));
        let blocked_lines = render_overlay_lines(InfoOverlay::new("Sync blocked", message));
        let header_text = "Project `agentty` on main branch `main`.";
        let loader_text = "Sync in progress...";

        // Act
        let loading_header_column =
            line_start_column(&loading_lines, header_text).expect("missing loading header");
        let blocked_header_column =
            line_start_column(&blocked_lines, header_text).expect("missing blocked header");
        let loading_loader_column =
            line_start_column(&loading_lines, loader_text).expect("missing loader text");

        // Assert
        assert!(loading_header_column > blocked_header_column);
        assert!(loading_loader_column > blocked_header_column);
    }

    /// Renders one overlay and returns text rows for column-position
    /// assertions.
    fn render_overlay_lines(overlay: InfoOverlay<'_>) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        terminal
            .draw(|frame| {
                let area = frame.area();
                crate::ui::Component::render(&overlay, frame, area);
            })
            .expect("failed to draw");

        let text_cells = terminal.backend().buffer().content();

        text_cells
            .chunks(100)
            .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
            .collect()
    }

    /// Returns the left-most column index for `needle` within rendered rows.
    fn line_start_column(lines: &[String], needle: &str) -> Option<usize> {
        lines.iter().find_map(|line| line.find(needle))
    }
}
