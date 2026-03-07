use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::session::Session;
use crate::ui::component::file_explorer::{FileExplorer, FileTreeItem};
use crate::ui::state::help_action;
use crate::ui::util::{
    DiffLine, DiffLineKind, diff_line_change_totals, max_diff_line_number, parse_diff_lines,
    wrap_diff_content,
};
use crate::ui::{Component, Page, style};

const BORDER_HORIZONTAL_WIDTH: u16 = 2;
const FOOTER_HEIGHT: u16 = 1;
const GUTTER_EXTRA_WIDTH: usize = 2;
const LINE_NUMBER_COLUMN_COUNT: usize = 2;
const LAYOUT_MARGIN: u16 = 1;
const MIN_GUTTER_WIDTH: usize = 1;
const SCROLL_X_OFFSET: u16 = 0;
const SIGN_COLUMN_WIDTH: usize = 1;
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
        total_added_lines: usize,
        total_removed_lines: usize,
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
                format!(") Diff — {} ", self.session.display_title()),
                Style::default().fg(style::palette::WARNING),
            ),
        ]);

        let max_num = max_diff_line_number(parsed);
        let gutter_width = if max_num == 0 {
            MIN_GUTTER_WIDTH
        } else {
            max_num.ilog10() as usize + MIN_GUTTER_WIDTH
        };

        // gutter: "old│new " = gutter_width * 2 + 2 (separator + trailing space)
        // sign column: 1 char
        let prefix_width =
            gutter_width * LINE_NUMBER_COLUMN_COUNT + GUTTER_EXTRA_WIDTH + SIGN_COLUMN_WIDTH;
        let inner_width = area.width.saturating_sub(BORDER_HORIZONTAL_WIDTH) as usize;

        let gutter_style = Style::default().fg(style::palette::TEXT_SUBTLE);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(parsed.len());

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
                Some(num) => format!("{num:>gutter_width$}"),
                None => " ".repeat(gutter_width),
            };
            let new_str = match diff_line.new_line {
                Some(num) => format!("{num:>gutter_width$}"),
                None => " ".repeat(gutter_width),
            };

            let gutter_text = format!("{old_str}│{new_str} ");
            let content_available = inner_width.saturating_sub(prefix_width);
            let chunks = wrap_diff_content(diff_line.content, content_available);

            for (idx, chunk) in chunks.iter().enumerate() {
                if idx == WRAPPED_CHUNK_START_INDEX {
                    lines.push(Line::from(vec![
                        Span::styled(gutter_text.clone(), gutter_style),
                        Span::styled(sign, content_style),
                        Span::styled(*chunk, content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(" ".repeat(prefix_width), gutter_style),
                        Span::styled(*chunk, content_style),
                    ]));
                }
            }
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .scroll((self.scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, area);
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
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(FOOTER_HEIGHT)])
            .margin(LAYOUT_MARGIN)
            .split(area);

        let content_area = chunks[0];
        let footer_area = chunks[1];

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(content_area);

        let file_list_area = content_layout[0];
        let diff_area = content_layout[1];

        let parsed = parse_diff_lines(&self.diff);
        let (total_added_lines, total_removed_lines) = diff_line_change_totals(&parsed);
        let tree_items = FileExplorer::file_tree_items(&parsed);

        FileExplorer::new(&parsed)
            .selected_index(self.file_explorer_selected_index)
            .render(f, file_list_area);

        let filtered = selected_diff_lines(&parsed, &tree_items, self.file_explorer_selected_index);
        self.render_diff_content(
            f,
            diff_area,
            &filtered,
            total_added_lines,
            total_removed_lines,
        );

        let help_message =
            Paragraph::new(help_action::footer_line(&help_action::diff_footer_actions()));
        f.render_widget(help_message, footer_area);
    }
}

/// Returns diff lines for the selected file explorer item.
///
/// When `selected_index` is out of bounds, the full parsed diff is returned so
/// the diff panel continues to show meaningful content.
fn selected_diff_lines<'a>(
    parsed_lines: &[DiffLine<'a>],
    tree_items: &[FileTreeItem],
    selected_index: usize,
) -> Vec<DiffLine<'a>> {
    let Some(selected_item) = tree_items.get(selected_index) else {
        return parsed_lines.iter().map(clone_diff_line).collect();
    };

    FileExplorer::filter_diff_lines(parsed_lines, selected_item)
}

/// Clones a parsed diff line while preserving borrowed content.
fn clone_diff_line<'a>(diff_line: &DiffLine<'a>) -> DiffLine<'a> {
    DiffLine {
        kind: diff_line.kind,
        old_line: diff_line.old_line,
        new_line: diff_line.new_line,
        content: diff_line.content,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::session::{SessionSize, SessionStats, Status};
    use crate::ui::util::parse_diff_lines;

    const SAMPLE_DIFF: &str = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "+added in main\n",
        "diff --git a/README.md b/README.md\n",
        "+added in readme\n"
    );

    fn session_fixture() -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            review_request: None,
            questions: Vec::new(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: Some("Diff Session".to_string()),
            updated_at: 0,
        }
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
        let session = session_fixture();
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
}
