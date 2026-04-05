use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use crate::ui::Page;

/// Placeholder tasks page shown for projects that expose
/// `docs/plan/roadmap.md`.
pub struct TasksPage;

impl Page for TasksPage {
    /// Renders the temporary tasks placeholder content.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        f.render_widget(Paragraph::new("Tasks coming soon"), area);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn test_tasks_page_renders_placeholder_message() {
        // Arrange
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        let mut page = TasksPage;

        // Act
        terminal
            .draw(|frame| page.render(frame, frame.area()))
            .expect("failed to render tasks page");
        let buffer = terminal.backend().buffer().clone();
        let rendered_text: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        // Assert
        assert!(rendered_text.contains("Tasks coming soon"));
    }
}
