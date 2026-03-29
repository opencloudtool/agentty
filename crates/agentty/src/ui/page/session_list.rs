use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::domain::session::{Session, SessionSize, Status};
use crate::ui::state::help_action;
use crate::ui::util::{first_table_column_width, format_duration_compact, truncate_with_ellipsis};
use crate::ui::{Page, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
/// Horizontal spacing between table columns in the session list.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Shared page margin that keeps table and footer spacing aligned with other
/// pages.
const PAGE_MARGIN: u16 = 1;
/// Placeholder text rendered under group headers with no sessions.
const GROUP_EMPTY_PLACEHOLDER: &str = "No sessions...";

/// Session list page renderer.
pub struct SessionListPage<'a> {
    /// Session rows available for rendering.
    pub sessions: &'a [Session],
    /// Table selection state tied to the raw session ordering.
    pub table_state: &'a mut TableState,
    /// Current wall-clock time expressed as Unix seconds for live timer labels.
    wall_clock_unix_seconds: i64,
}

impl<'a> SessionListPage<'a> {
    /// Creates a session list page renderer.
    pub fn new(
        sessions: &'a [Session],
        table_state: &'a mut TableState,
        wall_clock_unix_seconds: i64,
    ) -> Self {
        Self {
            sessions,
            table_state,
            wall_clock_unix_seconds,
        }
    }

    /// Splits the available page area into main and footer regions using the
    /// shared page margin convention.
    fn content_chunks(area: Rect) -> (Rect, Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(PAGE_MARGIN)
            .split(area);

        (chunks[0], chunks[1])
    }
}

impl Page for SessionListPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let (main_area, footer_area) = Self::content_chunks(area);

        let selected_style = Style::default().bg(style::palette::SURFACE);
        let header_style = Style::default()
            .bg(style::palette::SURFACE)
            .fg(style::palette::TEXT_MUTED)
            .add_modifier(Modifier::BOLD);
        let header_cells = ["Session", "Model", "Size", "Status", "Timer"]
            .iter()
            .map(|h| Cell::from(*h));
        let header = Row::new(header_cells)
            .style(header_style)
            .height(1)
            .bottom_margin(1);

        let block = Block::default().borders(Borders::ALL).title("Sessions");
        let column_constraints = [
            Constraint::Fill(1),
            model_column_width(self.sessions),
            size_column_width(),
            status_column_width(),
            timer_column_width(self.sessions, self.wall_clock_unix_seconds),
        ];
        let title_column_width = first_table_column_width(
            block.inner(main_area).width,
            &column_constraints,
            TABLE_COLUMN_SPACING,
            0,
        );
        let table_rows = grouped_session_rows(self.sessions);
        let selected_session_id = selected_session_id(self.sessions, self.table_state.selected());
        let selected_row = selected_render_row(&table_rows, selected_session_id);
        let rows = table_rows.iter().map(|table_row| {
            render_table_row(table_row, title_column_width, self.wall_clock_unix_seconds)
        });
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

        let selected_session = self
            .table_state
            .selected()
            .and_then(|selected_index| self.sessions.get(selected_index));
        let help_message = Paragraph::new(session_list_help_line(selected_session));
        f.render_widget(help_message, footer_area);
    }
}

/// Builds footer help content for session list mode.
fn session_list_help_line(selected_session: Option<&Session>) -> Line<'static> {
    let can_open_selected_session = selected_session.is_some();
    let actions = help_action::session_list_footer_actions(can_open_selected_session);

    help_action::footer_line(&actions)
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

/// Resolves the initial raw-session selection index for list-mode focus.
///
/// Active sessions are preferred so opening the session list lands on ongoing
/// work when both active and archived items are present. If no active sessions
/// exist, this falls back to the first selectable grouped row.
pub(crate) fn preferred_initial_session_index(sessions: &[Session]) -> Option<usize> {
    sessions_for_group(sessions, SessionGroup::ActiveSessions)
        .map(|(index, _)| index)
        .next()
        .or_else(|| grouped_session_indexes(sessions).first().copied())
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
fn render_table_row(
    row: &SessionTableRow<'_>,
    title_column_width: usize,
    wall_clock_unix_seconds: i64,
) -> Row<'static> {
    match row {
        SessionTableRow::GroupLabel(group) => render_group_label_row(*group),
        SessionTableRow::EmptyGroupPlaceholder => render_empty_group_placeholder_row(),
        SessionTableRow::Session(session) => {
            render_session_row(session, title_column_width, wall_clock_unix_seconds)
        }
    }
}

