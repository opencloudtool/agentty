use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::domain::input::InputState;
use crate::domain::session::PublishBranchAction;
use crate::ui::component::chat_input::ChatInput;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const PUSH_EDITABLE_HELP_TEXT: &str = "Enter: publish branch | Esc: cancel";
const PUSH_LOCKED_HELP_TEXT: &str = "Enter: publish branch again | Esc: cancel";
const PULL_REQUEST_EDITABLE_HELP_TEXT: &str = "Enter: publish pull request | Esc: cancel";
const PULL_REQUEST_LOCKED_HELP_TEXT: &str = "Enter: publish pull request again | Esc: cancel";
const INPUT_TITLE: &str = "Remote Branch";
const MIN_OVERLAY_HEIGHT: u16 = 11;
const MIN_OVERLAY_WIDTH: u16 = 58;
const OVERLAY_HEIGHT_PERCENT: u16 = 42;
const OVERLAY_WIDTH_PERCENT: u16 = 62;

/// Centered popup that collects an optional remote branch name for branch
/// publishing.
pub struct PublishBranchOverlay<'a> {
    default_branch_name: &'a str,
    input: &'a InputState,
    locked_upstream_ref: Option<&'a str>,
    publish_branch_action: PublishBranchAction,
}

impl<'a> PublishBranchOverlay<'a> {
    /// Creates a publish-branch popup for the provided input state.
    pub fn new(
        input: &'a InputState,
        default_branch_name: &'a str,
        locked_upstream_ref: Option<&'a str>,
        publish_branch_action: PublishBranchAction,
    ) -> Self {
        Self {
            default_branch_name,
            input,
            locked_upstream_ref,
            publish_branch_action,
        }
    }

    /// Returns the centered popup rectangle constrained to terminal bounds.
    fn popup_area(area: Rect) -> Rect {
        overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            OVERLAY_HEIGHT_PERCENT,
            MIN_OVERLAY_WIDTH,
            MIN_OVERLAY_HEIGHT,
        )
    }

    /// Returns the placeholder shown before the first publish.
    fn placeholder(&self) -> String {
        format!("Leave blank to push as `{}`", self.default_branch_name)
    }

    /// Returns the explanatory message shown above the branch field.
    fn message_text(&self) -> String {
        match (self.publish_branch_action, self.locked_upstream_ref) {
            (PublishBranchAction::Push, Some(upstream_ref)) => format!(
                "Already published to `{upstream_ref}`. This session stays locked to that remote \
                 branch. Use `Shift+P` to create or refresh the GitHub pull request."
            ),
            (PublishBranchAction::Push, None) => "Optional remote branch name for this branch \
                                                  publish. Use `Shift+P` to create or refresh the \
                                                  GitHub pull request."
                .to_string(),
            (PublishBranchAction::PublishPullRequest, Some(upstream_ref)) => format!(
                "Already published to `{upstream_ref}`. Publish the GitHub pull request from this \
                 locked branch."
            ),
            (PublishBranchAction::PublishPullRequest, None) => {
                "Optional remote branch name for this publish before creating or refreshing the \
                 GitHub pull request."
                    .to_string()
            }
        }
    }

    /// Returns the footer help line for the current overlay state.
    fn help_text(&self) -> &'static str {
        match (
            self.publish_branch_action,
            self.locked_upstream_ref.is_some(),
        ) {
            (PublishBranchAction::Push, true) => PUSH_LOCKED_HELP_TEXT,
            (PublishBranchAction::Push, false) => PUSH_EDITABLE_HELP_TEXT,
            (PublishBranchAction::PublishPullRequest, true) => PULL_REQUEST_LOCKED_HELP_TEXT,
            (PublishBranchAction::PublishPullRequest, false) => PULL_REQUEST_EDITABLE_HELP_TEXT,
        }
    }

    /// Renders a non-editable branch field when the session already tracks one
    /// remote branch.
    fn render_locked_branch_field(&self, f: &mut Frame, area: Rect) {
        let title = format!(" {INPUT_TITLE} ");
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(
                Style::default()
                    .fg(palette::ACCENT)
                    .add_modifier(Modifier::BOLD),
            )
            .title(Span::styled(
                title,
                Style::default()
                    .fg(palette::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ));
        let paragraph = Paragraph::new(self.input.text()).block(block);

        f.render_widget(Clear, area);
        f.render_widget(paragraph, area);
    }
}

impl Component for PublishBranchOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = Self::popup_area(area);
        let title = match self.publish_branch_action {
            PublishBranchAction::Push => "Publish Branch",
            PublishBranchAction::PublishPullRequest => "Publish GitHub Pull Request",
        };
        let block = overlay::overlay_block(title, palette::ACCENT);
        let inner_area = block.inner(popup_area);
        let sections = Layout::vertical([
            Constraint::Min(2),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner_area);
        let message = Paragraph::new(Line::from(vec![Span::styled(
            self.message_text(),
            Style::default().fg(palette::TEXT_MUTED),
        )]))
        .wrap(Wrap { trim: true });
        let help = Paragraph::new(
            Line::from(vec![Span::styled(
                self.help_text(),
                Style::default().fg(palette::TEXT_MUTED),
            )])
            .alignment(Alignment::Center),
        );

        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);
        f.render_widget(message, sections[0]);
        if self.locked_upstream_ref.is_some() {
            self.render_locked_branch_field(f, sections[1]);
        } else {
            let placeholder = self.placeholder();
            let input = ChatInput::new(INPUT_TITLE, self.input.text(), self.input.cursor)
                .placeholder(&placeholder);

            input.render(f, sections[1]);
        }
        f.render_widget(help, sections[2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_branch_overlay_popup_area_is_centered() {
        // Arrange
        let area = Rect::new(0, 0, 120, 40);

        // Act
        let popup_area = PublishBranchOverlay::popup_area(area);

        // Assert
        assert_eq!(popup_area.width, 74);
        assert_eq!(popup_area.height, 16);
        assert_eq!(popup_area.x, 23);
        assert_eq!(popup_area.y, 12);
    }

    #[test]
    fn test_publish_branch_overlay_render_contains_placeholder_and_help_text() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let input = InputState::default();
        let overlay =
            PublishBranchOverlay::new(&input, "agentty/ff45463f", None, PublishBranchAction::Push);

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                overlay.render(frame, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Publish Branch"));
        assert!(text.contains("Leave blank to push as `agentty/ff45463f`"));
        assert!(text.contains("Shift+P"));
        assert!(text.contains(PUSH_EDITABLE_HELP_TEXT));
    }

    #[test]
    fn test_publish_branch_overlay_render_shows_locked_upstream_message() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let input = InputState::with_text("review/custom".to_string());
        let overlay = PublishBranchOverlay::new(
            &input,
            "agentty/ff45463f",
            Some("origin/review/custom"),
            PublishBranchAction::Push,
        );

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                overlay.render(frame, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("origin/review/custom"));
        assert!(text.contains("review/custom"));
        assert!(text.contains("Shift+P"));
        assert!(text.contains(PUSH_LOCKED_HELP_TEXT));
    }

    #[test]
    fn test_publish_pull_request_overlay_render_shows_pull_request_copy() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let input = InputState::with_text("review/custom".to_string());
        let overlay = PublishBranchOverlay::new(
            &input,
            "agentty/ff45463f",
            None,
            PublishBranchAction::PublishPullRequest,
        );

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                overlay.render(frame, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Publish GitHub Pull Request"));
        assert!(text.contains("GitHub pull request"));
        assert!(text.contains(PULL_REQUEST_EDITABLE_HELP_TEXT));
    }
}
