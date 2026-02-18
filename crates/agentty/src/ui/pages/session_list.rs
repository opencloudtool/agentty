use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::model::{PermissionMode, Session, SessionSize, Status};
use crate::ui::Page;
use crate::ui::util::{first_table_column_width, truncate_with_ellipsis};

const ROW_HIGHLIGHT_SYMBOL: &str = ">> ";
const TABLE_COLUMN_SPACING: u16 = 1;

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
        let header_cells = ["Session", "Project", "Model", "Mode", "Size", "Status"]
            .iter()
            .map(|h| Cell::from(*h));
        let header = Row::new(header_cells)
            .style(normal_style)
            .height(1)
            .bottom_margin(1);

        let block = Block::default().borders(Borders::ALL).title("Sessions");
        let column_constraints = [
            Constraint::Min(0),
            project_column_width(self.sessions),
            model_column_width(self.sessions),
            mode_column_width(),
            size_column_width(),
            status_column_width(),
        ];
        let has_selection = !self.sessions.is_empty() && self.table_state.selected().is_some();
        let selection_width = if has_selection {
            u16::try_from(ROW_HIGHLIGHT_SYMBOL.chars().count()).unwrap_or(u16::MAX)
        } else {
            0
        };
        let title_column_width = first_table_column_width(
            block.inner(main_area).width,
            &column_constraints,
            TABLE_COLUMN_SPACING,
            selection_width,
        );
        let rows = self.sessions.iter().map(|session| {
            let status = session.status;
            let display_title = truncate_with_ellipsis(session.display_title(), title_column_width);
            let cells = vec![
                Cell::from(display_title),
                Cell::from(session.project_name.clone()),
                Cell::from(session.model.clone()),
                Cell::from(session.permission_mode.display_label()),
                Cell::from(session.size.to_string())
                    .style(Style::default().fg(size_color(session.size))),
                Cell::from(format!("{status}")).style(Style::default().fg(status.color())),
            ];
            Row::new(cells).height(1)
        });
        let table = Table::new(rows, column_constraints)
            .column_spacing(TABLE_COLUMN_SPACING)
            .header(header)
            .block(block)
            .row_highlight_style(selected_style)
            .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        f.render_stateful_widget(table, main_area, self.table_state);

        let mut help_text = "q: quit | /: command | a: add".to_string();
        if !self.sessions.is_empty() {
            help_text.push_str(" | d: delete | c: cancel");
        }
        help_text.push_str(" | Enter: view | j/k: nav | ?: help");

        let help_message = Paragraph::new(help_text).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

pub(crate) fn project_column_width(sessions: &[Session]) -> Constraint {
    text_column_width(
        "Project",
        sessions.iter().map(|session| session.project_name.as_str()),
    )
}

pub(crate) fn model_column_width(sessions: &[Session]) -> Constraint {
    text_column_width(
        "Model",
        sessions.iter().map(|session| session.model.as_str()),
    )
}

fn mode_column_width() -> Constraint {
    text_column_width(
        "Mode",
        [
            PermissionMode::AutoEdit,
            PermissionMode::Autonomous,
            PermissionMode::Plan,
        ]
        .iter()
        .map(|mode| mode.display_label()),
    )
}

fn size_column_width() -> Constraint {
    text_column_width(
        "Size",
        SessionSize::ALL
            .iter()
            .map(std::string::ToString::to_string),
    )
}

fn status_column_width() -> Constraint {
    text_column_width(
        "Status",
        [
            Status::New,
            Status::InProgress,
            Status::Review,
            Status::Merging,
            Status::CreatingPullRequest,
            Status::PullRequest,
            Status::Done,
            Status::Canceled,
        ]
        .iter()
        .map(std::string::ToString::to_string),
    )
}

fn size_color(size: SessionSize) -> Color {
    match size {
        SessionSize::Xs => Color::Green,
        SessionSize::S => Color::LightGreen,
        SessionSize::M => Color::Yellow,
        SessionSize::L => Color::LightYellow,
        SessionSize::Xl => Color::LightRed,
        SessionSize::Xxl => Color::Red,
    }
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

    #[test]
    fn test_mode_column_width_uses_longest_mode_label() {
        // Arrange
        let expected_width = u16::try_from("Autonomous".chars().count()).unwrap_or(u16::MAX);

        // Act
        let width = mode_column_width();

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_size_column_width_uses_header_width() {
        // Arrange
        let expected_width = u16::try_from("Size".chars().count()).unwrap_or(u16::MAX);

        // Act
        let width = size_column_width();

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

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
    fn test_size_color_uses_expected_palette() {
        // Arrange
        let test_cases = [
            (SessionSize::Xs, Color::Green),
            (SessionSize::S, Color::LightGreen),
            (SessionSize::M, Color::Yellow),
            (SessionSize::L, Color::LightYellow),
            (SessionSize::Xl, Color::LightRed),
            (SessionSize::Xxl, Color::Red),
        ];

        // Act & Assert
        for (size, expected_color) in test_cases {
            assert_eq!(size_color(size), expected_color);
        }
    }
}