/// Renders a non-selectable group label row.
fn render_group_label_row(group: SessionGroup) -> Row<'static> {
    let cells = vec![
        Cell::from(group.label()).style(Style::default().fg(style::palette::ACCENT)),
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
        Cell::from(GROUP_EMPTY_PLACEHOLDER).style(Style::default().fg(style::palette::TEXT_SUBTLE)),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ];

    Row::new(cells).height(1)
}

/// Renders one session row.
fn render_session_row(
    session: &Session,
    title_column_width: usize,
    wall_clock_unix_seconds: i64,
) -> Row<'static> {
    let status = session.status;
    let display_title = truncate_with_ellipsis(session.display_title(), title_column_width);
    let timer_label = if session.has_in_progress_timer() {
        format_duration_compact(session.in_progress_duration_seconds(wall_clock_unix_seconds))
    } else {
        String::new()
    };
    let cells = vec![
        Cell::from(display_title),
        Cell::from(session.model.as_str()),
        Cell::from(session.size.to_string()).style(Style::default().fg(size_color(session.size))),
        Cell::from(format!("{status}")).style(Style::default().fg(style::status_color(status))),
        Cell::from(timer_label),
    ];

    Row::new(cells).height(1)
}

/// Calculates the width of the project column from known session values.
pub(crate) fn project_column_width(sessions: &[Session]) -> Constraint {
    text_column_width(
        "Project",
        sessions.iter().map(|session| session.project_name.as_str()),
    )
}

