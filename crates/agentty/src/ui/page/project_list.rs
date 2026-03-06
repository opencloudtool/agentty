use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use time::OffsetDateTime;

use crate::domain::project::ProjectListItem;
use crate::ui::state::help_action;
use crate::ui::{Page, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
const ACTIVE_PROJECT_MARKER: &str = "* ";
/// Horizontal spacing between project-table columns.
const TABLE_COLUMN_SPACING: u16 = 2;

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

        let selected_style = Style::default().bg(style::palette::SURFACE);
        let header = Row::new(["Project", "Branch", "Sessions", "Last Opened", "Path"])
            .style(
                Style::default()
                    .bg(style::palette::SURFACE)
                    .fg(style::palette::TEXT_MUTED)
                    .add_modifier(Modifier::BOLD),
            )
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
        .column_spacing(TABLE_COLUMN_SPACING)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Projects"))
        .row_highlight_style(selected_style)
        .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        f.render_stateful_widget(table, main_area, self.table_state);

        let help_message = Paragraph::new(help_action::footer_line(
            &help_action::project_list_footer_actions(),
        ));
        f.render_widget(help_message, footer_area);
    }
}

/// Renders one project metadata row.
fn render_project_row(project_item: &ProjectListItem, active_project_id: i64) -> Row<'static> {
    let (title, branch, last_opened, path) = project_row_values(project_item, active_project_id);

    Row::new(vec![
        Cell::from(title),
        Cell::from(branch),
        Cell::from(session_count_line(
            project_item.session_count,
            project_item.active_session_count,
        )),
        Cell::from(last_opened),
        Cell::from(path),
    ])
    .style(project_row_style(project_item, active_project_id))
}

/// Returns project row display values for reuse and testing.
fn project_row_values(
    project_item: &ProjectListItem,
    active_project_id: i64,
) -> (String, String, String, String) {
    let project = &project_item.project;
    let title = project_title(project_item, active_project_id);
    let branch = project.git_branch.as_deref().unwrap_or("-");
    let last_opened = format_last_opened(project.last_opened_at);
    let path = project.path.to_string_lossy().to_string();

    (title, branch.to_string(), last_opened, path)
}

/// Returns style for one project row, emphasizing the active project.
fn project_row_style(project_item: &ProjectListItem, active_project_id: i64) -> Style {
    if project_item.project.id == active_project_id {
        return Style::default().fg(style::palette::ACCENT_SOFT);
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

/// Builds a styled line for the session count column, coloring the active
/// indicator in yellow when active sessions exist.
fn session_count_line(total: u32, active: u32) -> Line<'static> {
    if active > 0 {
        return Line::from(vec![
            Span::raw(format!("{total} ")),
            Span::styled(
                format!("▶ {active}"),
                Style::default().fg(style::palette::WARNING),
            ),
        ]);
    }

    Line::from(total.to_string())
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
    fn test_row_highlight_symbol_uses_background_only_selection() {
        // Arrange
        let highlight_symbol = ROW_HIGHLIGHT_SYMBOL;

        // Act
        let is_empty_symbol = highlight_symbol.is_empty();

        // Assert
        assert!(is_empty_symbol);
    }

    #[test]
    fn test_project_table_column_spacing_is_wider_for_readability() {
        // Arrange
        let expected_spacing = 2;

        // Act
        let spacing = TABLE_COLUMN_SPACING;

        // Assert
        assert_eq!(spacing, expected_spacing);
    }

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
    fn test_project_row_values_show_metadata() {
        // Arrange
        let project_item = ProjectListItem {
            active_session_count: 0,
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
        assert_eq!(values.2, "2023-11-14");
    }

    #[test]
    fn test_session_count_line_shows_plain_total_without_active() {
        // Arrange & Act
        let line = session_count_line(7, 0);

        // Assert
        assert_eq!(line.to_string(), "7");
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn test_session_count_line_colors_active_indicator_yellow() {
        // Arrange & Act
        let line = session_count_line(5, 2);

        // Assert
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content.as_ref(), "5 ");
        assert_eq!(line.spans[1].content.as_ref(), "▶ 2");
        assert_eq!(line.spans[1].style.fg, Some(style::palette::WARNING));
    }

    #[test]
    fn test_project_row_values_mark_active_project_title() {
        // Arrange
        let project_item = ProjectListItem {
            active_session_count: 0,
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
