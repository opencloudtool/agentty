use ag_tui_text::text_util;
use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::ui::input_layout;

const APP_FRAME_BAR_HEIGHT: u16 = 1;
const CHAT_INPUT_MIN_PANEL_HEIGHT: u16 = CHAT_INPUT_BORDER_HEIGHT + 1;
const CHAT_INPUT_BORDER_HEIGHT: u16 = 2;
const QUESTION_PANEL_HELP_HEIGHT: u16 = 1;
const QUESTION_PANEL_SPACER_HEIGHT: u16 = 1;
const SESSION_HEADER_HEIGHT_MIN: u16 = 2;
const TAB_PAGE_INSET: u16 = 1;
const SINGLE_LINE_FOOTER_HEIGHT: u16 = 1;

/// Height allocation for question mode's prompt, answer input, and footer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuestionPanelLayout {
    /// Height reserved for the trailing help footer row.
    pub help_height: u16,
    /// Height reserved for the bordered answer input widget.
    pub input_height: u16,
    /// Height reserved for the question title and wrapped question text.
    pub question_height: u16,
    /// Height reserved for the blank spacer between question and input.
    pub spacer_height: u16,
}

/// Concrete sub-areas used to paint the question-mode bottom panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuestionPanelAreas {
    /// Area used for the footer help text row.
    pub help_area: Rect,
    /// Area used for the bordered answer input widget.
    pub input_area: Rect,
    /// Area used for the predefined answer options.
    pub options_area: Rect,
    /// Area used for the question title and wrapped question text.
    pub question_area: Rect,
    /// Area used for the blank spacer between question text and input.
    pub spacer_area: Rect,
}

/// Top-level frame areas for one rendered session chat page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionChatAreas {
    /// Area used for the footer/help row below the session transcript.
    pub bottom_area: Rect,
    /// Area used for the title and metadata header.
    pub header_area: Rect,
    /// Area used for the bordered transcript/output panel.
    pub output_area: Rect,
}

/// Sub-areas used to render the prompt composer and its footer help row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptPanelAreas {
    /// Area used for the footer/help row below the composer.
    pub footer_area: Rect,
    /// Area used for the bordered prompt input widget.
    pub input_area: Rect,
}

/// Vertical bands of one rendered app frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppFrameAreas {
    /// Area routed to the active page between the two bars.
    pub content_area: Rect,
    /// Area used for the bottom footer bar.
    pub footer_bar_area: Rect,
    /// Area used for the top status bar.
    pub status_bar_area: Rect,
}

/// Content and footer areas used by top-level tab pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabPageAreas {
    /// Area used for the one-line footer below the tab page content.
    pub footer_area: Rect,
    /// Area used for the tab page's primary panels or tables.
    pub main_area: Rect,
}

/// Split an area into a centered content column with side gutters.
pub fn centered_horizontal_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(2),
            Constraint::Percentage(80),
            Constraint::Min(2),
        ])
        .split(area)
}

/// Returns a fixed-size content area centered within the available `area`.
///
/// The returned rectangle is clamped to the bounds of `area` so callers can
/// safely request a preferred content size even when the terminal is smaller.
pub fn centered_content_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let origin_x = area.x + area.width.saturating_sub(width) / 2;
    let origin_y = area.y + area.height.saturating_sub(height) / 2;

    Rect::new(origin_x, origin_y, width, height)
}

/// Splits the terminal frame into status bar, page content, and footer bar.
///
/// Runtime scroll math reuses this split so key handlers derive page geometry
/// from the same area the renderer paints into.
pub fn app_frame_areas(area: Rect) -> AppFrameAreas {
    let vertical_chunks = Layout::default()
        .constraints([
            Constraint::Length(APP_FRAME_BAR_HEIGHT),
            Constraint::Min(0),
            Constraint::Length(APP_FRAME_BAR_HEIGHT),
        ])
        .split(area);

    AppFrameAreas {
        content_area: vertical_chunks[1],
        footer_bar_area: vertical_chunks[2],
        status_bar_area: vertical_chunks[0],
    }
}

/// Splits a tab page area so content starts directly below the tab header.
///
/// The tab header already provides the vertical separation through its bottom
/// border, so tab pages keep only side and bottom insets. This prevents a
/// blank row between the header border and the first table or panel while
/// preserving the one-line footer convention.
pub fn tab_page_areas(area: Rect) -> TabPageAreas {
    let horizontal_inset = TAB_PAGE_INSET.min(area.width / 2);
    let content_x = area.x.saturating_add(horizontal_inset);
    let content_width = area
        .width
        .saturating_sub(horizontal_inset.saturating_mul(2));
    let footer_y = area
        .y
        .saturating_add(area.height.saturating_sub(TAB_PAGE_INSET + 1));
    let main_height = area.height.saturating_sub(TAB_PAGE_INSET + 1);

    TabPageAreas {
        footer_area: Rect::new(content_x, footer_y, content_width, 1.min(area.height)),
        main_area: Rect::new(content_x, area.y, content_width, main_height),
    }
}

/// Splits one session chat page into header, transcript, and bottom panel.
///
/// The outer frame keeps a one-cell margin, reserves `bottom_height` for the
/// prompt/help region, and then dedicates the requested header rows above the
/// bordered session output panel.
pub fn session_chat_areas(area: Rect, bottom_height: u16, header_height: u16) -> SessionChatAreas {
    let header_height = header_height.max(SESSION_HEADER_HEIGHT_MIN);
    let vertical_chunks = Layout::default()
        .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
        .margin(1)
        .split(area);
    let output_chunks = Layout::default()
        .constraints([Constraint::Length(header_height), Constraint::Min(0)])
        .split(vertical_chunks[0]);

    SessionChatAreas {
        bottom_area: vertical_chunks[1],
        header_area: output_chunks[0],
        output_area: output_chunks[1],
    }
}

/// Splits one prompt bottom panel into input and footer rows.
///
/// Panels that only have one visible row keep the whole area for the input and
/// collapse the footer to height `0`.
pub fn prompt_panel_areas(area: Rect) -> PromptPanelAreas {
    if area.height <= SINGLE_LINE_FOOTER_HEIGHT {
        return PromptPanelAreas {
            footer_area: Rect::new(area.x, area.y.saturating_add(area.height), area.width, 0),
            input_area: area,
        };
    }

    let sections = Layout::default()
        .constraints([
            Constraint::Min(0),
            Constraint::Length(SINGLE_LINE_FOOTER_HEIGHT),
        ])
        .split(area);

    PromptPanelAreas {
        footer_area: sections[1],
        input_area: sections[0],
    }
}

