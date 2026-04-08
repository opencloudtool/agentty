use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ui::state::help_action;
use crate::ui::{Page, style, util};

/// Tasks page showing the selected project's roadmap queues and cards.
pub struct TasksPage<'a> {
    /// Loaded roadmap markdown for the active project, when available.
    roadmap: Option<&'a str>,
    /// User-visible roadmap load failure for the active project, when present.
    roadmap_error: Option<&'a str>,
    /// Current vertical scroll offset for the rendered roadmap body.
    scroll_offset: u16,
}

impl<'a> TasksPage<'a> {
    /// Creates a tasks page renderer from the active project's roadmap
    /// snapshot.
    pub fn new(
        roadmap: Option<&'a str>,
        roadmap_error: Option<&'a str>,
        scroll_offset: u16,
    ) -> Self {
        Self {
            roadmap,
            roadmap_error,
            scroll_offset,
        }
    }
}

impl Page for TasksPage<'_> {
    /// Renders the active project's roadmap queue overview.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let content = match (self.roadmap, self.roadmap_error) {
            (Some(roadmap), _) => util::roadmap_task_lines(roadmap),
            (None, Some(error)) => roadmap_error_lines(error),
            (None, None) => roadmap_missing_lines(),
        };
        let max_scroll_offset = util::roadmap_task_max_scroll_offset(
            &content,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        let paragraph = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Roadmap Tasks")
                    .border_style(Style::default().fg(style::palette::BORDER)),
            )
            .scroll((self.scroll_offset.min(max_scroll_offset), 0))
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }
}

/// Returns the fallback lines shown when the roadmap file could not be loaded.
fn roadmap_error_lines(error: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Unable to load roadmap tasks.",
            Style::default().fg(style::palette::DANGER),
        )),
        Line::default(),
        Line::from(error.to_string()),
        Line::default(),
        help_action::footer_line(&help_action::task_footer_actions()),
    ]
}

/// Returns the fallback lines shown when no roadmap snapshot is available.
fn roadmap_missing_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "No roadmap data available for the active project.",
            Style::default().fg(style::palette::TEXT_MUTED),
        )),
        Line::default(),
        Line::from("Add `docs/plan/roadmap.md` to expose roadmap tasks here."),
        Line::default(),
        help_action::footer_line(&help_action::task_footer_actions()),
    ]
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn test_tasks_page_renders_roadmap_content() {
        // Arrange
        let roadmap = r"
## Ready Now
### [ca014af3-5cd0-4567-bf11-3495765dcf6f] Forge: Add GitHub pull-request publish shortcut in session chat
#### Assignee
`@minev-dev`
#### Usable outcome
Pressing `Shift+P` creates or refreshes the pull request.
#### Substeps
- [ ] Reuse the existing review-request workflow for GitHub sessions.

## Queued Next
### [17a9e2ba-0b7d-407d-9cd4-72807ef7bc1f] Delivery: Add project commit strategy selection
#### Outcome
Let each project stored in Agentty choose its expected landing path.
#### Promote when
Promote when maintainers want review and publish actions to respect project delivery flow.
#### Depends on
`[ca014af3] Forge`

## Parked
### [6bb0cae7-c07c-4fab-ae6b-e74444d3f0f0] Planning: Move roadmap tasks to a single canonical TOML plan
#### Outcome
Let Agentty manage roadmap tasks through one canonical `docs/plan/roadmap.toml` file.
#### Promote when
Promote when maintainers want direct task management in Agentty.
#### Depends on
`None`
";
        let backend = TestBackend::new(90, 30);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        let mut page = TasksPage::new(Some(roadmap), None, 0);

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
        assert!(rendered_text.contains("Roadmap Tasks"));
        assert!(rendered_text.contains("Ready Now"));
        assert!(rendered_text.contains("Forge: Add GitHub"));
        assert!(rendered_text.contains("Queued Next"));
        assert!(rendered_text.contains("Parked"));
    }

    #[test]
    fn test_tasks_page_renders_load_error_message() {
        // Arrange
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        let mut page = TasksPage::new(None, Some("Failed to load `docs/plan/roadmap.md`."), 0);

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
        assert!(rendered_text.contains("Unable to load roadmap tasks"));
        assert!(rendered_text.contains("Failed to load `docs/plan/roadmap.md`."));
    }

    #[test]
    fn test_tasks_page_applies_scroll_offset_to_roadmap_content() {
        // Arrange
        let roadmap = r"
## Ready Now
### [ca014af3-5cd0-4567-bf11-3495765dcf6f] Forge: Add GitHub pull-request publish shortcut in session chat
#### Assignee
`@minev-dev`
#### Usable outcome
Pressing `Shift+P` creates or refreshes the pull request.
#### Substeps
- [ ] First long scrolling line
- [ ] Second long scrolling line
- [ ] Third long scrolling line

## Queued Next
### [17a9e2ba-0b7d-407d-9cd4-72807ef7bc1f] Delivery: Add project commit strategy selection
#### Outcome
Let each project stored in Agentty choose its expected landing path.
";
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        let mut page = TasksPage::new(Some(roadmap), None, 6);

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
        assert!(!rendered_text.contains("Source: docs/plan/roadmap.md"));
        assert!(rendered_text.contains("Third long scrolling line"));
    }
}
