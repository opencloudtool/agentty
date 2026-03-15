use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::icon::Icon;
use crate::ui::{Component, style};

/// Footer widget that renders the working directory and optional git status.
pub struct FooterBar {
    git_branch: Option<String>,
    git_status: Option<(u32, u32)>,
    working_dir: String,
}

impl FooterBar {
    /// Creates a footer bar initialized with the active working directory.
    pub fn new(working_dir: String) -> Self {
        Self {
            git_branch: None,
            git_status: None,
            working_dir,
        }
    }

    /// Sets the active git branch name.
    #[must_use]
    pub fn git_branch(mut self, branch: Option<String>) -> Self {
        self.git_branch = branch;
        self
    }

    /// Sets the git status (ahead, behind) counts.
    #[must_use]
    pub fn git_status(mut self, status: Option<(u32, u32)>) -> Self {
        self.git_status = status;
        self
    }
}

impl Component for FooterBar {
    fn render(&self, f: &mut Frame, area: Rect) {
        if area.width == 0 {
            return;
        }

        let display_path = if let Some(home) = dirs::home_dir() {
            if let Ok(path) = std::path::Path::new(&self.working_dir).strip_prefix(home) {
                format!("~/{}", path.display())
            } else {
                self.working_dir.clone()
            }
        } else {
            self.working_dir.clone()
        };

        let left_text = Span::styled(
            format!(" {display_path}"),
            Style::default()
                .fg(style::palette::TEXT)
                .add_modifier(Modifier::DIM),
        );

        let left_width = left_text.width();
        let total_width = usize::from(area.width);
        let mut spans = vec![left_text];

        if let Some(branch) = &self.git_branch {
            let trailing_branch_padding = 1;
            let status_text = if let Some((ahead, behind)) = self.git_status {
                if ahead == 0 && behind == 0 {
                    format!("{} ", Icon::Check)
                } else {
                    format!("{}{behind} {}{ahead} ", Icon::ArrowDown, Icon::ArrowUp)
                }
            } else {
                String::new()
            };

            let branch_span = Span::styled(
                format!("{status_text}{} {branch}", Icon::GitBranch),
                Style::default().fg(style::palette::SUCCESS),
            );
            let branch_width = branch_span.width();

            if left_width + branch_width + trailing_branch_padding <= total_width {
                let padding_width =
                    total_width - left_width - branch_width - trailing_branch_padding;
                let padding = " ".repeat(padding_width);

                spans.push(Span::raw(padding));
                spans.push(branch_span);
                spans.push(Span::raw(" ".repeat(trailing_branch_padding)));
            }
        }

        let line_width: usize = spans.iter().map(Span::width).sum();
        if line_width < total_width {
            spans.push(Span::raw(" ".repeat(total_width - line_width)));
        }

        let footer = Paragraph::new(Line::from(spans)).style(
            Style::default()
                .bg(style::palette::SURFACE)
                .fg(style::palette::TEXT),
        );

        f.render_widget(footer, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_footer_bar_new_with_git_branch() {
        // Arrange
        let path = "/home/user/project".to_string();
        let branch = Some("main".to_string());

        // Act
        let footer = FooterBar::new(path.clone()).git_branch(branch.clone());

        // Assert
        assert_eq!(footer.working_dir, path);
        assert_eq!(footer.git_branch, branch);
        assert_eq!(footer.git_status, None);
    }

    #[test]
    fn test_footer_bar_new_without_git_branch() {
        // Arrange
        let path = "/home/user/project".to_string();

        // Act
        let footer = FooterBar::new(path.clone());

        // Assert
        assert_eq!(footer.working_dir, path);
        assert_eq!(footer.git_branch, None);
        assert_eq!(footer.git_status, None);
    }

    #[test]
    fn test_footer_bar_render_with_git_branch() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let path = if let Some(home) = dirs::home_dir() {
            home.join("project").to_string_lossy().to_string()
        } else {
            "/tmp/project".to_string()
        };
        let footer = FooterBar::new(path).git_branch(Some("main".to_string()));

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&footer, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        if dirs::home_dir().is_some() {
            assert!(text.contains("~/project"));
        } else {
            assert!(text.contains("/tmp/project"));
        }
        assert!(text.contains("main"));
    }

    #[test]
    fn test_footer_bar_render_with_git_status() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let path = "/tmp/project".to_string();
        // 1 ahead, 2 behind
        let footer = FooterBar::new(path)
            .git_branch(Some("main".to_string()))
            .git_status(Some((1, 2)));

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&footer, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("↓2 ↑1"));
        assert!(text.contains("main"));
    }

    #[test]
    fn test_footer_bar_render_without_git_branch() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let path = "/tmp/other-project".to_string();
        let footer = FooterBar::new(path);

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&footer, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("/tmp/other-project"));
    }

    #[test]
    fn test_footer_bar_render_shows_git_status_when_unicode_width_exactly_fits() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(20, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/tmp/p".to_string())
            .git_branch(Some("main".to_string()))
            .git_status(Some((1, 2)));

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&footer, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("↓2 ↑1"));
        assert!(text.contains("main"));
    }

    #[test]
    fn test_footer_bar_render_clears_stale_branch_cells_on_redraw() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(40, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let footer_with_branch = FooterBar::new("/tmp/project".to_string())
            .git_branch(Some("main".to_string()))
            .git_status(Some((0, 0)));
        let footer_without_branch = FooterBar::new("/tmp/other".to_string());

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&footer_with_branch, f, area);
            })
            .expect("failed to draw");
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&footer_without_branch, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("/tmp/other"));
        assert!(!text.contains("main"));
        assert!(!text.contains("✓"));
    }

    #[test]
    fn test_footer_bar_render_keeps_one_space_after_branch_name() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(25, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/tmp/p".to_string()).git_branch(Some("main".to_string()));

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                Component::render(&footer, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let last_column_index = buffer.area.width.saturating_sub(1);
        let second_to_last_column_index = last_column_index.saturating_sub(1);
        let last_symbol = buffer[(last_column_index, 0)].symbol();
        let second_to_last_symbol = buffer[(second_to_last_column_index, 0)].symbol();
        assert_eq!(last_symbol, " ");
        assert_eq!(second_to_last_symbol, "n");
    }
}