/// Returns how many lookup entries can fit above the question input overlay.
pub(crate) fn question_at_mention_max_visible(
    bottom_area: Rect,
    input_area: Rect,
    default_max_visible: usize,
) -> usize {
    usize::from(input_area.y.saturating_sub(bottom_area.y))
        .saturating_sub(2)
        .clamp(1, default_max_visible)
}

/// Calculates the height split for question mode's prompt, input, and footer.
///
/// The answer input keeps its visible content row whenever `available_height`
/// allows it. When space is tight, the wrapped question text yields height
/// before the spacer row and bordered input collapse.
pub fn question_panel_layout(
    width: u16,
    available_height: u16,
    question: &str,
    input: &str,
    max_input_panel_height: u16,
) -> QuestionPanelLayout {
    let question_text_height = wrapped_text_height(question, width);
    // +1 for the "Question N/M" title line when there is question text.
    let requested_question_height = if question_text_height > 0 {
        question_text_height.saturating_add(1)
    } else {
        0
    };
    let requested_input_height = input_layout::calculate_input_height(width, input)
        .min(max_input_panel_height.max(CHAT_INPUT_MIN_PANEL_HEIGHT));
    let requested_spacer_height = if requested_question_height > 0 {
        QUESTION_PANEL_SPACER_HEIGHT
    } else {
        0
    };
    let preferred_total_height = requested_question_height
        .saturating_add(requested_input_height)
        .saturating_add(requested_spacer_height)
        .saturating_add(QUESTION_PANEL_HELP_HEIGHT);
    let total_height = preferred_total_height.min(available_height);
    let help_height = total_height.min(QUESTION_PANEL_HELP_HEIGHT);
    let remaining_height = total_height.saturating_sub(help_height);
    let input_height = remaining_height.min(requested_input_height);
    let question_and_spacer_height = remaining_height.saturating_sub(input_height);
    let spacer_height =
        if question_and_spacer_height >= requested_question_height + requested_spacer_height {
            requested_spacer_height
        } else {
            0
        };
    let question_height = question_and_spacer_height.saturating_sub(spacer_height);

    QuestionPanelLayout {
        help_height,
        input_height,
        question_height,
        spacer_height,
    }
}

/// Returns the total height reserved for a question-mode bottom panel.
///
/// The caller supplies the maximum panel height budget; this helper accounts
/// for question text, predefined options, the answer input, spacer, and help
/// footer using the same allocation logic used during rendering.
pub fn question_panel_reserved_height(
    width: u16,
    available_height: u16,
    question: &str,
    input: &str,
    option_count: usize,
    max_input_panel_height: u16,
) -> u16 {
    let options_height = question_options_height(option_count, available_height);
    let panel_layout = question_panel_layout(
        width,
        available_height.saturating_sub(options_height),
        question,
        input,
        max_input_panel_height,
    );

    panel_layout
        .question_height
        .saturating_add(options_height)
        .saturating_add(panel_layout.spacer_height)
        .saturating_add(panel_layout.input_height)
        .saturating_add(panel_layout.help_height)
}

/// Splits an already reserved question-mode bottom area into paintable rows.
///
/// The returned rectangles mirror [`question_panel_reserved_height`] so render
/// code can consume precomputed geometry instead of repeating the area math.
pub fn question_panel_areas(
    area: Rect,
    question: &str,
    input: &str,
    option_count: usize,
    max_input_panel_height: u16,
) -> QuestionPanelAreas {
    let options_height = question_options_height(option_count, area.height);
    let panel_layout = question_panel_layout(
        area.width,
        area.height.saturating_sub(options_height),
        question,
        input,
        max_input_panel_height,
    );
    let chunks = Layout::default()
        .constraints([
            Constraint::Length(panel_layout.question_height),
            Constraint::Length(options_height),
            Constraint::Length(panel_layout.spacer_height),
            Constraint::Length(panel_layout.input_height),
            Constraint::Length(panel_layout.help_height),
        ])
        .split(area);

    QuestionPanelAreas {
        help_area: chunks[4],
        input_area: chunks[3],
        options_area: chunks[1],
        question_area: chunks[0],
        spacer_area: chunks[2],
    }
}

/// Returns the total height for a question-mode options section.
///
/// The section contains one header row plus one row per predefined option and
/// clamps to `max_height` so callers can preserve space for surrounding UI.
pub fn question_options_height(option_count: usize, max_height: u16) -> u16 {
    if option_count == 0 {
        return 0;
    }

    u16::try_from(option_count)
        .unwrap_or(u16::MAX)
        .saturating_add(1)
        .min(max_height)
}

