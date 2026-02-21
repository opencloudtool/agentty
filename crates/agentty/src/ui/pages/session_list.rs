use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::domain::permission::PermissionMode;
use crate::domain::session::{Session, SessionSize, Status};
use crate::ui::Page;
use crate::ui::util::{first_table_column_width, truncate_with_ellipsis};

const ROW_HIGHLIGHT_SYMBOL: &str = ">> ";
const TABLE_COLUMN_SPACING: u16 = 1;
/// Placeholder text rendered under group headers with no sessions.
const GROUP_EMPTY_PLACEHOLDER: &str = "No sessions...";

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
            Constraint::Fill(1),
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
        let table_rows = grouped_session_rows(self.sessions);
        let selected_session_id = selected_session_id(self.sessions, self.table_state.selected());
        let selected_row = selected_render_row(&table_rows, selected_session_id);
        let rows = table_rows
            .iter()
            .map(|table_row| render_table_row(table_row, title_column_width));
        let table = Table::new(rows, column_constraints)
            .column_spacing(TABLE_COLUMN_SPACING)
            .header(header)
            .block(block)
            .row_highlight_style(selected_style)
            .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        let previous_selection = self.table_state.selected();
        prepare_grouped_table_state(self.table_state, selected_row);
        f.render_stateful_widget(table, main_area, self.table_state);
        self.table_state.select(previous_selection);

        let mut help_text = "q: quit | /: command | a: add".to_string();
        if !self.sessions.is_empty() {
            help_text.push_str(" | d: delete | c: cancel");
        }
        help_text.push_str(" | Enter: view | j/k: nav | ?: help");

        let help_message = Paragraph::new(help_text).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

/// Prepares list table state for grouped row rendering.
///
/// The app stores selection as an index in the raw session slice, while the
/// table is rendered with extra group label and placeholder rows. Resetting
/// the offset before selecting a grouped row avoids stale deep offsets hiding
/// top group sections after scrolling back up.
fn prepare_grouped_table_state(table_state: &mut TableState, selected_row: Option<usize>) {
    *table_state.offset_mut() = 0;
    table_state.select(selected_row);
}

/// Render rows for grouped session list display.
enum SessionTableRow<'a> {
    GroupLabel(SessionGroup),
    /// Marker row shown when a group has zero sessions.
    EmptyGroupPlaceholder,
    Session(&'a Session),
}

/// Session list groups shown in the table.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SessionGroup {
    ActiveSessions,
    Archive,
    MergeQueue,
}

impl SessionGroup {
    /// Returns the display label for a session group.
    fn label(self) -> &'static str {
        match self {
            Self::MergeQueue => "Merge queue",
            Self::ActiveSessions => "Active sessions",
            Self::Archive => "Archive",
        }
    }
}

/// Returns session indexes in the same order as selectable rows in the grouped
/// session table.
pub(crate) fn grouped_session_indexes(sessions: &[Session]) -> Vec<usize> {
    let mut indexes = Vec::with_capacity(sessions.len());
    indexes.extend(sessions_for_group(sessions, SessionGroup::MergeQueue).map(|(index, _)| index));
    indexes
        .extend(sessions_for_group(sessions, SessionGroup::ActiveSessions).map(|(index, _)| index));
    indexes.extend(sessions_for_group(sessions, SessionGroup::Archive).map(|(index, _)| index));

    indexes
}

/// Returns grouped display rows with merge queue, active, then archive
/// sessions.
fn grouped_session_rows(sessions: &[Session]) -> Vec<SessionTableRow<'_>> {
    let mut rows = Vec::with_capacity(sessions.len() + 6);
    append_group_rows(&mut rows, sessions, SessionGroup::MergeQueue);
    append_group_rows(&mut rows, sessions, SessionGroup::ActiveSessions);
    append_group_rows(&mut rows, sessions, SessionGroup::Archive);

    rows
}

/// Adds one group label row and either its sessions or an empty placeholder.
fn append_group_rows<'a>(
    rows: &mut Vec<SessionTableRow<'a>>,
    sessions: &'a [Session],
    group: SessionGroup,
) {
    rows.push(SessionTableRow::GroupLabel(group));

    let mut group_has_sessions = false;
    for (_, session) in sessions_for_group(sessions, group) {
        rows.push(SessionTableRow::Session(session));
        group_has_sessions = true;
    }

    if !group_has_sessions {
        rows.push(SessionTableRow::EmptyGroupPlaceholder);
    }
}

/// Returns session indexes and snapshots for one grouped section.
fn sessions_for_group(
    sessions: &[Session],
    group: SessionGroup,
) -> impl Iterator<Item = (usize, &Session)> {
    sessions
        .iter()
        .enumerate()
        .filter(move |(_, session)| session_group(session) == group)
}

/// Returns the grouped section where a session should be displayed.
fn session_group(session: &Session) -> SessionGroup {
    match session.status {
        Status::Queued | Status::Merging => SessionGroup::MergeQueue,
        Status::Done | Status::Canceled => SessionGroup::Archive,
        _ => SessionGroup::ActiveSessions,
    }
}

/// Resolves the selected session id from the original session ordering.
fn selected_session_id(sessions: &[Session], selected_index: Option<usize>) -> Option<&str> {
    selected_index
        .and_then(|index| sessions.get(index))
        .map(|session| session.id.as_str())
}

