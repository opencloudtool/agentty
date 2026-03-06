use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use crate::ui::Component;
use crate::ui::icon::Icon;
use crate::ui::markdown::render_markdown;

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

    /// Renders the body message as markdown with inline highlight styles and
    /// explicit line breaks.
    fn message_lines(&self, message_width: usize) -> Vec<Line<'static>> {
        let normalized_message = Self::markdown_message_with_block_headers(self.message);
        let mut message_lines = render_markdown(&normalized_message, message_width);

        if message_lines.is_empty() {
            message_lines.push(Line::from(""));
        }

        message_lines
    }

    /// Adds markdown block headers for numbered sync sections and inserts blank
    /// lines between each section.
    fn markdown_message_with_block_headers(message: &str) -> String {
        let mut normalized_lines: Vec<String> = Vec::new();

        for raw_line in message.split('\n') {
            if let Some(formatted_line) = Self::format_sync_block_title(raw_line) {
                if let Some(previous_line) = normalized_lines.last()
                    && !previous_line.is_empty()
                {
                    normalized_lines.push(String::new());
                }

                normalized_lines.push(formatted_line);
                continue;
            }

            normalized_lines.push(raw_line.to_string());
        }

        normalized_lines.join("\n")
    }

    /// Converts known sync section title lines (e.g., `1. Pull`, `2. Push`) to
    /// markdown heading text.
    fn format_sync_block_title(raw_line: &str) -> Option<String> {
        let trimmed_line = raw_line.trim();
        if trimmed_line.is_empty() {
            return None;
        }

        let heading_content = trimmed_line.strip_prefix("## ").unwrap_or(trimmed_line);

        split_prefixed_title(heading_content)?;
        let normalized_title = heading_content.to_ascii_lowercase();
        if !normalized_title.contains("pull")
            && !normalized_title.contains("push")
            && !normalized_title.contains("conflict")
        {
            return None;
        }

        if trimmed_line.starts_with("## ") {
            return Some(trimmed_line.to_string());
        }

        Some(format!("## {heading_content}"))
    }

    /// Centers the first body line when it matches the sync context header
    /// format (`Project ...` or `Main branch ...`).
    fn center_sync_context_header(message_lines: &mut [Line<'static>]) {
        let Some(first_line) = message_lines.first_mut() else {
            return;
        };

        if !Self::is_sync_context_header(first_line) {
            return;
        }

        *first_line = first_line.clone().alignment(Alignment::Center);
    }

    /// Returns whether a message line is the generated sync context header.
    fn is_sync_context_header(line: &Line<'_>) -> bool {
        let line_text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        line_text.starts_with("Project ") || line_text.starts_with("Main branch ")
    }

    /// Builds the styled body lines including the action row at the bottom.
    fn body_lines(&self, message_width: usize) -> Vec<Line<'static>> {
        let mut lines = self.message_lines(message_width);

        if !self.is_loading {
            Self::center_sync_context_header(&mut lines);
        }

        lines.push(Line::from(""));
        lines.push(self.action_row());

        lines
    }

    /// Returns the bottom action row: a spinner during loading or an `OK`
    /// button when complete.
    fn action_row(&self) -> Line<'static> {
        if self.is_loading {
            let loading_text = format!("{} Sync in progress...", Icon::current_spinner());

            Line::from(vec![Span::styled(loading_text, loading_indicator_style())])
                .alignment(Alignment::Center)
        } else {
            Line::from(vec![Span::styled(" OK ", ok_button_style())]).alignment(Alignment::Center)
        }
    }

    /// Returns the popup border color: cyan during loading, yellow when
    /// complete.
    fn border_color(&self) -> Color {
        if self.is_loading {
            Color::Cyan
        } else {
            Color::Yellow
        }
    }

    /// Returns the body text alignment: centered during loading, left-aligned
    /// when complete.
    fn body_alignment(&self) -> Alignment {
        if self.is_loading {
            Alignment::Center
        } else {
            Alignment::Left
        }
    }

    /// Returns popup width constrained by overlay defaults and frame bounds.
    fn popup_width(area: Rect) -> u16 {
        (area.width * OVERLAY_WIDTH_PERCENT / 100)
            .max(MIN_OVERLAY_WIDTH)
            .min(area.width)
    }

    /// Returns the message rendering width after subtracting borders and
    /// horizontal padding.
    fn message_width(width: u16) -> usize {
        let horizontal_chrome = 2 + (BODY_HORIZONTAL_PADDING * 2);

        usize::from(width.saturating_sub(horizontal_chrome).max(1))
    }

    /// Returns popup height sized to keep wrapped body content and the action
    /// row visible.
    fn popup_height(&self, area: Rect, width: u16) -> u16 {
        let vertical_chrome = 2 + (BODY_VERTICAL_PADDING * 2);
        let min_height = (area.height * OVERLAY_HEIGHT_PERCENT / 100)
            .max(MIN_OVERLAY_HEIGHT)
            .min(area.height);
        let message_width = Self::message_width(width);
        let required_inner_lines = self.body_lines(message_width).len();
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
        let message_width = Self::message_width(width);
        let border_color = self.border_color();
        let title_text = format!(" {} ", self.title);
        let paragraph = Paragraph::new(self.body_lines(message_width))
            .alignment(self.body_alignment())
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
                    .title(Span::styled(title_text, title_style(border_color)))
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

/// Style for the popup title text, colored to match the border.
fn title_style(border_color: Color) -> Style {
    Style::default()
        .fg(border_color)
        .add_modifier(Modifier::BOLD)
}

/// Style for the `OK` confirmation button.
fn ok_button_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// Style for the loading spinner text.
fn loading_indicator_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// Splits numbered section titles like `1. Pull` or `2) Push`.
fn split_prefixed_title(line: &str) -> Option<&str> {
    if let Some(dot_index) = line.find('.') {
        if dot_index == 0 {
            return None;
        }

        if !line[..dot_index]
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            return None;
        }

        return Some(line[dot_index + 1..].trim());
    }

    let parenthesis_index = line.find(')')?;
    if parenthesis_index == 0 {
        return None;
    }

    if !line[..parenthesis_index]
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        return None;
    }

    Some(line[parenthesis_index + 1..].trim())
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
                Component::render(&overlay, f, area);
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
                Component::render(&overlay, f, area);
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
                Component::render(&overlay, f, area);
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
        let message_lines = overlay.message_lines(80);

        // Assert
        assert_eq!(message_lines.len(), 2);
        assert_eq!(
            rendered_line_text(&message_lines[0]),
            "Sync cannot run on this branch."
        );
        assert_eq!(
            rendered_line_text(&message_lines[1]),
            "Commit or stash, then retry."
        );
    }

    #[test]
    fn test_message_lines_highlight_inline_code_segments() {
        // Arrange
        let overlay = InfoOverlay::new("Sync blocked", "Run `gh auth login` then retry.");

        // Act
        let message_lines = overlay.message_lines(80);

        // Assert
        assert_eq!(message_lines.len(), 1);
        assert_eq!(
            rendered_line_text(&message_lines[0]),
            "Run gh auth login then retry."
        );
        assert!(
            message_lines[0]
                .spans
                .iter()
                .any(|span| span.style.fg == Some(Color::Yellow))
        );
    }

    #[test]
    fn test_markdown_message_with_block_headers_formats_sync_sections() {
        // Arrange
        let message = "1. Pull\n2 commits pulled\n\n2. Push\n1 commit pushed\n\n3. \
                       Conflicts\nconflicts fixed: src/lib.rs";

        // Act
        let formatted_message = InfoOverlay::markdown_message_with_block_headers(message);

        // Assert
        assert_eq!(
            formatted_message,
            concat!(
                "## 1. Pull\n",
                "2 commits pulled\n",
                "\n",
                "## 2. Push\n",
                "1 commit pushed\n",
                "\n",
                "## 3. Conflicts\n",
                "conflicts fixed: src/lib.rs",
            )
        );
    }

    #[test]
    fn test_markdown_message_with_block_headers_inserts_missing_section_spacing() {
        // Arrange
        let message = concat!(
            "## 1. 2 commits pulled\n",
            "2 commits pulled\n",
            "## 2. 1 commit pushed\n",
            "1 commit pushed\n",
            "## 3. conflicts fixed: src/lib.rs\n",
            "conflicts fixed: src/lib.rs",
        );

        // Act
        let formatted_message = InfoOverlay::markdown_message_with_block_headers(message);

        // Assert
        assert!(formatted_message.contains("2 commits pulled\n\n## 2. 1 commit pushed"));
        assert!(
            formatted_message.contains("1 commit pushed\n\n## 3. conflicts fixed: src/lib.rs",)
        );
    }

    #[test]
    fn test_result_state_centers_branch_header_like_loading_state() {
        // Arrange
        let message =
            "Project `agentty` on main branch `main`.\n\nSynchronizing with its upstream.";
        let loading_overlay = InfoOverlay::new("Sync in progress", message).is_loading(true);
        let blocked_overlay = InfoOverlay::new("Sync blocked", message);
        let loading_lines = render_overlay_lines(&loading_overlay);
        let blocked_lines = render_overlay_lines(&blocked_overlay);
        let header_text = "Project";
        let detail_text = "Synchronizing with its upstream.";
        let loader_text = "Sync in progress...";

        // Act
        let loading_header_column =
            line_start_column(&loading_lines, header_text).expect("missing loading header");
        let blocked_header_column =
            line_start_column(&blocked_lines, header_text).expect("missing blocked header");
        let blocked_detail_column =
            line_start_column(&blocked_lines, detail_text).expect("missing blocked detail");
        let loading_loader_column =
            line_start_column(&loading_lines, loader_text).expect("missing loader text");

        // Assert
        assert_eq!(loading_header_column, blocked_header_column);
        assert!(blocked_header_column > blocked_detail_column);
        assert!(loading_loader_column > blocked_header_column);
    }

    /// Renders one overlay and returns text rows for column-position
    /// assertions.
    fn render_overlay_lines(overlay: &InfoOverlay<'_>) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        terminal
            .draw(|frame| {
                let area = frame.area();
                Component::render(overlay, frame, area);
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

    /// Returns the concatenated plain text for one rendered `Line`.
    fn rendered_line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}
