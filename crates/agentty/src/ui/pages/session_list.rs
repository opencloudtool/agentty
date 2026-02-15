use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::model::{Session, Status};
use crate::ui::Page;

/// Session list page renderer.
pub struct SessionListPage<'a> {
    pub sessions: &'a [Session],
    pub table_state: &'a mut TableState,
}

impl<'a> SessionListPage<'a> {
    /// Creates a session list page renderer.
    pub fn new(sessions: &'a [Session], table_state: &'a mut TableState) -> Self {
        Self {
            sessions,
            table_state,
        }
    }
}

impl Page for SessionListPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        let selected_style = Style::default().bg(Color::DarkGray);
        let normal_style = Style::default().bg(Color::Gray).fg(Color::Black);
        let header_cells = ["Session", "Project", "Model", "Status"]
            .iter()
            .map(|h| Cell::from(*h));
        let header = Row::new(header_cells)
            .style(normal_style)
            .height(1)
            .bottom_margin(1);
        let rows = self.sessions.iter().map(|session| {
            let status = session.status();
            let cells = vec![
                Cell::from(session.display_title().to_string()),
                Cell::from(session.project_name.clone()),
                Cell::from(session.model.clone()),
                Cell::from(format!("{status}")).style(Style::default().fg(status.color())),
            ];
            Row::new(cells).height(1)
        });
        let table = Table::new(
            rows,
            [
                Constraint::Min(0),
                project_column_width(self.sessions),
                model_column_width(self.sessions),
                status_column_width(),
            ],
        )
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Sessions"))
        .row_highlight_style(selected_style)
        .highlight_symbol(">> ");

        f.render_stateful_widget(table, main_area, self.table_state);

        let help_message = Paragraph::new(
            "q: quit | /: command | a: add | d: delete | Enter: view | j/k: nav | ?: help",
        )
        .style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

fn project_column_width(sessions: &[Session]) -> Constraint {
    text_column_width(
        "Project",
        sessions.iter().map(|session| session.project_name.as_str()),
    )
}

fn model_column_width(sessions: &[Session]) -> Constraint {
    text_column_width(
        "Model",
        sessions.iter().map(|session| session.model.as_str()),
    )
}

fn status_column_width() -> Constraint {
    text_column_width(
        "Status",
        [
            Status::New,
            Status::InProgress,
            Status::Review,
            Status::CreatingPullRequest,
            Status::PullRequest,
            Status::Done,
            Status::Canceled,
        ]
        .iter()
        .map(std::string::ToString::to_string),
    )
}

fn text_column_width<T>(header: &str, values: impl Iterator<Item = T>) -> Constraint
where
    T: AsRef<str>,
{
    let column_width = values
        .map(|value| value.as_ref().chars().count())
        .fold(header.chars().count(), usize::max);
    let column_width = u16::try_from(column_width).unwrap_or(u16::MAX);

    Constraint::Length(column_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_column_width_uses_longest_possible_status_label() {
        // Arrange
        let expected_width =
            u16::try_from("CreatingPullRequest".chars().count()).unwrap_or(u16::MAX);

        // Act
        let width = status_column_width();

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_project_column_width_uses_longest_project_value() {
        // Arrange
        let expected_width =
            u16::try_from("very-long-project-name".chars().count()).unwrap_or(u16::MAX);
        let project_names = ["api", "very-long-project-name"];

        // Act
        let width = text_column_width("Project", project_names.into_iter());

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_model_column_width_uses_longest_model_value() {
        // Arrange
        let expected_width =
            u16::try_from("claude-sonnet-4-5-20250929".chars().count()).unwrap_or(u16::MAX);
        let models = ["gpt-5.2-codex", "claude-sonnet-4-5-20250929"];

        // Act
        let width = text_column_width("Model", models.into_iter());

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }
}
