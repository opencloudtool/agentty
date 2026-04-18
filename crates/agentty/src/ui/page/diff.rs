use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::session::Session;
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::state::help_action;
use crate::ui::util::{DiffLine, DiffLineKind, inline_text, parse_diff_lines, selected_diff_lines};
use crate::ui::{Component, Page, diff_util, style};

const SCROLL_X_OFFSET: u16 = 0;
const SCROLLBAR_TRACK_SYMBOL: &str = "│";
const SCROLLBAR_THUMB_SYMBOL: &str = "█";
const WRAPPED_CHUNK_START_INDEX: usize = 0;

/// Renders the current session's git diff in a scrollable page.
pub struct DiffPage<'a> {
    pub diff: String,
    pub scroll_offset: u16,
    pub session: &'a Session,
    pub file_explorer_selected_index: usize,
}

impl<'a> DiffPage<'a> {
    /// Creates a diff page for the given session and scroll position.
    pub fn new(
        session: &'a Session,
        diff: String,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
    ) -> Self {
        Self {
            diff,
            scroll_offset,
            session,
            file_explorer_selected_index,
        }
    }

    /// Renders the right-side diff panel with line-number gutters and
    /// change totals prefixed in the title.
    fn render_diff_content(
        &self,
        f: &mut Frame,
        area: Rect,
        parsed: &[DiffLine],
        total_added_lines: u64,
        total_removed_lines: u64,
    ) {
        let title = Line::from(vec![
            Span::styled(" (", Style::default().fg(style::palette::WARNING)),
            Span::styled(
                format!("+{total_added_lines}"),
                Style::default().fg(style::palette::SUCCESS),
            ),
            Span::styled(" ", Style::default().fg(style::palette::WARNING)),
            Span::styled(
                format!("-{total_removed_lines}"),
                Style::default().fg(style::palette::DANGER),
            ),
            Span::styled(
                format!(") Diff — {} ", inline_text(self.session.display_title())),
                Style::default().fg(style::palette::WARNING),
            ),
        ]);

        let mut layout = diff_util::diff_render_layout(parsed, area, false);
        let mut lines = Self::build_diff_lines(parsed, layout);
        let mut show_scrollbar =
            diff_util::diff_has_scrollable_overflow(lines.len(), layout.viewport_height);

        if show_scrollbar {
            layout = diff_util::diff_render_layout(parsed, area, true);
            lines = Self::build_diff_lines(parsed, layout);
            show_scrollbar =
                diff_util::diff_has_scrollable_overflow(lines.len(), layout.viewport_height);
        }
        let total_lines = lines.len();

        let scroll_offset = diff_util::clamp_diff_scroll_offset(
            self.scroll_offset,
            total_lines,
            layout.viewport_height,
        );

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .scroll((scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, area);

        if show_scrollbar {
            Self::render_diff_scrollbar(
                f,
                area,
                layout.viewport_height,
                scroll_offset,
                total_lines,
            );
        }
    }

    /// Returns the style used for added diff lines.
    fn addition_line_style() -> Style {
        Style::default()
            .fg(style::palette::SUCCESS)
            .bg(style::palette::SURFACE_SUCCESS)
    }

    /// Returns the style used for removed diff lines.
    fn deletion_line_style() -> Style {
        Style::default()
            .fg(style::palette::DANGER)
            .bg(style::palette::SURFACE_DANGER)
    }

    /// Builds wrapped diff lines for the diff panel, optionally reserving one
    /// column for the scrollbar thumb.
    fn build_diff_lines<'line>(
        parsed: &[DiffLine<'line>],
        layout: diff_util::DiffRenderLayout,
    ) -> Vec<Line<'line>> {
        let gutter_style = Style::default().fg(style::palette::TEXT_SUBTLE);
        let mut lines: Vec<Line<'line>> = Vec::with_capacity(parsed.len());

        for diff_line in parsed {
            let (sign, content_style) = match diff_line.kind {
                DiffLineKind::FileHeader => {
                    if diff_line.content.starts_with("diff ") && !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(style::palette::WARNING),
                    )));

                    continue;
                }
                DiffLineKind::HunkHeader => {
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(style::palette::ACCENT),
                    )));

                    continue;
                }
                DiffLineKind::Addition => ("+", Self::addition_line_style()),
                DiffLineKind::Deletion => ("-", Self::deletion_line_style()),
                DiffLineKind::Context => (" ", Style::default().fg(style::palette::TEXT_MUTED)),
            };

            let old_str = match diff_line.old_line {
                Some(num) => format!("{num:>width$}", width = layout.gutter_width),
                None => " ".repeat(layout.gutter_width),
            };
            let new_str = match diff_line.new_line {
                Some(num) => format!("{num:>width$}", width = layout.gutter_width),
                None => " ".repeat(layout.gutter_width),
            };

            let gutter_text = format!("{old_str}│{new_str} ");
            let content_available = layout.content_width.saturating_sub(layout.prefix_width);
            let chunks = diff_util::wrap_diff_content(diff_line.content, content_available);

            for (idx, chunk) in chunks.iter().enumerate() {
                if idx == WRAPPED_CHUNK_START_INDEX {
                    lines.push(Line::from(vec![
                        Span::styled(gutter_text.clone(), gutter_style),
                        Span::styled(sign, content_style),
                        Span::styled(*chunk, content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(" ".repeat(layout.prefix_width), gutter_style),
                        Span::styled(*chunk, content_style),
                    ]));
                }
            }
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        lines
    }

    /// Renders a slim scrollbar inside the diff panel so users can see their
    /// position in long diffs at a glance.
    fn render_diff_scrollbar(
        f: &mut Frame,
        area: Rect,
        viewport_height: u16,
        scroll_offset: u16,
        total_lines: usize,
    ) {
        if viewport_height == 0 {
            return;
        }

        if !diff_util::diff_has_scrollable_overflow(total_lines, viewport_height) {
            return;
        }

        let track_height = usize::from(viewport_height);
        let thumb_height = (track_height * track_height / total_lines).max(1);
        let max_scroll = total_lines.saturating_sub(track_height);
        let max_thumb_offset = track_height.saturating_sub(thumb_height);
        let thumb_offset = usize::from(scroll_offset)
            .saturating_mul(max_thumb_offset)
            .checked_div(max_scroll)
            .unwrap_or(0);

        let scrollbar_area = diff_util::diff_scrollbar_area(area, viewport_height);
        let mut scrollbar_lines = Vec::with_capacity(track_height);

        for line_index in 0..track_height {
            let is_thumb_line =
                line_index >= thumb_offset && line_index < thumb_offset + thumb_height;
            let (symbol, symbol_style) = if is_thumb_line {
                (
                    SCROLLBAR_THUMB_SYMBOL,
                    Style::default().fg(style::palette::WARNING),
                )
            } else {
                (
                    SCROLLBAR_TRACK_SYMBOL,
                    Style::default().fg(style::palette::TEXT_SUBTLE),
                )
            };

            scrollbar_lines.push(Line::from(Span::styled(symbol, symbol_style)));
        }

        f.render_widget(Paragraph::new(scrollbar_lines), scrollbar_area);
    }
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = diff_util::diff_page_areas(area);

        let parsed = parse_diff_lines(&self.diff);
        let tree_items = FileExplorer::file_tree_items(&parsed);

        FileExplorer::new(&parsed)
            .selected_index(self.file_explorer_selected_index)
            .render(f, areas.file_list_area);

        let filtered = selected_diff_lines(&parsed, &tree_items, self.file_explorer_selected_index);
        self.render_diff_content(
            f,
            areas.diff_area,
            &filtered,
            self.session.stats.added_lines,
            self.session.stats.deleted_lines,
        );

        let help_message =
            Paragraph::new(help_action::footer_line(&help_action::diff_footer_actions()));
        f.render_widget(help_message, areas.footer_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::tests::SessionFixtureBuilder;
    use crate::ui::util::parse_diff_lines;

    const SAMPLE_DIFF: &str = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "+added in main\n",
        "diff --git a/README.md b/README.md\n",
        "+added in readme\n"
    );

    fn session_fixture() -> Session {
        SessionFixtureBuilder::new()
            .title(Some("Diff Session".to_string()))
            .build()
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn background_cell_count(
        buffer: &ratatui::buffer::Buffer,
        color: ratatui::style::Color,
    ) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.bg == color)
            .count()
    }

    #[test]
    fn test_render_shows_updated_diff_help_hint() {
        // Arrange
        let mut session = session_fixture();
        session.stats.added_lines = 1;
        session.stats.deleted_lines = 0;
        let mut diff_page = DiffPage::new(
            &session,
            "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            0,
            0,
        );
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("(+1 -0) Diff — Diff Session"));
        assert!(text.contains("j/k: select file"));
        assert!(text.contains("Up/Down: scroll file"));
    }

    #[test]
    fn test_render_diff_title_uses_persisted_session_line_totals() {
        // Arrange
        let mut session = session_fixture();
        session.stats.added_lines = 9;
        session.stats.deleted_lines = 4;
        let mut diff_page = DiffPage::new(
            &session,
            "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            0,
            0,
        );
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("(+9 -4) Diff — Diff Session"));
        assert!(!text.contains("(+1 -0) Diff — Diff Session"));
    }

    #[test]
    fn test_selected_diff_lines_returns_filtered_section_for_selected_file() {
        // Arrange
        let parsed_lines = parse_diff_lines(SAMPLE_DIFF);
        let tree_items = FileExplorer::file_tree_items(&parsed_lines);

        // Act
        let selected_lines = selected_diff_lines(&parsed_lines, &tree_items, 1);

        // Assert
        assert_eq!(selected_lines.len(), 2);
        assert_eq!(
            selected_lines[0].content,
            "diff --git a/src/main.rs b/src/main.rs"
        );
        assert_eq!(selected_lines[1].content, "added in main");
    }

    #[test]
    fn test_selected_diff_lines_returns_full_diff_when_index_is_out_of_bounds() {
        // Arrange
        let parsed_lines = parse_diff_lines(SAMPLE_DIFF);
        let tree_items = FileExplorer::file_tree_items(&parsed_lines);

        // Act
        let selected_lines = selected_diff_lines(&parsed_lines, &tree_items, usize::MAX);

        // Assert
        assert_eq!(selected_lines.len(), parsed_lines.len());
        assert_eq!(selected_lines[0].content, parsed_lines[0].content);
        assert_eq!(selected_lines[3].content, parsed_lines[3].content);
    }

    #[test]
    fn test_render_applies_background_tints_to_changed_lines() {
        // Arrange
        let session = session_fixture();
        let mut diff_page = DiffPage::new(
            &session,
            concat!(
                "diff --git a/src/main.rs b/src/main.rs\n",
                "@@ -1,2 +1,2 @@\n",
                "-old content\n",
                "+new content\n"
            )
            .to_string(),
            0,
            0,
        );
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let buffer = terminal.backend().buffer();
        assert!(
            background_cell_count(buffer, style::palette::SURFACE_SUCCESS) > 0,
            "expected added lines to include success background tint"
        );
        assert!(
            background_cell_count(buffer, style::palette::SURFACE_DANGER) > 0,
            "expected removed lines to include danger background tint"
        );
    }

    #[test]
    fn test_render_shows_scrollbar_for_overflowing_diff() {
        // Arrange
        let session = session_fixture();
        let diff = (0..80)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut diff_page = DiffPage::new(&session, diff, 12, 0);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains(SCROLLBAR_TRACK_SYMBOL));
        assert!(text.contains(SCROLLBAR_THUMB_SYMBOL));
    }

    #[test]
    fn test_render_clamps_overscroll_to_last_visible_diff_lines() {
        // Arrange
        let session = session_fixture();
        let diff = (0..40)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut diff_page = DiffPage::new(&session, diff, u16::MAX, 0);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("line 39"));
    }
}
