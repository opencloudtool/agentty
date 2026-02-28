use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::domain::session::Session;
use crate::infra::file_index::FileEntry;
use crate::ui::Page;
use crate::ui::state::help_action;

const CONTENT_FOOTER_HEIGHT: u16 = 1;
const CONTENT_MARGIN: u16 = 1;
const PATH_SEPARATOR: char = '/';
const PREVIEW_SCROLL_X_OFFSET: u16 = 0;
const TREE_BRANCH: &str = "└ ";

/// Renders full project file navigation with a file preview panel.
pub struct ProjectExplorerPage<'a> {
    pub entries: &'a [FileEntry],
    pub selected_index: usize,
    pub preview: &'a str,
    pub scroll_offset: u16,
    pub session: &'a Session,
}

impl<'a> ProjectExplorerPage<'a> {
    /// Creates a project explorer page for one session worktree.
    pub fn new(
        entries: &'a [FileEntry],
        selected_index: usize,
        preview: &'a str,
        scroll_offset: u16,
        session: &'a Session,
    ) -> Self {
        Self {
            entries,
            selected_index,
            preview,
            scroll_offset,
            session,
        }
    }

    /// Renders the left-side file list panel.
    fn render_file_list(&self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem<'_>> = if self.entries.is_empty() {
            vec![ListItem::new(Span::styled(
                "No files found",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.entries
                .iter()
                .map(|entry| {
                    let label = if entry.is_dir {
                        format_tree_label(entry.path.as_str(), true)
                    } else {
                        format_tree_label(entry.path.as_str(), false)
                    };
                    let color = if entry.is_dir {
                        Color::Yellow
                    } else {
                        Color::Cyan
                    };

                    ListItem::new(Span::styled(label, Style::default().fg(color)))
                })
                .collect()
        };

        let mut list_state = ListState::default();
        if !self.entries.is_empty() {
            list_state.select(Some(
                self.selected_index
                    .min(self.entries.len().saturating_sub(1)),
            ));
        }

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                " Project Files ",
                Style::default().fg(Color::Cyan),
            )))
            .highlight_style(Style::default().bg(Color::DarkGray));
        f.render_stateful_widget(list, area, &mut list_state);
    }

    /// Renders the right-side file preview panel.
    fn render_preview(&self, f: &mut Frame, area: Rect) {
        let selected_entry = self
            .entries
            .get(self.selected_index)
            .map_or("No file selected", |entry| entry.path.as_str());
        let title = format!(" Preview — {} ", self.session.display_title());
        let header = format!("Path: {selected_entry}\n\n");

        let paragraph = Paragraph::new(format!("{header}{}", self.preview))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(title, Style::default().fg(Color::Yellow))),
            )
            .scroll((self.scroll_offset, PREVIEW_SCROLL_X_OFFSET));
        f.render_widget(paragraph, area);
    }
}

/// Formats one explorer row using the same tree glyph style as diff explorer.
fn format_tree_label(path: &str, is_dir: bool) -> String {
    let depth = path.matches(PATH_SEPARATOR).count();
    let name = path.rsplit(PATH_SEPARATOR).next().unwrap_or(path);
    let indent = "  ".repeat(depth);
    let suffix = if is_dir { "/" } else { "" };

    format!("{indent}{TREE_BRANCH}{name}{suffix}")
}

impl Page for ProjectExplorerPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([
                Constraint::Min(0),
                Constraint::Length(CONTENT_FOOTER_HEIGHT),
            ])
            .margin(CONTENT_MARGIN)
            .split(area);
        let content_area = chunks[0];
        let footer_area = chunks[1];

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(content_area);
        self.render_file_list(f, content_layout[0]);
        self.render_preview(f, content_layout[1]);

        let help_text = help_action::footer_text(&help_action::project_explorer_footer_actions());
        let help_message = Paragraph::new(help_text).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::session::{SessionSize, SessionStats, Status};

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
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: Some("Project Explorer Session".to_string()),
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

    #[test]
    fn test_render_shows_project_explorer_help_hint() {
        // Arrange
        let session = session_fixture();
        let entries = vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }];
        let mut project_explorer_page =
            ProjectExplorerPage::new(&entries, 0, "fn main() {}", 0, &session);
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                crate::ui::Page::render(&mut project_explorer_page, frame, area);
            })
            .expect("failed to draw project explorer page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("j/k: select file"));
        assert!(text.contains("Up/Down: scroll file"));
    }
}
