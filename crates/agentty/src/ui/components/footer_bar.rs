use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::Component;

const GIT_BRANCH_INDICATOR: char = '\u{25cf}'; // ●

pub struct FooterBar {
    working_dir: String,
    git_branch: Option<String>,
}

impl FooterBar {
    pub fn new(working_dir: String, git_branch: Option<String>) -> Self {
        Self {
            working_dir,
            git_branch,
        }
    }
}

impl Component for FooterBar {
    fn render(&self, f: &mut Frame, area: Rect) {
        let left_text = Span::styled(
            format!(" Working Directory: {}", self.working_dir),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::DIM),
        );

        let mut spans = vec![left_text];

        if let Some(branch) = &self.git_branch {
            let left_width = format!(" Working Directory: {}", self.working_dir).len();
            let branch_text = format!("{GIT_BRANCH_INDICATOR} {branch}");
            let branch_width = branch_text.len();
            let total_width = area.width as usize;

            if left_width + branch_width + 1 < total_width {
                let padding_width = total_width - left_width - branch_width;
                let padding = " ".repeat(padding_width);

                spans.push(Span::raw(padding));
                spans.push(Span::styled(branch_text, Style::default().fg(Color::Green)));
            }
        }

        let footer = Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));

        f.render_widget(footer, area);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn test_footer_bar_new_with_git_branch() {
        // Arrange
        let path = "/home/user/project".to_string();
        let branch = Some("main".to_string());

        // Act
        let footer = FooterBar::new(path.clone(), branch.clone());

        // Assert
        assert_eq!(footer.working_dir, path);
        assert_eq!(footer.git_branch, branch);
    }

    #[test]
    fn test_footer_bar_new_without_git_branch() {
        // Arrange
        let path = "/home/user/project".to_string();

        // Act
        let footer = FooterBar::new(path.clone(), None);

        // Assert
        assert_eq!(footer.working_dir, path);
        assert_eq!(footer.git_branch, None);
    }

    #[test]
    fn test_footer_bar_render_with_git_branch() {
        // Arrange
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/home/user".to_string(), Some("main".to_string()));

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                footer.render(f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("Working Directory: /home/user"));
        assert!(text.contains("main"));
    }

    #[test]
    fn test_footer_bar_render_without_git_branch() {
        // Arrange
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        let footer = FooterBar::new("/home/user".to_string(), None);

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                footer.render(f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("Working Directory: /home/user"));
    }
}