/// Calculates the width of the model column from known session values.
pub(crate) fn model_column_width(sessions: &[Session]) -> Constraint {
    text_column_width(
        "Model",
        sessions.iter().map(|session| session.model.as_str()),
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

/// Calculates the width of the timer column from known session durations.
fn timer_column_width(sessions: &[Session], wall_clock_unix_seconds: i64) -> Constraint {
    text_column_width(
        "Timer",
        sessions
            .iter()
            .filter(|session| session.has_in_progress_timer())
            .map(|session| {
                format_duration_compact(
                    session.in_progress_duration_seconds(wall_clock_unix_seconds),
                )
            }),
    )
}

/// Returns the palette color representing each session size bucket.
fn size_color(size: SessionSize) -> Color {
    match size {
        SessionSize::Xs => style::palette::SUCCESS,
        SessionSize::S => style::palette::SUCCESS_SOFT,
        SessionSize::M => style::palette::WARNING,
        SessionSize::L => style::palette::WARNING_SOFT,
        SessionSize::Xl => style::palette::DANGER_SOFT,
        SessionSize::Xxl => style::palette::DANGER,
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

    use ratatui::widgets::TableState;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::session::SessionStats;

    fn test_session(id: &str, status: Status) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: id.to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: Some(id.to_string()),
            updated_at: 0,
        }
    }

    /// Flattens a rendered test buffer into a plain string for assertions.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_content_chunks_use_shared_page_margin() {
        // Arrange
        let area = Rect::new(5, 3, 40, 10);

        // Act
        let (main_area, footer_area) = SessionListPage::content_chunks(area);

        // Assert
        assert_eq!(main_area.x, area.x + PAGE_MARGIN);
        assert_eq!(main_area.y, area.y + PAGE_MARGIN);
        assert_eq!(main_area.width, area.width - PAGE_MARGIN.saturating_mul(2));
        assert_eq!(
            main_area.height,
            area.height - PAGE_MARGIN.saturating_mul(2) - 1
        );
        assert_eq!(footer_area.x, area.x + PAGE_MARGIN);
        assert_eq!(footer_area.y, area.y + area.height - PAGE_MARGIN - 1);
        assert_eq!(
            footer_area.width,
            area.width - PAGE_MARGIN.saturating_mul(2)
        );
        assert_eq!(footer_area.height, 1);
    }

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
    fn test_table_column_spacing_is_wider_for_readability() {
        // Arrange
        let expected_spacing = 2;

        // Act
        let spacing = TABLE_COLUMN_SPACING;

        // Assert
        assert_eq!(spacing, expected_spacing);
    }

    #[test]
    fn test_preferred_initial_session_index_prefers_active_group_when_available() {
        // Arrange
        let sessions = vec![
            test_session("archive-1", Status::Done),
            test_session("active-1", Status::Review),
            test_session("merge-1", Status::Queued),
        ];

        // Act
        let selected_index = preferred_initial_session_index(&sessions);

        // Assert
        assert_eq!(selected_index, Some(1));
    }

    #[test]
    fn test_preferred_initial_session_index_falls_back_to_first_grouped_session() {
        // Arrange
        let sessions = vec![
            test_session("archive-1", Status::Done),
            test_session("merge-1", Status::Queued),
        ];

        // Act
        let selected_index = preferred_initial_session_index(&sessions);

        // Assert
        assert_eq!(selected_index, Some(1));
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
        let models = ["gpt-5.3-codex", "claude-sonnet-4-6"];

        // Act
        let width = text_column_width("Model", models.into_iter());

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
    fn test_timer_column_width_uses_longest_rendered_timer_label() {
        // Arrange
        let mut active_session = test_session("active-1", Status::InProgress);
        active_session.in_progress_started_at = Some(100);
        active_session.in_progress_total_seconds = 60;
        let mut archived_session = test_session("done-1", Status::Done);
        archived_session.in_progress_total_seconds = 3_661;
        let sessions = vec![active_session, archived_session];
        let expected_width = u16::try_from("1h1m1s".chars().count()).unwrap_or(u16::MAX);

        // Act
        let width = timer_column_width(&sessions, 160);

        // Assert
        assert_eq!(width, Constraint::Length(expected_width));
    }

    #[test]
    fn test_size_color_uses_expected_palette() {
        // Arrange
        let test_cases = [
            (SessionSize::Xs, style::palette::SUCCESS),
            (SessionSize::S, style::palette::SUCCESS_SOFT),
            (SessionSize::M, style::palette::WARNING),
            (SessionSize::L, style::palette::WARNING_SOFT),
            (SessionSize::Xl, style::palette::DANGER_SOFT),
            (SessionSize::Xxl, style::palette::DANGER),
        ];

        // Act & Assert
        for (size, expected_color) in test_cases {
            assert_eq!(size_color(size), expected_color);
        }
    }

    #[test]
    fn test_session_list_help_line_includes_sync_for_non_empty_sessions() {
        // Arrange
        let session = test_session("session-1", Status::Review);

        // Act
        let help_text = session_list_help_line(Some(&session)).to_string();

        // Assert
        assert!(help_text.contains("s: sync"));
    }

    #[test]
    fn test_session_list_help_line_hides_cancel_for_non_review_session() {
        // Arrange
        let session = test_session("session-1", Status::Done);

        // Act
        let help_text = session_list_help_line(Some(&session)).to_string();

        // Assert
        assert!(!help_text.contains("c: cancel"));
    }

    #[test]
    fn test_session_list_help_line_includes_open_for_canceled_session() {
        // Arrange
        let session = test_session("session-1", Status::Canceled);

        // Act
        let help_text = session_list_help_line(Some(&session)).to_string();

        // Assert
        assert!(help_text.contains("Enter: open session"));
    }

    #[test]
    fn test_render_shows_live_active_work_timer_in_grouped_session_row() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("active-1", Status::InProgress);
        session.in_progress_started_at = Some(100);
        session.in_progress_total_seconds = 60;
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, 160).render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("2m0s"));
    }

    #[test]
    fn test_render_shows_frozen_completed_timer_in_grouped_session_row() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut session = test_session("done-1", Status::Done);
        session.in_progress_total_seconds = 125;
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                SessionListPage::new(&sessions, &mut table_state, 9_999)
                    .render(frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("2m5s"));
    }
}
