use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use time::OffsetDateTime;

use crate::domain::project::ProjectListItem;
use crate::ui::Page;
use crate::ui::state::help_action;

const ROW_HIGHLIGHT_SYMBOL: &str = ">> ";
const ACTIVE_PROJECT_MARKER: &str = "* ";

/// Projects tab renderer showing saved repositories and quick metadata.
pub struct ProjectListPage<'a> {
    /// Identifier for the currently active project.
    pub active_project_id: i64,
    pub projects: &'a [ProjectListItem],
    pub table_state: &'a mut TableState,
}

impl<'a> ProjectListPage<'a> {
    /// Creates a project-list page renderer with active-project highlighting.
    pub fn new(
        projects: &'a [ProjectListItem],
        table_state: &'a mut TableState,
        active_project_id: i64,
    ) -> Self {
        Self {
            active_project_id,
            projects,
            table_state,
        }
    }
}

impl Page for ProjectListPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        let selected_style = Style::default().bg(Color::DarkGray);
        let header = Row::new(["Project", "Branch", "Sessions", "Last Opened", "Path"])
            .style(Style::default().bg(Color::Gray).fg(Color::Black))
            .height(1)
            .bottom_margin(1);
        let active_project_id = self.active_project_id;
        let rows = self
            .projects
            .iter()
            .map(|project_item| render_project_row(project_item, active_project_id));
        let table = Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(12),
                Constraint::Length(8),
                Constraint::Length(12),
                Constraint::Fill(1),
            ],
        )
        .column_spacing(1)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Projects"))
        .row_highlight_style(selected_style)
        .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        f.render_stateful_widget(table, main_area, self.table_state);

        let help_text = help_action::footer_text(&help_action::project_list_footer_actions());
        let help_message = Paragraph::new(help_text).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

/// Renders one project metadata row.
fn render_project_row(project_item: &ProjectListItem, active_project_id: i64) -> Row<'static> {
    let (title, branch, session_count, last_opened, path) =
        project_row_values(project_item, active_project_id);

    Row::new(vec![
        Cell::from(title),
        Cell::from(branch),
        Cell::from(session_count),
        Cell::from(last_opened),
        Cell::from(path),
    ])
    .style(project_row_style(project_item, active_project_id))
}

/// Returns all project row display values for reuse and testing.
fn project_row_values(
    project_item: &ProjectListItem,
    active_project_id: i64,
) -> (String, String, String, String, String) {
    let project = &project_item.project;
    let title = project_title(project_item, active_project_id);
    let branch = project.git_branch.as_deref().unwrap_or("-");
    let session_count = project_item.session_count.to_string();
    let last_opened = format_last_opened(project.last_opened_at);
    let path = project.path.to_string_lossy().to_string();

    (title, branch.to_string(), session_count, last_opened, path)
}

/// Returns style for one project row, emphasizing the active project.
fn project_row_style(project_item: &ProjectListItem, active_project_id: i64) -> Style {
    if project_item.project.id == active_project_id {
        return Style::default().fg(Color::LightCyan);
    }

    Style::default()
}

/// Returns the visible project title, marking the active project in the list.
fn project_title(project_item: &ProjectListItem, active_project_id: i64) -> String {
    let display_label = project_item.project.display_label();
    if project_item.project.id == active_project_id {
        return format!("{ACTIVE_PROJECT_MARKER}{display_label}");
    }

    display_label
}

/// Formats the project last-opened timestamp for table display.
fn format_last_opened(last_opened_at: Option<i64>) -> String {
    let Some(last_opened_at) = last_opened_at else {
        return "Never".to_string();
    };
    let Ok(last_opened_datetime) = OffsetDateTime::from_unix_timestamp(last_opened_at) else {
        return "Unknown".to_string();
    };

    let year = last_opened_datetime.year();
    let month = u8::from(last_opened_datetime.month());
    let day = last_opened_datetime.day();

    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::project::Project;

    #[test]
    fn test_format_last_opened_uses_iso_like_date() {
        // Arrange
        let last_opened_at = Some(1_700_000_000);

        // Act
        let formatted = format_last_opened(last_opened_at);

        // Assert
        assert_eq!(formatted, "2023-11-14");
    }

    #[test]
    fn test_project_row_values_show_session_count() {
        // Arrange
        let project_item = ProjectListItem {
            last_session_updated_at: Some(20),
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 1,
                is_favorite: true,
                last_opened_at: Some(1_700_000_000),
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 3,
        };

        // Act
        let values = project_row_values(&project_item, 99);

        // Assert
        assert_eq!(values.0, "agentty");
        assert_eq!(values.2, "3");
        assert_eq!(values.3, "2023-11-14");
    }

    #[test]
    fn test_project_row_values_mark_active_project_title() {
        // Arrange
        let project_item = ProjectListItem {
            last_session_updated_at: Some(20),
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 42,
                is_favorite: true,
                last_opened_at: Some(1_700_000_000),
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 3,
        };

        // Act
        let values = project_row_values(&project_item, 42);

        // Assert
        assert_eq!(values.0, "* agentty");
    }
}
