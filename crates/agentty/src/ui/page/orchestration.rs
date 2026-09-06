//! Board-first orchestration campaign page.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ui::page::session_chat::{SessionChatPage, SessionChatPageInput};
use crate::ui::{Page, style};

/// Controller page that keeps campaign state above a compact conversation
/// pane.
pub struct OrchestrationPage<'a> {
    can_open_worktree: bool,
    chat_input: SessionChatPageInput<'a>,
}

impl<'a> OrchestrationPage<'a> {
    /// Creates a campaign page from the same immutable inputs used by the
    /// controller chat pane.
    pub fn new(chat_input: SessionChatPageInput<'a>) -> Self {
        Self {
            can_open_worktree: false,
            chat_input,
        }
    }

    /// Sets whether the controller worktree can be opened.
    #[must_use]
    pub fn can_open_worktree(mut self, can_open_worktree: bool) -> Self {
        self.can_open_worktree = can_open_worktree;

        self
    }

    fn render_board(&self, frame: &mut Frame, area: Rect) {
        let session = self.chat_input.sessions.get(self.chat_input.session_index);
        let progress = session
            .and_then(|session| session.orchestration_progress.as_deref())
            .unwrap_or("Phase: Planning\nDiscuss the goal with the controller to produce a plan.");
        let mut lines = progress
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let style = if index == 0 {
                    Style::default()
                        .fg(style::palette::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(style::palette::text())
                };

                Line::from(Span::styled(line.to_string(), style))
            })
            .collect::<Vec<_>>();
        if progress.contains("AwaitingApproval") || progress == "Awaiting approval" {
            lines.push(Line::from(Span::styled(
                "a approve  Enter discuss/revise",
                Style::default().fg(style::palette::text_muted()),
            )));
        } else if progress.contains("AwaitingIntegration") {
            lines.push(Line::from(Span::styled(
                "a approve integration",
                Style::default().fg(style::palette::text_muted()),
            )));
        }
        let title = session.map_or("Orchestration Campaign", |session| session.display_title());
        let board = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(style::palette::border()))
                    .title(format!(" Campaign: {title} ")),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(board, area);
    }
}

impl Page for OrchestrationPage<'_> {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let progress = self
            .chat_input
            .sessions
            .get(self.chat_input.session_index)
            .and_then(|session| session.orchestration_progress.as_deref());
        let [board_area, chat_area] = campaign_page_areas(area, progress);

        self.render_board(frame, board_area);
        SessionChatPage::new(self.chat_input)
            .can_open_worktree(self.can_open_worktree)
            .render(frame, chat_area);
    }
}

/// Splits a controller page into its campaign board and chat areas.
///
/// Runtime scroll metrics use the same chat area so line-step bounds match
/// the compact transcript viewport painted below the campaign board.
pub(crate) fn campaign_page_areas(area: Rect, progress: Option<&str>) -> [Rect; 2] {
    let board_height = campaign_board_height(progress);

    Layout::vertical([Constraint::Length(board_height), Constraint::Min(8)]).areas(area)
}

/// Calculates the bounded campaign board height from the current snapshot.
fn campaign_board_height(progress: Option<&str>) -> u16 {
    let progress_lines = progress.map_or(1, |progress| progress.lines().count());

    u16::try_from(progress_lines.saturating_add(4))
        .unwrap_or(12)
        .clamp(6, 12)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::session::{SessionRole, Status};
    use crate::presentation::app_mode::AppMode;
    use crate::presentation::frame_time::FrameTime;
    use crate::ui::component::session_output::SessionOutputLayoutCache;
    use crate::ui::markdown::MarkdownRenderCache;

    fn render_campaign(progress: Option<&str>) -> String {
        let mut session = crate::test_support::SessionFixtureBuilder::new()
            .role(SessionRole::Orchestrator)
            .status(Status::Review)
            .build();
        session.orchestration_progress = progress.map(str::to_string);
        let sessions = [session];
        let mode = AppMode::View {
            scroll_offset: None,
            session_id: sessions[0].id.clone(),
        };
        let markdown_render_cache = MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let input = SessionChatPageInput {
            active_prompt_output: None,
            active_progress: None,
            resources: None,
            default_reasoning_level: ReasoningLevel::default(),
            frame_time: FrameTime::new(0, 0, 0),
            has_merge_conflict: false,
            markdown_render_cache: &markdown_render_cache,
            mode: &mode,
            output_layout_cache: &output_layout_cache,
            review_text: None,
            scroll_offset: None,
            session_index: 0,
            session_update_version: 0,
            sessions: &sessions,
        };
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");

        terminal
            .draw(|frame| {
                OrchestrationPage::new(input)
                    .can_open_worktree(true)
                    .render(frame, frame.area());
            })
            .expect("failed to render campaign");

        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn campaign_board_height_has_readable_minimum() {
        // Arrange, Act
        let height = campaign_board_height(None);

        // Assert
        assert_eq!(height, 6);
    }

    #[test]
    fn campaign_board_height_is_bounded_for_large_campaigns() {
        // Arrange
        let progress = (0..20)
            .map(|index| format!("task-{index}"))
            .collect::<Vec<_>>()
            .join("\n");

        // Act
        let height = campaign_board_height(Some(&progress));

        // Assert
        assert_eq!(height, 12);
    }

    #[test]
    fn campaign_page_areas_reserve_the_board_above_chat() {
        // Arrange
        let page_area = Rect::new(2, 3, 80, 24);
        let progress = "Phase: Running\n1. api\n2. ui\n3. docs";

        // Act
        let [board_area, chat_area] = campaign_page_areas(page_area, Some(progress));

        // Assert
        assert_eq!(board_area, Rect::new(2, 3, 80, 8));
        assert_eq!(chat_area, Rect::new(2, 11, 80, 16));
    }

    #[test]
    fn campaign_page_renders_planning_approval_and_integration_boards() {
        // Arrange
        let cases = [
            (None, "Discuss the goal"),
            (
                Some("Phase: AwaitingApproval\nParallel workers: 3 (global setting)"),
                "a approve  Enter discuss/revise",
            ),
            (
                Some("Phase: AwaitingIntegration\n1. api - ready"),
                "a approve integration",
            ),
        ];

        // Act
        let rendered = cases
            .iter()
            .map(|(progress, _)| render_campaign(*progress))
            .collect::<Vec<_>>();

        // Assert
        for ((_, expected), frame) in cases.iter().zip(rendered) {
            assert!(frame.contains(expected));
            assert!(frame.contains("Campaign:"));
        }
    }
}
