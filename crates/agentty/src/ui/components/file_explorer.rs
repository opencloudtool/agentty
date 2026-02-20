use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::ui::Component;
use crate::ui::util::{DiffLine, DiffLineKind};

/// Diff file explorer panel rendering the changed file list.
pub struct FileExplorer {
    file_list_lines: Vec<Line<'static>>,
}

impl FileExplorer {
    /// Creates a new file explorer component from parsed diff lines.
    pub fn new(parsed_lines: &[DiffLine<'_>]) -> Self {
        Self {
            file_list_lines: Self::file_list_lines(parsed_lines),
        }
    }

    fn file_list_lines(parsed_lines: &[DiffLine<'_>]) -> Vec<Line<'static>> {
        let mut file_list_lines = Vec::new();

        for diff_line in parsed_lines {
            if diff_line.kind != DiffLineKind::FileHeader
                || !diff_line.content.starts_with("diff --git")
            {
                continue;
            }

            let file_path_text = Self::display_path(diff_line.content);
            file_list_lines.push(Line::from(Span::styled(
                file_path_text,
                Style::default().fg(Color::Cyan),
            )));
        }

        if file_list_lines.is_empty() {
            file_list_lines.push(Line::from(Span::styled(
                "No files",
                Style::default().fg(Color::DarkGray),
            )));
        }

        file_list_lines
    }

    fn display_path(file_header_line: &str) -> String {
        if let Some(stripped_header) = file_header_line.strip_prefix("diff --git a/") {
            if let Some((old_path, new_path)) = stripped_header.split_once(" b/") {
                if old_path == new_path {
                    return old_path.to_string();
                }

                return format!("{old_path} -> {new_path}");
            }

            return stripped_header.to_string();
        }

        file_header_line.replace("diff --git ", "")
    }
}

impl Component for FileExplorer {
    fn render(&self, f: &mut Frame, area: Rect) {
        let file_list_paragraph = Paragraph::new(self.file_list_lines.clone()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Files ", Style::default().fg(Color::Cyan))),
        );

        f.render_widget(file_list_paragraph, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_list_lines_with_same_path() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: "diff --git a/src/main.rs b/src/main.rs",
        }];

        // Act
        let lines = FileExplorer::file_list_lines(&parsed_lines);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "src/main.rs");
    }

    #[test]
    fn test_file_list_lines_with_rename() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: "diff --git a/src/old.rs b/src/new.rs",
        }];

        // Act
        let lines = FileExplorer::file_list_lines(&parsed_lines);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "src/old.rs -> src/new.rs");
    }

    #[test]
    fn test_file_list_lines_with_nonstandard_header() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::FileHeader,
            old_line: None,
            new_line: None,
            content: "diff --git old/path new/path",
        }];

        // Act
        let lines = FileExplorer::file_list_lines(&parsed_lines);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "old/path new/path");
    }

    #[test]
    fn test_file_list_lines_with_no_files() {
        // Arrange
        let parsed_lines = vec![DiffLine {
            kind: DiffLineKind::Context,
            old_line: Some(1),
            new_line: Some(1),
            content: " unchanged",
        }];

        // Act
        let lines = FileExplorer::file_list_lines(&parsed_lines);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "No files");
    }
}