/// Maps selected session id to the grouped table row index.
fn selected_render_row(
    rows: &[SessionTableRow<'_>],
    selected_session_id: Option<&str>,
) -> Option<usize> {
    let selected_session_id = selected_session_id?;

    rows.iter().position(|row| match row {
        SessionTableRow::GroupLabel(_) | SessionTableRow::EmptyGroupPlaceholder => false,
        SessionTableRow::Session(session) => session.id == selected_session_id,
    })
}

/// Converts one grouped row descriptor into a `ratatui` table row.
fn render_table_row(row: &SessionTableRow<'_>, title_column_width: usize) -> Row<'static> {
    match row {
        SessionTableRow::GroupLabel(group) => render_group_label_row(*group),
        SessionTableRow::EmptyGroupPlaceholder => render_empty_group_placeholder_row(),
        SessionTableRow::Session(session) => render_session_row(session, title_column_width),
    }
}

/// Renders a non-selectable group label row.
fn render_group_label_row(group: SessionGroup) -> Row<'static> {
    let cells = vec![
        Cell::from(group.label()).style(Style::default().fg(Color::Cyan)),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Renders a non-selectable placeholder row for empty groups.
fn render_empty_group_placeholder_row() -> Row<'static> {
    let cells = vec![
        Cell::from(GROUP_EMPTY_PLACEHOLDER).style(Style::default().fg(Color::DarkGray)),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Renders one session row.
fn render_session_row(session: &Session, title_column_width: usize) -> Row<'static> {
    let status = session.status;
    let display_title = truncate_with_ellipsis(session.display_title(), title_column_width);
    let cells = vec![
        Cell::from(display_title),
        Cell::from(session.project_name.clone()),
        Cell::from(session.model.as_str()),
        Cell::from(session.permission_mode.display_label()),
        Cell::from(session.size.to_string()).style(Style::default().fg(size_color(session.size))),
        Cell::from(format!("{status}")).style(Style::default().fg(status.color())),
    ];

    Row::new(cells).height(1)
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
            Status::Queued,
            Status::Merging,
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
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::session::SessionStats;

    fn test_session(id: &str, status: Status) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::new(),
            id: id.to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            permission_mode: PermissionMode::AutoEdit,
            project_name: "project".to_string(),
            prompt: String::new(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: Some(id.to_string()),
            updated_at: 0,
        }
    }

    #[test]
    fn test_grouped_session_indexes_orders_selectable_sessions_without_headers() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("done-1", Status::Done),
            test_session("canceled-1", Status::Canceled),
            test_session("active-2", Status::New),
        ];

        // Act
        let indexes = grouped_session_indexes(&sessions);
        let ordered_ids = indexes
            .into_iter()
            .map(|index| sessions[index].id.clone())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            ordered_ids,
            vec![
                "queued-1".to_string(),
                "merge-1".to_string(),
                "active-1".to_string(),
                "active-2".to_string(),
                "done-1".to_string(),
                "canceled-1".to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_orders_merge_queue_before_active_and_archive_sessions() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("done-1", Status::Done),
            test_session("canceled-1", Status::Canceled),
            test_session("active-2", Status::New),
        ];

        // Act
        let rows = grouped_session_rows(&sessions);
        let labels_and_ids = rows
            .iter()
            .map(|row| match row {
                SessionTableRow::GroupLabel(group) => group.label().to_string(),
                SessionTableRow::EmptyGroupPlaceholder => GROUP_EMPTY_PLACEHOLDER.to_string(),
                SessionTableRow::Session(session) => session.id.clone(),
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            labels_and_ids,
            vec![
                SessionGroup::MergeQueue.label().to_string(),
                "queued-1".to_string(),
                "merge-1".to_string(),
                SessionGroup::ActiveSessions.label().to_string(),
                "active-1".to_string(),
                "active-2".to_string(),
                SessionGroup::Archive.label().to_string(),
                "done-1".to_string(),
                "canceled-1".to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_includes_placeholder_for_groups_without_sessions() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("active-2", Status::InProgress),
        ];

        // Act
        let rows = grouped_session_rows(&sessions);
        let labels_and_ids = rows
            .iter()
            .map(|row| match row {
                SessionTableRow::GroupLabel(group) => group.label().to_string(),
                SessionTableRow::EmptyGroupPlaceholder => GROUP_EMPTY_PLACEHOLDER.to_string(),
                SessionTableRow::Session(session) => session.id.clone(),
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            labels_and_ids,
            vec![
                SessionGroup::MergeQueue.label().to_string(),
                GROUP_EMPTY_PLACEHOLDER.to_string(),
                SessionGroup::ActiveSessions.label().to_string(),
                "active-1".to_string(),
                "active-2".to_string(),
                SessionGroup::Archive.label().to_string(),
                GROUP_EMPTY_PLACEHOLDER.to_string(),
            ]
        );
    }

    #[test]
    fn test_selected_render_row_maps_original_selection_to_grouped_index() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("active-2", Status::New),
        ];
        let rows = grouped_session_rows(&sessions);
        let selected_session_id = selected_session_id(&sessions, Some(3));

        // Act
        let row_index = selected_render_row(&rows, selected_session_id);

        // Assert
        assert_eq!(row_index, Some(5));
    }

    #[test]
    fn test_prepare_grouped_table_state_resets_offset_and_sets_selected_group_row() {
        // Arrange
        let mut table_state = TableState::default();
        *table_state.offset_mut() = 24;
        table_state.select(Some(7));

        // Act
        prepare_grouped_table_state(&mut table_state, Some(3));

        // Assert
        assert_eq!(table_state.offset(), 0);
        assert_eq!(table_state.selected(), Some(3));
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
        let expected_width = u16::try_from("claude-sonnet-4-6".chars().count()).unwrap_or(u16::MAX);
        let models = ["gpt-5.2-codex", "claude-sonnet-4-6"];

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
        let expected_width = u16::try_from("InProgress".chars().count()).unwrap_or(u16::MAX);

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
