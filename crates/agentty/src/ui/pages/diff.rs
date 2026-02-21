use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::session::Session;
use crate::ui::components::file_explorer::FileExplorer;
use crate::ui::util::{
    DiffLine, DiffLineKind, max_diff_line_number, parse_diff_lines, wrap_diff_content,
};
use crate::ui::{Component, Page};

const BORDER_HORIZONTAL_WIDTH: u16 = 2;
const FOOTER_HEIGHT: u16 = 1;
const GUTTER_EXTRA_WIDTH: usize = 2;
const LINE_NUMBER_COLUMN_COUNT: usize = 2;
const LAYOUT_MARGIN: u16 = 1;
const MIN_GUTTER_WIDTH: usize = 1;
const SCROLL_X_OFFSET: u16 = 0;
const SIGN_COLUMN_WIDTH: usize = 1;
const WRAPPED_CHUNK_START_INDEX: usize = 0;
const HELP_HINT: &str = "q: back | j/k: select file | Up/Down: scroll selected file | ?: help";

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

    fn render_diff_content(&self, f: &mut Frame, area: Rect, parsed: &[DiffLine]) {
        let title = format!(" Diff — {} ", self.session.display_title());

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

        let gutter_style = Style::default().fg(Color::DarkGray);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(parsed.len());

        for diff_line in parsed {
            let (sign, content_style) = match diff_line.kind {
                DiffLineKind::FileHeader => {
                    if diff_line.content.starts_with("diff ") && !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(Color::Yellow),
                    )));

                    continue;
                }
                DiffLineKind::HunkHeader => {
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(Color::Cyan),
                    )));

                    continue;
                }
                DiffLineKind::Addition => ("+", Style::default().fg(Color::Green)),
                DiffLineKind::Deletion => ("-", Style::default().fg(Color::Red)),
                DiffLineKind::Context => (" ", Style::default().fg(Color::Gray)),
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
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(title, Style::default().fg(Color::Yellow))),
            )
            .scroll((self.scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, area);
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
        let tree_items = FileExplorer::file_tree_items(&parsed);

        FileExplorer::new(&parsed, self.file_explorer_selected_index).render(f, file_list_area);

        let filtered = tree_items
            .get(self.file_explorer_selected_index)
            .map(|item| FileExplorer::filter_diff_lines(&parsed, item))
            .unwrap_or_default();
        self.render_diff_content(f, diff_area, &filtered);

        let help_message = Paragraph::new(HELP_HINT).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::permission::PermissionMode;
    use crate::domain::session::{SessionSize, SessionStats, Status};

    fn session_fixture() -> Session {
        Session {
            base_branch: "main".to_string(),
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            permission_mode: PermissionMode::default(),
            project_name: "project".to_string(),
            prompt: String::new(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: Some("Diff Session".to_string()),
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
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
                crate::ui::Page::render(&mut diff_page, frame, area);
            })
            .expect("failed to draw diff page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("j/k: select file"));
        assert!(text.contains("Up/Down: scroll selected file"));
    }
}
