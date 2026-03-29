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
    git_base_ref: Option<String>,
    git_base_status: Option<(u32, u32)>,
    git_status: Option<(u32, u32)>,
    git_upstream_ref: Option<String>,
    working_dir: String,
}

impl FooterBar {
    /// Creates a footer bar initialized with the active working directory.
    pub fn new(working_dir: String) -> Self {
        Self {
            git_branch: None,
            git_base_ref: None,
            git_base_status: None,
            git_status: None,
            git_upstream_ref: None,
            working_dir,
        }
    }

    /// Sets the active git branch name.
    #[must_use]
    pub fn git_branch(mut self, branch: Option<String>) -> Self {
        self.git_branch = branch;
        self
    }

    /// Sets the base branch reference label used by session footers.
    #[must_use]
    pub fn git_base_ref(mut self, base_ref: Option<String>) -> Self {
        self.git_base_ref = base_ref;
        self
    }

    /// Sets the base-branch comparison `(ahead, behind)` counts for session
    /// footers.
    #[must_use]
    pub fn git_base_status(mut self, status: Option<(u32, u32)>) -> Self {
        self.git_base_status = status;
        self
    }

    /// Sets the git status `(ahead, behind)` counts for the rendered branch.
    ///
    /// Project footers typically pass upstream-tracking counts, while session
    /// footers use this slot for tracked-remote counts.
    #[must_use]
    pub fn git_status(mut self, status: Option<(u32, u32)>) -> Self {
        self.git_status = status;
        self
    }

    /// Sets the upstream reference tracked by the active branch.
    #[must_use]
    pub fn git_upstream_ref(mut self, upstream_ref: Option<String>) -> Self {
        self.git_upstream_ref = upstream_ref;
        self
    }

    /// Returns the footer branch label, including the tracked upstream when
    /// available.
    fn branch_label(&self, branch: &str) -> String {
        match self.git_upstream_ref.as_deref() {
            Some(upstream_ref) => format!("{branch} -> {upstream_ref}"),
            None => branch.to_string(),
        }
    }

    /// Returns the full text rendered after the branch icon.
    fn branch_text(&self, branch: &str) -> String {
        if let Some(base_ref) = self.git_base_ref.as_deref() {
            return self.session_branch_text(branch, base_ref);
        }

        let status_text = self
            .git_status
            .map(|status| Self::format_status(status))
            .unwrap_or_default();

        format!("{status_text}{}", self.branch_label(branch))
    }

    /// Returns the compact session footer text with base and optional remote
    /// or local segments.
    fn session_branch_text(&self, branch: &str, base_ref: &str) -> String {
        let mut segments = vec![Self::format_session_segment(self.git_base_status, base_ref)];
        let remote_label = match self.git_upstream_ref.as_deref() {
            Some(upstream_ref) => format!("{branch} -> {upstream_ref}"),
            None => branch.to_string(),
        };
        segments.push(Self::format_session_segment(
            self.git_status,
            remote_label.as_str(),
        ));

        segments.join(" | ")
    }

    /// Formats one session segment as `<stats> <label>`.
    fn format_session_segment(status: Option<(u32, u32)>, label: &str) -> String {
        let status_text = status.map_or_else(|| Self::format_status((0, 0)), Self::format_status);

        format!("{status_text}{label}")
    }

    /// Formats one status segment with no reference label attached.
    fn format_status(status: (u32, u32)) -> String {
        let (ahead, behind) = status;

        if ahead == 0 && behind == 0 {
            return format!("{} ", Icon::Check);
        }

        format!("{}{behind} {}{ahead} ", Icon::ArrowDown, Icon::ArrowUp)
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
            let branch_text = self.branch_text(branch);

            let branch_span = Span::styled(
                format!("{} {branch_text}", Icon::GitBranch),
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
        assert_eq!(footer.git_base_ref, None);
        assert_eq!(footer.git_base_status, None);
        assert_eq!(footer.git_status, None);
        assert_eq!(footer.git_upstream_ref, None);
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
        assert_eq!(footer.git_base_ref, None);
        assert_eq!(footer.git_base_status, None);
        assert_eq!(footer.git_status, None);
        assert_eq!(footer.git_upstream_ref, None);
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
    fn test_footer_bar_render_with_git_upstream_reference() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/tmp/project".to_string())
            .git_branch(Some("main".to_string()))
            .git_upstream_ref(Some("origin/main".to_string()));

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
        assert!(text.contains("main -> origin/main"));
    }

    #[test]
    fn test_footer_bar_render_with_base_and_remote_statuses() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/tmp/project".to_string())
            .git_branch(Some("agentty/session".to_string()))
            .git_base_ref(Some("main".to_string()))
            .git_base_status(Some((1, 2)))
            .git_status(Some((3, 4)))
            .git_upstream_ref(Some("origin/agentty/session".to_string()));

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
        assert!(text.contains("↓2 ↑1 main"));
        assert!(text.contains("| ↓4 ↑3 agentty/session -> origin/agentty/session"));
    }

    #[test]
    fn test_footer_bar_render_with_base_and_local_statuses_without_upstream() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/tmp/project".to_string())
            .git_branch(Some("agentty/session".to_string()))
            .git_base_ref(Some("main".to_string()))
            .git_base_status(Some((1, 2)));

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
        assert!(text.contains("↓2 ↑1 main"));
        assert!(text.contains("| ✓ agentty/session"));
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
        let backend = ratatui::backend::TestBackend::new(40, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/tmp/p".to_string())
            .git_branch(Some("main".to_string()))
            .git_upstream_ref(Some("origin/main".to_string()));

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