/// Returns the wrapped line count for plain text rendered in a paragraph.
fn wrapped_text_height(text: &str, width: u16) -> u16 {
    let wrapped_line_count = text_util::wrap_lines(text, usize::from(width.max(1))).len();

    u16::try_from(wrapped_line_count).unwrap_or(u16::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::style::{Modifier, Style};
    use ratatui::widgets::Borders;

    use super::*;
    use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
    use crate::domain::file_entry::FileEntry;
    use crate::domain::input::InputState;
    use crate::domain::session::{COMMITTING_PROGRESS_LABEL, Session, SessionDiffState, Status};
    use crate::domain::theme::ColorTheme;
    use crate::presentation::app_mode::ChatFocus;
    use crate::presentation::help_action::{self, ViewActionAvailability, ViewHelpState};
    use crate::presentation::prompt::{PromptAtMentionState, PromptSlashStage, PromptSlashState};
    use crate::ui::icon::Icon;
    use crate::ui::input_layout::*;
    use crate::ui::prompt_format::*;
    use crate::ui::question_format::*;
    use crate::ui::session_format::*;
    use crate::ui::style;

    fn session_fixture() -> Session {
        crate::test_support::SessionFixtureBuilder::new()
            .status(Status::Draft)
            .build()
    }

    fn view_footer_text(session: &Session, can_open_worktree: bool) -> String {
        session_view_footer_line(ViewHelpState {
            can_fork_session: ViewActionAvailability::from_bool(session.allows_fork_action()),
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::from_bool(can_open_worktree),
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_show_diff: ViewActionAvailability::from_bool(session.stats.should_show_diff()),
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::from_bool(
                session.can_start_staged_session(),
            ),
            publish_pull_request_action: session.publish_pull_request_action(),
            session_state: help_action::session_view_state(session),
        })
        .to_string()
    }

    fn file_entries_fixture() -> Vec<FileEntry> {
        vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: true,
                path: "tests".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "Cargo.toml".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "README.md".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "tests/integration.rs".to_string(),
            },
        ]
    }

    #[test]
    fn test_app_frame_areas_reserve_status_and_footer_bars() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);

        // Act
        let frame_areas = app_frame_areas(area);

        // Assert
        assert_eq!(frame_areas.status_bar_area, Rect::new(0, 0, 80, 1));
        assert_eq!(frame_areas.content_area, Rect::new(0, 1, 80, 22));
        assert_eq!(frame_areas.footer_bar_area, Rect::new(0, 23, 80, 1));
    }

    #[test]
    fn test_session_chat_areas_reserve_margin_header_and_bottom_panel() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);

        // Act
        let chat_areas = session_chat_areas(area, 5, 2);

        // Assert
        assert_eq!(chat_areas.header_area, Rect::new(1, 1, 78, 2));
        assert_eq!(chat_areas.output_area, Rect::new(1, 3, 78, 15));
        assert_eq!(chat_areas.bottom_area, Rect::new(1, 18, 78, 5));
    }

    #[test]
    fn test_session_chat_areas_expand_for_three_line_header() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);

        // Act
        let chat_areas = session_chat_areas(area, 5, 3);

        // Assert
        assert_eq!(chat_areas.header_area, Rect::new(1, 1, 78, 3));
        assert_eq!(chat_areas.output_area, Rect::new(1, 4, 78, 14));
        assert_eq!(chat_areas.bottom_area, Rect::new(1, 18, 78, 5));
    }

    #[test]
    fn test_prompt_panel_areas_split_input_and_footer_rows() {
        // Arrange
        let area = Rect::new(3, 10, 50, 6);

        // Act
        let panel_areas = prompt_panel_areas(area);

        // Assert
        assert_eq!(panel_areas.input_area, Rect::new(3, 10, 50, 5));
        assert_eq!(panel_areas.footer_area, Rect::new(3, 15, 50, 1));
    }

    #[test]
    fn test_prompt_panel_areas_collapse_footer_when_only_one_row_fits() {
        // Arrange
        let area = Rect::new(3, 10, 50, 1);

        // Act
        let panel_areas = prompt_panel_areas(area);

        // Assert
        assert_eq!(panel_areas.input_area, area);
        assert_eq!(panel_areas.footer_area, Rect::new(3, 11, 50, 0));
    }

    #[test]
    fn test_session_header_lines_truncate_long_titles_and_keep_metadata() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;
        session.title = Some("This is a very long timer-aware session header title".to_string());
        session.agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            AgentModel::Gpt56Sol,
        );
        session.in_progress_started_at = Some(0);

        // Act
        let header_lines =
            session_header_lines(&session, 50, ReasoningLevel::default(), 3_660, false);

        // Assert
        assert_eq!(header_lines.len(), 2);
        assert!(header_lines[0].to_string().contains("..."));
        assert!(header_lines[1].to_string().contains("Timer: 1h 1m 0s"));
    }

    #[test]
    fn test_session_header_lines_use_theme_status_color() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Green);
        let mut session = session_fixture();
        session.status = Status::Canceled;
        session.title = Some("Canceled session".to_string());

        // Act
        let header_lines = session_header_lines(&session, 80, ReasoningLevel::default(), 0, false);

        // Assert
        assert_eq!(
            header_lines[0].spans[0].style.fg,
            Some(style::status_color(Status::Canceled))
        );
    }

    #[test]
    fn test_session_metadata_text_ticks_live_in_progress_timer() {
        // Arrange
        let mut session = session_fixture();
        session.agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            AgentModel::Gpt56Sol,
        );
        session.stats.added_lines = 9;
        session.stats.deleted_lines = 3;
        session.status = Status::InProgress;
        session.in_progress_started_at = Some(60);

        // Act
        let early_metadata = session_metadata_text(&session, 120, ReasoningLevel::default(), 90);
        let later_metadata = session_metadata_text(&session, 120, ReasoningLevel::default(), 3_720);

        // Assert
        assert!(early_metadata.contains("Lines: +9 / -3"));
        assert!(early_metadata.contains("Timer: 30s"));
        assert!(early_metadata.contains("Model: gpt-5.6-sol"));
        assert!(
            early_metadata.find("Model: gpt-5.6-sol") < early_metadata.find("Reasoning: high"),
            "model should appear before reasoning in metadata text"
        );
        assert!(later_metadata.contains("Timer: 1h 1m 0s"));
    }

    #[test]
    fn test_session_metadata_text_freezes_timer_after_in_progress_ends() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        session.in_progress_total_seconds = 3_660;

        // Act
        let earlier_metadata =
            session_metadata_text(&session, 80, ReasoningLevel::default(), 4_000);
        let later_metadata = session_metadata_text(&session, 80, ReasoningLevel::default(), 40_000);

        // Assert
        assert_eq!(earlier_metadata, later_metadata);
        assert!(earlier_metadata.contains("Timer: 1h 1m 0s"));
    }

    #[test]
    fn test_session_view_footer_line_in_progress_shows_stop_and_hides_open_and_diff() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;

        // Act
        let help_text = view_footer_text(&session, true);

        // Assert
        assert!(help_text.contains("q: back"));
        assert!(help_text.contains("Ctrl+c: stop"));
        assert!(help_text.contains("j/k: scroll"));
        assert!(!help_text.contains("o: open"));
        assert!(!help_text.contains("d: diff"));
        assert!(!help_text.contains("Enter: reply"));
    }

    #[test]
    fn test_session_view_footer_line_hides_known_empty_diff() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        session.stats.diff_state = SessionDiffState::Empty;

        // Act
        let help_text = view_footer_text(&session, true);

        // Assert
        assert!(!help_text.contains("d: diff"));
        assert!(help_text.contains("Enter: reply"));
    }

    #[test]
    fn test_session_view_footer_line_noninteractive_busy_statuses_hide_worktree_open_hint() {
        // Arrange
        let busy_statuses = [Status::Rebasing, Status::Queued, Status::Merging];

        // Act
        let help_texts: Vec<String> = busy_statuses
            .iter()
            .map(|session_status| {
                let mut session = session_fixture();
                session.status = *session_status;

                view_footer_text(&session, true)
            })
            .collect();

        // Assert
        for help_text in help_texts {
            assert!(help_text.contains("q: back"));
            assert!(help_text.contains("j/k: scroll"));
            assert!(!help_text.contains("o: open"));
            assert!(!help_text.contains("Ctrl+c: stop"));
        }
    }

    #[test]
    fn test_session_view_footer_line_new_session_shows_draft_and_start_actions() {
        // Arrange
        let mut session = session_fixture();
        session.folder = PathBuf::new();
        session.is_draft = true;
        session.prompt = "Staged draft".to_string();

        // Act
        let help_text = view_footer_text(&session, false);

        // Assert
        assert!(help_text.contains("Enter: add draft"));
        assert!(help_text.contains("s: start"));
        assert!(!help_text.contains("o: open"));
        assert!(help_text.contains("m: add to merge queue"));
        assert!(!help_text.contains("r: sync"));
        assert!(help_text.contains("/: commands menu"));
        assert!(!help_text.contains("d: diff"));
    }

    #[test]
    fn test_session_view_footer_line_done_session_shows_continue_action() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Done;

        // Act
        let help_text = view_footer_text(&session, true);

        // Assert
        assert!(help_text.contains("c: continue"));
        assert!(!help_text.contains("comments"));
    }

    #[test]
    fn test_prompt_footer_line_shows_highlighted_actions_and_attachment_count() {
        // Arrange
        let session = session_fixture();

        // Act
        let footer_line = prompt_footer_line(&session, 2, ChatFocus::Input);

        // Assert
        assert_eq!(
            footer_line.to_string(),
            "Tab: focus | Enter: send | Alt+Enter: newline | Ctrl+V/Alt+V: paste image | Esc: \
             cancel | Shift+Tab: switch mode | 2 images ready"
        );
        assert_eq!(
            footer_line.spans[0].style,
            Style::default()
                .fg(style::palette::accent())
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            footer_line.spans[1].style,
            Style::default().fg(style::palette::text_muted())
        );
        assert_eq!(
            footer_line.spans[footer_line.spans.len() - 2].style,
            Style::default().fg(style::palette::text_subtle())
        );
        assert_eq!(
            footer_line.spans[footer_line.spans.len() - 1].style,
            Style::default().fg(style::palette::text_muted())
        );
    }

    #[test]
    fn test_prompt_footer_line_uses_stage_label_for_new_sessions() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Draft;
        session.is_draft = true;

        // Act
        let footer_line = prompt_footer_line(&session, 0, ChatFocus::Input);

        // Assert
        assert!(footer_line.to_string().contains("Enter: stage draft"));
        assert!(!footer_line.to_string().contains("Enter: send"));
    }

    #[test]
    fn test_prompt_footer_line_shows_scroll_actions_while_chat_is_focused() {
        // Arrange
        let mut session = session_fixture();
        session.stats.diff_state = SessionDiffState::Empty;

        // Act
        let unchanged_footer_line = prompt_footer_line(&session, 0, ChatFocus::Chat);
        session.stats.diff_state = SessionDiffState::Present;
        let changed_footer_line = prompt_footer_line(&session, 0, ChatFocus::Chat);

        // Assert
        assert_eq!(
            unchanged_footer_line.to_string(),
            "Tab: focus | j/k: scroll | q: sessions"
        );
        assert_eq!(
            changed_footer_line.to_string(),
            "Tab: focus | j/k: scroll | d: diff | q: sessions"
        );
    }

    #[test]
    fn test_prompt_suggestion_list_for_command_stage_has_description() {
        // Arrange
        let input = InputState::with_text("/model".to_string());
        let slash_state = PromptSlashState::default();

        // Act
        let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 1);
        assert_eq!(menu.items[0].label, "/model");
        assert_eq!(
            menu.items[0].detail,
            Some("Choose an agent and model for this session.".to_string())
        );
    }

    #[test]
    fn test_prompt_suggestion_list_for_agent_stage_has_agent_descriptions() {
        // Arrange
        let input = InputState::with_text("/model".to_string());
        let slash_state = PromptSlashState {
            stage: PromptSlashStage::Agent,
            ..PromptSlashState::default()
        };

        // Act
        let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), AgentKind::ALL.len());
        assert_eq!(menu.items[0].label, "gemini");
        assert_eq!(
            menu.items[0].detail,
            Some("Google Gemini CLI agent.".to_string())
        );
    }

    #[test]
    fn test_prompt_suggestion_list_for_agent_stage_filters_available_agents() {
        // Arrange
        let input = InputState::with_text("/model".to_string());
        let mut slash_state = PromptSlashState::with_available_agent_kinds(vec![AgentKind::Codex]);
        slash_state.stage = PromptSlashStage::Agent;

        // Act
        let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 1);
        assert_eq!(menu.items[0].label, "codex");
    }

    #[test]
    fn test_prompt_suggestion_list_for_model_stage_has_model_descriptions() {
        // Arrange
        let input = InputState::with_text("/model".to_string());
        let slash_state = PromptSlashState {
            stage: PromptSlashStage::Model,
            selected_agent: Some(AgentKind::Codex),
            ..PromptSlashState::default()
        };

        // Act
        let menu = prompt_suggestion_list(&input, &slash_state, None, AgentKind::Codex, true)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), AgentKind::Codex.models().len());
        assert_eq!(menu.items[0].label, "gpt-6-astra");
        assert_eq!(
            menu.items[0].detail,
            Some("Most capable Codex model for the hardest end-to-end work.".to_string())
        );
    }

    #[test]
    fn test_prompt_suggestion_list_for_command_stage_includes_commands() {
        // Arrange
        let input = InputState::with_text("/".to_string());

        // Act
        let menu = prompt_suggestion_list(
            &input,
            &PromptSlashState::default(),
            None,
            AgentKind::Codex,
            true,
        )
        .expect("expected suggestion list");
        let labels: Vec<&str> = menu.items.iter().map(|item| item.label.as_str()).collect();

        // Assert
        assert!(labels.contains(&"/model"));
        assert!(labels.contains(&"/reasoning"));
    }

    #[test]
    fn test_prompt_suggestion_list_for_command_stage_omits_disabled_apply() {
        // Arrange
        let input = InputState::with_text("/".to_string());

        // Act
        let menu = prompt_suggestion_list(
            &input,
            &PromptSlashState::default(),
            None,
            AgentKind::Codex,
            false,
        )
        .expect("expected suggestion list");
        let labels: Vec<&str> = menu.items.iter().map(|item| item.label.as_str()).collect();

        // Assert
        assert_eq!(
            labels,
            vec![
                "/mode",
                "/model",
                "/personality",
                "/reasoning",
                "/style",
                "/speed"
            ]
        );
        assert_eq!(menu.selected_index, 0);
    }

    #[test]
    fn test_file_lookup_suggestion_list_with_matches() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu =
            file_lookup_suggestion_list("@src", 4, &state, 10).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 3);
        assert_eq!(menu.items[0].label, "src/");
        assert_eq!(menu.items[0].detail, Some("folder".to_string()));
        assert_eq!(menu.items[1].label, "src/lib.rs");
        assert_eq!(menu.items[2].label, "src/main.rs");
    }

    #[test]
    fn test_file_lookup_suggestion_list_with_trailing_slash_includes_exact_directory() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu =
            file_lookup_suggestion_list("@src/", 5, &state, 10).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items[0].label, "src/");
        assert_eq!(menu.items[0].detail, Some("folder".to_string()));
        assert_eq!(menu.items[1].label, "src/lib.rs");
        assert_eq!(menu.items[2].label, "src/main.rs");
    }

    #[test]
    fn test_file_lookup_suggestion_list_no_matches() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu = file_lookup_suggestion_list("@nonexistent", 12, &state, 10);

        // Assert
        assert!(menu.is_none());
    }

    #[test]
    fn test_file_lookup_suggestion_list_empty_query_returns_all() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu =
            file_lookup_suggestion_list("@", 1, &state, 10).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 7);
    }

    #[test]
    fn test_file_lookup_suggestion_list_caps_at_10() {
        // Arrange
        let entries: Vec<FileEntry> = (0..20)
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:02}.rs"),
            })
            .collect();
        let state = PromptAtMentionState::new(entries);

        // Act
        let menu =
            file_lookup_suggestion_list("@", 1, &state, 10).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 10);
    }

    #[test]
    fn test_file_lookup_suggestion_list_respects_capacity() {
        // Arrange
        let entries: Vec<FileEntry> = (0..20)
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:02}.rs"),
            })
            .collect();
        let state = PromptAtMentionState::new(entries);

        // Act
        let menu =
            file_lookup_suggestion_list("@", 1, &state, 5).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 5);
    }

    #[test]
    fn test_file_lookup_suggestion_list_capacity_scroll_keeps_selection_visible() {
        // Arrange
        let entries: Vec<FileEntry> = (0..20)
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:02}.rs"),
            })
            .collect();
        let mut state = PromptAtMentionState::new(entries);
        state.selected_index = 18;

        // Act
        let menu =
            file_lookup_suggestion_list("@", 1, &state, 5).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 5);
        assert_eq!(menu.items[0].label, "file_15.rs");
        assert_eq!(menu.items[4].label, "file_19.rs");
        assert_eq!(menu.selected_index, 3);
    }

    #[test]
    fn test_file_lookup_suggestion_list_clamps_selected_index() {
        // Arrange
        let mut state = PromptAtMentionState::new(file_entries_fixture());
        state.selected_index = 100;

        // Act
        let menu =
            file_lookup_suggestion_list("@src", 4, &state, 10).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.selected_index, 2);
    }

    #[test]
    fn test_file_lookup_suggestion_list_scroll_window() {
        // Arrange
        let entries: Vec<FileEntry> = (0..20)
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:02}.rs"),
            })
            .collect();
        let mut state = PromptAtMentionState::new(entries);
        state.selected_index = 15;

        // Act
        let menu =
            file_lookup_suggestion_list("@", 1, &state, 10).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items.len(), 10);
        assert_eq!(menu.items[0].label, "file_10.rs");
        assert_eq!(menu.items[9].label, "file_19.rs");
        assert_eq!(menu.selected_index, 5);
    }

    #[test]
    fn test_file_lookup_suggestion_list_directory_has_trailing_slash() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
        ];
        let state = PromptAtMentionState::new(entries);

        // Act
        let menu =
            file_lookup_suggestion_list("@src", 4, &state, 10).expect("expected suggestion list");

        // Assert
        assert_eq!(menu.items[0].label, "src/");
        assert_eq!(menu.items[0].detail, Some("folder".to_string()));
        assert_eq!(menu.items[1].label, "src/main.rs");
        assert_eq!(menu.items[1].detail, None);
    }

    #[test]
    fn test_question_panel_lines_wrap_and_style_title() {
        // Arrange
        let title = "Question 1/2";

        // Act
        let lines = question_panel_lines(title, "Need tests for the new flow?", false, 12);

        // Assert
        assert_eq!(lines[0].to_string(), title);
        assert!(lines.len() > 2);
        assert_eq!(lines[0].spans[0].style.fg, Some(style::palette::question()));
    }

    #[test]
    fn test_question_option_lines_highlight_selected_row() {
        // Arrange
        let options = vec!["Yes".to_string(), "No".to_string()];

        // Act
        let lines = question_option_lines(&options, Some(1), false);

        // Assert
        assert_eq!(lines[0].to_string(), "Options:");
        assert_eq!(lines[2].to_string(), "▸ 2. No");
        assert_eq!(lines[2].spans[0].style.bg, Some(style::palette::warning()));
    }

    #[test]
    fn test_question_at_mention_max_visible_clamps_to_available_rows() {
        // Arrange
        let bottom_area = Rect::new(1, 10, 78, 8);
        let input_area = Rect::new(1, 15, 78, 2);

        // Act
        let max_visible = question_at_mention_max_visible(bottom_area, input_area, 10);

        // Assert
        assert_eq!(max_visible, 3);
    }

    #[test]
    fn test_format_review_markdown_compacts_sections_and_appends_apply_hint() {
        // Arrange
        let review_markdown =
            "## Review\n\n### Project Impact\n\n- None\n\n### Suggestions\n\n- Fix typo.";

        // Act
        let formatted = format_review_markdown(review_markdown);

        // Assert
        assert!(formatted.starts_with("## Review\n\n### Project Impact\n- None"));
        assert!(
            formatted
                .contains("### Suggestions (type \"/apply\" to verify and apply)\n- Fix typo.")
        );
        assert!(!formatted.contains("### Suggestions\n"));
    }

    #[test]
    fn test_format_review_markdown_leaves_non_matching_lines_untouched() {
        // Arrange
        let review_markdown = "### Suggestions extra\n- keep as-is";

        // Act
        let formatted = format_review_markdown(review_markdown);

        // Assert
        assert_eq!(formatted, review_markdown);
    }

    #[test]
    fn test_format_review_markdown_compacts_empty_suggestions_without_hint() {
        // Arrange
        let review_markdown =
            "## Review\n\n### Suggestions\n\n- None\n\n### Project Impact\n\n- None";

        // Act
        let formatted = format_review_markdown(review_markdown);

        // Assert
        assert_eq!(
            formatted,
            "## Review\n\n### Suggestions\n- None\n\n### Project Impact\n- None"
        );
        assert!(!formatted.contains("(type \"/apply\" to verify and apply)"));
    }

    #[test]
    fn test_question_help_footer_line_switches_actions_by_focus() {
        // Arrange

        // Act
        let unchanged_chat_focus_line =
            question_help_footer_line(ChatFocus::Chat, false, false, false);
        let changed_chat_focus_line =
            question_help_footer_line(ChatFocus::Chat, true, false, false);
        let answer_focus_line = question_help_footer_line(ChatFocus::Input, false, false, false);

        // Assert
        assert_eq!(
            unchanged_chat_focus_line.to_string(),
            "Tab: focus | j/k: scroll | q: sessions"
        );
        assert_eq!(
            changed_chat_focus_line.to_string(),
            "Tab: focus | j/k: scroll | d: diff | q: sessions"
        );
        assert_eq!(
            answer_focus_line.to_string(),
            "Tab: focus | Enter: send | Ctrl+C: end turn"
        );
    }

    #[test]
    fn test_question_help_footer_line_shows_sessions_in_answer_focus_when_navigating_options() {
        // Arrange — answer focus while navigating predefined options. The
        // runtime accepts plain `q` as an exit-to-list shortcut here, so the
        // footer must surface it. The at-mention overlay variant is also
        // covered here so the footer surfaces `Esc` as a dedicated cancel
        // hint without advertising it as an end-turn shortcut.

        // Act
        let answer_focus_with_options =
            question_help_footer_line(ChatFocus::Input, false, true, false).to_string();
        let answer_focus_with_overlay =
            question_help_footer_line(ChatFocus::Input, false, false, true).to_string();

        // Assert
        assert_eq!(
            answer_focus_with_options,
            "Tab: focus | Enter: send | q: sessions | Ctrl+C: end turn"
        );
        assert_eq!(
            answer_focus_with_overlay,
            "Tab: focus | Enter: send | Esc: cancel @ | Ctrl+C: end turn"
        );
    }

    #[test]
    fn test_session_output_panel_borders_leave_full_inner_width_without_vertical_edges() {
        // Arrange & Act
        let inner_width = panel_inner_width(Rect::new(0, 0, 80, 5), session_output_panel_borders());
        let output_panel_borders = session_output_panel_borders();

        // Assert
        assert_eq!(inner_width, 80);
        assert!(!output_panel_borders.intersects(Borders::LEFT));
        assert!(!output_panel_borders.intersects(Borders::RIGHT));
    }

    #[test]
    fn test_session_output_done_line_shows_continue_hint() {
        // Arrange

        // Act
        let done_line = session_output_done_line();

        // Assert
        assert_eq!(
            done_line.to_string(),
            "Press 'c' to continue in a new session."
        );
    }

    #[test]
    fn test_session_output_status_lines_for_in_progress_include_progress_text() {
        // Arrange

        // Act
        let status_lines = session_output_status_lines(
            Status::InProgress,
            Some("Inspecting changed files"),
            None,
            None,
        );

        // Assert
        assert_eq!(status_lines.len(), 1);
        assert!(
            status_lines[0]
                .to_string()
                .contains("Working... Inspecting changed files")
        );
    }

    #[test]
    fn test_session_output_status_lines_for_in_progress_use_committing_label() {
        // Arrange

        // Act
        let status_lines = session_output_status_lines(
            Status::InProgress,
            Some(COMMITTING_PROGRESS_LABEL),
            None,
            None,
        );

        // Assert
        assert_eq!(status_lines.len(), 1);
        assert!(
            status_lines[0]
                .to_string()
                .contains(COMMITTING_PROGRESS_LABEL)
        );
        assert!(
            !status_lines[0]
                .to_string()
                .contains(&format!("Working... {COMMITTING_PROGRESS_LABEL}"))
        );
    }

    #[test]
    fn test_session_output_status_lines_for_agent_review_use_two_line_hierarchy() {
        // Arrange

        // Act
        let status_lines = session_output_status_lines(
            Status::AgentReview,
            None,
            Some("Reviewing changes\nCodex · gpt-5.6-sol · Extra-high reasoning · Fast"),
            None,
        );

        // Assert
        assert_eq!(status_lines.len(), 2);
        assert_eq!(
            status_lines[0].to_string(),
            format!("{} Reviewing changes", Icon::TachyonLoader)
        );
        assert_eq!(
            status_lines[1].to_string(),
            "    Codex · gpt-5.6-sol · Extra-high reasoning · Fast"
        );
        assert_eq!(
            status_lines[0].spans[0].style.fg,
            Some(style::status_color(Status::AgentReview))
        );
        assert_eq!(
            status_lines[1].spans[0].style.fg,
            Some(style::palette::text_muted())
        );
    }

    #[test]
    fn test_session_output_status_lines_for_agent_review_use_generic_fallback() {
        // Arrange

        // Act
        let status_lines = session_output_status_lines(Status::AgentReview, None, None, None);

        // Assert
        assert_eq!(status_lines.len(), 1);
        assert!(status_lines[0].to_string().contains("Reviewing changes..."));
    }

    #[test]
    fn test_session_output_status_lines_for_merging_use_status_label() {
        // Arrange

        // Act
        let status_lines = session_output_status_lines(Status::Merging, None, None, None);

        // Assert
        assert_eq!(status_lines.len(), 1);
        assert!(status_lines[0].to_string().contains("Merging..."));
    }

    #[test]
    fn test_centered_content_rect_centers_requested_size() {
        // Arrange
        let area = Rect::new(10, 5, 40, 12);

        // Act
        let centered_area = centered_content_rect(area, 20, 5);

        // Assert
        assert_eq!(centered_area, Rect::new(20, 8, 20, 5));
    }

    #[test]
    fn test_centered_content_rect_clamps_requested_size_to_available_area() {
        // Arrange
        let area = Rect::new(3, 2, 12, 4);

        // Act
        let centered_area = centered_content_rect(area, 20, 6);

        // Assert
        assert_eq!(centered_area, area);
    }

    #[test]
    fn test_tab_page_areas_start_content_at_top_and_keep_footer_inset() {
        // Arrange
        let area = Rect::new(5, 3, 40, 10);

        // Act
        let areas = tab_page_areas(area);

        // Assert
        assert_eq!(areas.main_area, Rect::new(6, 3, 38, 8));
        assert_eq!(areas.footer_area, Rect::new(6, 11, 38, 1));
    }

    #[test]
    fn test_calculate_input_height() {
        // Arrange & Act & Assert
        assert_eq!(calculate_input_height(20, ""), 3);
        assert_eq!(calculate_input_height(12, "1234567"), 4);
        assert_eq!(calculate_input_height(12, "12345678"), 4);
        assert_eq!(calculate_input_height(12, "12345671234567890"), 5);
        assert_eq!(calculate_input_height(12, &"a".repeat(120)), 12);
    }

    #[test]
    fn test_question_panel_layout_preserves_input_height_before_trimming_question() {
        // Arrange
        let question = "Need a detailed migration plan with rollback guidance?";
        let input = "Use two phases.";

        // Act
        let panel_layout = question_panel_layout(28, 4, question, input, 10);

        // Assert
        assert_eq!(
            panel_layout,
            QuestionPanelLayout {
                help_height: 1,
                input_height: 3,
                question_height: 0,
                spacer_height: 0,
            }
        );
    }

    #[test]
    fn test_question_panel_layout_uses_wrapped_question_height_when_space_allows() {
        // Arrange
        let question = "Need a detailed migration plan with rollback guidance?";
        let input = "Use two phases.";

        // Act
        let panel_layout = question_panel_layout(28, 9, question, input, 10);

        // Assert — question_height includes +1 for the "Question N/M" title
        // line.
        assert_eq!(
            panel_layout,
            QuestionPanelLayout {
                help_height: 1,
                input_height: 3,
                question_height: 3,
                spacer_height: 1,
            }
        );
    }

    #[test]
    fn test_question_panel_layout_drops_spacer_before_last_question_line() {
        // Arrange
        let question = "Need a detailed migration plan with rollback guidance?";
        let input = "Use two phases.";

        // Act
        let panel_layout = question_panel_layout(28, 6, question, input, 10);

        // Assert — question_height includes +1 for the "Question N/M" title
        // line but the spacer is dropped when space is tight.
        assert_eq!(
            panel_layout,
            QuestionPanelLayout {
                help_height: 1,
                input_height: 3,
                question_height: 2,
                spacer_height: 0,
            }
        );
    }

    #[test]
    fn test_question_panel_reserved_height_includes_options_and_input_rows() {
        // Arrange
        let question = "Continue?";
        let input = "";

        // Act
        let reserved_height = question_panel_reserved_height(40, 12, question, input, 2, 10);

        // Assert
        assert_eq!(reserved_height, 10);
    }

    #[test]
    fn test_question_panel_areas_assign_expected_rows() {
        // Arrange
        let area = Rect::new(4, 10, 40, 10);
        let question = "Continue?";
        let input = "";

        // Act
        let panel_areas = question_panel_areas(area, question, input, 2, 10);

        // Assert
        assert_eq!(panel_areas.question_area, Rect::new(4, 10, 40, 2));
        assert_eq!(panel_areas.options_area, Rect::new(4, 12, 40, 3));
        assert_eq!(panel_areas.spacer_area, Rect::new(4, 15, 40, 1));
        assert_eq!(panel_areas.input_area, Rect::new(4, 16, 40, 3));
        assert_eq!(panel_areas.help_area, Rect::new(4, 19, 40, 1));
    }

    #[test]
    fn test_question_options_height_adds_header_and_clamps() {
        // Arrange & Act & Assert
        assert_eq!(question_options_height(0, 10), 0);
        assert_eq!(question_options_height(2, 10), 3);
        assert_eq!(question_options_height(5, 3), 3);
    }

    #[test]
    fn test_calculate_input_viewport_without_scroll() {
        // Arrange
        let total_line_count = 4;
        let cursor_y = 2;
        let viewport_height = 10;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 0);
        assert_eq!(cursor_row, 2);
    }

    #[test]
    fn test_calculate_input_viewport_with_scroll() {
        // Arrange
        let total_line_count = 20;
        let cursor_y = 15;
        let viewport_height = 10;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 6);
        assert_eq!(cursor_row, 9);
    }

    #[test]
    fn test_calculate_input_viewport_clamps_cursor_to_last_line() {
        // Arrange
        let total_line_count = 3;
        let cursor_y = 10;
        let viewport_height = 2;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 1);
        assert_eq!(cursor_row, 1);
    }

    #[test]
    fn test_overlay_area_above_clamps_to_available_height() {
        // Arrange
        let container_area = Rect::new(0, 5, 30, 12);
        let anchor_area = Rect::new(2, 9, 20, 3);

        // Act
        let overlay_area = overlay_area_above(container_area, anchor_area, 6);

        // Assert
        assert_eq!(overlay_area, Some(Rect::new(2, 5, 20, 4)));
    }

    #[test]
    fn test_overlay_area_above_returns_none_without_space() {
        // Arrange
        let container_area = Rect::new(0, 5, 30, 12);
        let anchor_area = Rect::new(2, 5, 20, 3);

        // Act
        let overlay_area = overlay_area_above(container_area, anchor_area, 2);

        // Assert
        assert_eq!(overlay_area, None);
    }

    #[test]
    fn test_panel_inner_width_excludes_horizontal_borders() {
        // Arrange
        let area = Rect::new(0, 0, 10, 5);

        // Act
        let inner_width = panel_inner_width(area, Borders::LEFT | Borders::RIGHT);

        // Assert
        assert_eq!(inner_width, 8);
    }

    #[test]
    fn test_bottom_pinned_scroll_offset_uses_inner_height() {
        // Arrange
        let area = Rect::new(0, 0, 20, 5);

        // Act
        let scroll_offset =
            bottom_pinned_scroll_offset(area, Borders::TOP | Borders::BOTTOM, 6, None);

        // Assert
        assert_eq!(scroll_offset, 3);
    }

    #[test]
    fn test_bottom_pinned_scroll_offset_preserves_explicit_value() {
        // Arrange
        let area = Rect::new(0, 0, 20, 5);

        // Act
        let scroll_offset =
            bottom_pinned_scroll_offset(area, Borders::TOP | Borders::BOTTOM, 6, Some(2));

        // Assert
        assert_eq!(scroll_offset, 2);
    }

    #[test]
    fn test_placeholder_cursor_position_accounts_for_border_and_prompt_prefix() {
        // Arrange
        let area = Rect::new(10, 5, 40, 4);

        // Act
        let cursor_position = placeholder_cursor_position(area);

        // Assert
        assert_eq!(cursor_position, (14, 6));
    }

    #[test]
    fn test_input_cursor_position_accounts_for_border_and_offsets() {
        // Arrange
        let area = Rect::new(10, 5, 40, 6);
        let cursor_x = 7;
        let cursor_row = 2;

        // Act
        let cursor_position = input_cursor_position(area, cursor_x, cursor_row);

        // Assert
        assert_eq!(cursor_position, (18, 8));
    }

    #[test]
    fn test_suggestion_dropdown_height_includes_border_lines() {
        // Arrange
        let option_count = 4;

        // Act
        let dropdown_height = suggestion_dropdown_height(option_count);

        // Assert
        assert_eq!(dropdown_height, 6);
    }

    #[test]
    fn test_suggestion_dropdown_height_saturates_at_u16_max() {
        // Arrange
        let option_count = usize::MAX;

        // Act
        let dropdown_height = suggestion_dropdown_height(option_count);

        // Assert
        assert_eq!(dropdown_height, u16::MAX);
    }

    #[test]
    fn test_compute_input_layout_empty() {
        // Arrange
        let input = "";
        let width = 20;

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_x, 3); // prefix " › "
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_single_line() {
        // Arrange
        let input = "test";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_x, 7); // 3 (prefix) + 4 (text)
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_exact_fit() {
        // Arrange
        let input = "1234567";
        let width = 12; // Inner width 10, Prefix 3, Available 7
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_wrap() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 4);
        assert_eq!(lines[1].to_string(), "   8");
        assert_eq!(cursor_x, 4);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_wraps_whole_words_when_they_fit_on_next_line() {
        // Arrange
        let input = "abc def";
        let width = 11;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].to_string(), " › abc ");
        assert_eq!(lines[1].to_string(), "   def");
        assert_eq!(cursor_x, 6);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_multiline_exact_fit() {
        // Arrange
        let input = "1234567".to_owned() + "1234567890";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(&input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 10);
        assert_eq!(lines[2].width(), 6);
        assert_eq!(cursor_x, 6);
        assert_eq!(cursor_y, 2);
    }

    #[test]
    fn test_compute_input_layout_cursor_at_start() {
        // Arrange
        let input = "hello";
        let width = 20;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

        // Assert — cursor sits right after prefix
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_cursor_in_middle() {
        // Arrange
        let input = "hello";
        let width = 20;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 2);

        // Assert — prefix(3) + 2 chars
        assert_eq!(cursor_x, 5);
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_cursor_before_wrapped_char() {
        // Arrange
        let input = "12345678";
        let width = 12;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 7);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_cursor_moves_to_next_line_before_wrapped_word() {
        // Arrange
        let input = "abc def";
        let width = 11;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 4);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_move_input_cursor_up_on_wrapped_layout() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let cursor = move_input_cursor_up(input, width, cursor);

        // Assert
        assert_eq!(cursor, 1);
    }

    #[test]
    fn test_move_input_cursor_down_on_wrapped_layout() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = 1;

        // Act
        let cursor = move_input_cursor_down(input, width, cursor);

        // Assert
        assert_eq!(cursor, input.chars().count());
    }

    #[test]
    fn test_compute_input_layout_explicit_newline() {
        // Arrange
        let input = "ab\ncd";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].to_string(), "   cd");
        assert_eq!(cursor_x, 5); // continuation padding + "cd"
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_multiple_newlines() {
        // Arrange
        let input = "a\n\nb";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert — 3 lines: "a", padded continuation line, "b"
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].to_string(), "   ");
        assert_eq!(cursor_x, 4);
        assert_eq!(cursor_y, 2);
    }

    #[test]
    fn test_compute_input_layout_cursor_on_second_line() {
        // Arrange — cursor right after the newline
        let input = "ab\ncd";
        let width = 20;

        // Act — cursor at char index 3 = 'a','b','\n' -> position 0 of second
        // line
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 3);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_first_table_column_width_uses_remaining_layout_space() {
        // Arrange
        let constraints = [
            Constraint::Fill(1),
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(4),
            Constraint::Length(6),
        ];

        // Act
        let width = first_table_column_width(50, &constraints, 1, 3);

        // Assert
        assert_eq!(width, 21);
    }

    #[test]
    fn test_compute_input_layout_at_mention_highlighting() {
        // Arrange
        let input = "hello @file world";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        // Prefix is at index 0: " › "
        assert_eq!(spans[0].content, " › ");

        // "hello " (indices 1..7) should be normal style
        for span in spans.iter().take(7).skip(1) {
            assert_eq!(span.style.fg, None);
        }

        // "@file" (indices 7..12) should be highlighted
        for span in spans.iter().take(12).skip(7) {
            assert_eq!(span.style.fg, Some(style::palette::info()));
        }

        // " world" (indices 12..18) should be normal style
        for span in spans.iter().take(18).skip(12) {
            assert_eq!(span.style.fg, None);
        }
    }

    #[test]
    fn test_compute_input_layout_at_mention_highlighting_stops_before_trailing_comma() {
        // Arrange
        let input = "hello @file, world";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().take(12).skip(7) {
            assert_eq!(span.style.fg, Some(style::palette::info()));
        }
        assert_eq!(spans[12].content, ",");
        assert_eq!(spans[12].style.fg, None);
    }

    #[test]
    fn test_compute_input_layout_at_mention_highlighting_stops_before_trailing_parenthesis() {
        // Arrange
        let input = "hello @file) world";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().take(12).skip(7) {
            assert_eq!(span.style.fg, Some(style::palette::info()));
        }
        assert_eq!(spans[12].content, ")");
        assert_eq!(spans[12].style.fg, None);
    }

    #[test]
    fn test_compute_input_layout_highlights_parenthesized_at_mention() {
        // Arrange
        let input = "review (@src/main.rs)";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().take(21).skip(9) {
            assert_eq!(span.style.fg, Some(style::palette::info()));
        }
        assert_eq!(spans[21].content, ")");
        assert_eq!(spans[21].style.fg, None);
    }

    #[test]
    fn test_compute_input_layout_no_highlight_for_email() {
        // Arrange
        let input = "email@example.com";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().skip(1) {
            assert_eq!(span.style.fg, None);
        }
    }

    #[test]
    fn test_compute_input_layout_highlights_image_placeholder_tokens() {
        // Arrange
        let input = "Review [Image #12] before merge";
        let width = 60;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;
        let image_start = 1 + "Review ".chars().count();
        let image_end = image_start + "[Image #12]".chars().count();

        for span in spans.iter().take(image_end).skip(image_start) {
            assert_eq!(span.style.fg, Some(style::palette::warning()));
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn test_compute_input_layout_does_not_highlight_invalid_image_like_tokens() {
        // Arrange
        let input = "Review [Image #] before merge";
        let width = 60;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;
        let token_start = 1 + "Review ".chars().count();
        let token_end = token_start + "[Image #]".chars().count();

        for span in spans.iter().take(token_end).skip(token_start) {
            assert_eq!(span.style.fg, None);
        }
    }
}
