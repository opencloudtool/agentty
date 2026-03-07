use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use time::OffsetDateTime;

use crate::domain::project::ProjectListItem;
use crate::ui::state::help_action;
use crate::ui::{Page, layout, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
const ACTIVE_PROJECT_MARKER: &str = "* ";
/// Horizontal spacing between project-table columns.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Fixed height reserved for the top Agentty info panel.
const AGENTTY_INFO_PANEL_HEIGHT: u16 = 9;
/// Percentage of the top-panel width reserved for the ASCII logo.
const AGENTTY_INFO_ASCII_WIDTH_PERCENT: u16 = 58;
/// Number of lines in the Agentty ASCII art banner.
const AGENTTY_ASCII_ART_LINE_COUNT: u16 = 5;
/// Maximum visible width of the Agentty ASCII art banner.
const AGENTTY_ASCII_ART_WIDTH: u16 = 45;
/// Compile-time version text shown in the projects info panel.
const AGENTTY_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
/// Short overview text shown alongside the Agentty version.
const AGENTTY_SHORT_DESCRIPTION: &str = "Agentty is an ADE (Agentic Development Environment) for \
                                         structured, controllable AI-assisted software \
                                         development.";
/// ASCII logo shown in the projects info panel.
const AGENTTY_ASCII_ART: &str = r"    _    ____ _____ _   _ _____ _____ __   __
   / \  / ___| ____| \ | |_   _|_   _|\ \ / /
  / _ \| |  _|  _| |  \| | | |   | |   \ V /
 / ___ \ |_| | |___| |\  | | |   | |    | |
/_/   \_\____|_____|_| \_| |_|   |_|    |_|";

/// Projects tab renderer showing saved repositories and quick metadata.
pub struct ProjectListPage<'a> {
    /// Identifier for the currently active project.
    pub active_project_id: i64,
    /// Project rows displayed in the table.
    pub projects: &'a [ProjectListItem],
    /// Stateful cursor position for the project table.
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
        let content_chunks = Layout::vertical([
            Constraint::Length(AGENTTY_INFO_PANEL_HEIGHT),
            Constraint::Min(0),
        ])
        .split(main_area);
        let info_area = content_chunks[0];
        let project_area = content_chunks[1];
        let info_panel_block = Block::default()
            .borders(Borders::ALL)
            .title("Agentty")
            .border_style(Style::default().fg(style::palette::BORDER));
        let info_panel_inner_area = info_panel_block.inner(info_area);
        let info_panel_chunks = Layout::horizontal([
            Constraint::Percentage(AGENTTY_INFO_ASCII_WIDTH_PERCENT),
            Constraint::Percentage(100 - AGENTTY_INFO_ASCII_WIDTH_PERCENT),
        ])
        .split(info_panel_inner_area);
        let logo_area = info_panel_chunks[0];
        let details_area = info_panel_chunks[1];
        let centered_logo_area = layout::centered_content_rect(
            logo_area,
            AGENTTY_ASCII_ART_WIDTH,
            AGENTTY_ASCII_ART_LINE_COUNT,
        );
        let logo_panel = Paragraph::new(AGENTTY_ASCII_ART)
            .style(Style::default().fg(style::palette::TEXT))
            .wrap(Wrap { trim: false });
        let details_panel = Paragraph::new(agentty_info_details_text())
            .style(Style::default().fg(style::palette::TEXT))
            .wrap(Wrap { trim: true });

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

        f.render_stateful_widget(table, project_area, self.table_state);
        f.render_widget(info_panel_block, info_area);
        f.render_widget(logo_panel, centered_logo_area);
        f.render_widget(details_panel, details_area);

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

/// Builds top-panel Agentty metadata text shown to the right of the logo.
fn agentty_info_details_text() -> String {
    format!(
        "Version: {AGENTTY_VERSION}\n\n{AGENTTY_SHORT_DESCRIPTION}\n\nDocs: https://agentty.xyz/docs"
    )
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

    #[test]
    fn test_agentty_info_details_text_includes_version_and_description() {
        // Arrange
        let expected_version = AGENTTY_VERSION;
        let expected_description = AGENTTY_SHORT_DESCRIPTION;

        // Act
        let info_text = agentty_info_details_text();

        // Assert
        assert!(info_text.contains(expected_version));
        assert!(info_text.contains(expected_description));
    }

    #[test]
    fn test_agentty_ascii_art_banner_matches_reference_header() {
        // Arrange
        let expected_banner_header = "    _    ____ _____ _   _ _____ _____ __   __";

        // Assert
        assert!(AGENTTY_ASCII_ART.starts_with(expected_banner_header));
    }

    #[test]
    fn test_agentty_ascii_art_line_count_matches_banner() {
        // Arrange & Act
        let actual_line_count = AGENTTY_ASCII_ART.lines().count();

        // Assert
        assert_eq!(actual_line_count, usize::from(AGENTTY_ASCII_ART_LINE_COUNT));
    }

    #[test]
    fn test_agentty_ascii_art_width_matches_banner() {
        // Arrange & Act
        let actual_width = AGENTTY_ASCII_ART
            .lines()
            .map(str::len)
            .max()
            .unwrap_or_default();

        // Assert
        assert_eq!(actual_width, usize::from(AGENTTY_ASCII_ART_WIDTH));
    }
}
