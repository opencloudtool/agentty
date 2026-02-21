use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::permission::PlanFollowupAction;
use crate::domain::session::{Session, Status};
use crate::icon::Icon;
use crate::ui::Component;
use crate::ui::markdown::render_markdown;
use crate::ui::util::truncate_with_ellipsis;

/// Session chat output panel renderer.
pub struct SessionOutput<'a> {
    active_progress: Option<&'a str>,
    plan_followup_action: Option<PlanFollowupAction>,
    scroll_offset: Option<u16>,
    session: &'a Session,
}

impl<'a> SessionOutput<'a> {
    /// Creates a new session output component.
    pub fn new(
        session: &'a Session,
        scroll_offset: Option<u16>,
        plan_followup_action: Option<PlanFollowupAction>,
        active_progress: Option<&'a str>,
    ) -> Self {
        Self {
            active_progress,
            plan_followup_action,
            scroll_offset,
            session,
        }
    }

    /// Returns the rendered output line count for chat content at a given
    /// width.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering so scroll math can stay in sync with what users see.
    pub(crate) fn rendered_line_count(
        session: &Session,
        output_width: u16,
        plan_followup_action: Option<PlanFollowupAction>,
        active_progress: Option<&str>,
    ) -> u16 {
        let output_area = Rect::new(0, 0, output_width, 0);
        let lines = Self::output_lines(
            session,
            output_area,
            session.status,
            plan_followup_action,
            active_progress,
        );

        u16::try_from(lines.len()).unwrap_or(u16::MAX)
    }

    fn output_lines(
        session: &Session,
        output_area: Rect,
        status: Status,
        plan_followup_action: Option<PlanFollowupAction>,
        active_progress: Option<&str>,
    ) -> Vec<Line<'static>> {
        let output_text = Self::output_text(session, status);
        let output_text = Self::output_text_with_spaced_user_input(output_text);
        let inner_width = output_area.width.saturating_sub(2) as usize;
        let mut lines = render_markdown(&output_text, inner_width);

        if matches!(
            status,
            Status::InProgress | Status::Rebasing | Status::Merging
        ) {
            while lines.last().is_some_and(|line| line.width() == 0) {
                lines.pop();
            }

            lines.push(Line::from(""));

            let status_message = Self::status_message(status, active_progress);
            lines.push(Line::from(vec![Span::styled(
                format!("{} {}", Icon::current_spinner(), status_message),
                Style::default().fg(status.color()),
            )]));
        } else {
            lines.push(Line::from(""));
        }

        if let Some(selected_action) = plan_followup_action {
            lines.push(Line::from(""));
            lines.push(Self::plan_followup_action_line(selected_action));
            lines.push(Line::from(vec![Span::styled(
                "Use \u{2190}/\u{2192} to select and Enter to confirm.",
                Style::default().fg(Color::DarkGray),
            )]));
        }

        lines
    }

    fn output_text(session: &Session, status: Status) -> &str {
        if matches!(status, Status::Done | Status::Canceled)
            && let Some(summary) = session
                .summary
                .as_deref()
                .filter(|summary| !summary.is_empty())
        {
            return summary;
        }

        &session.output
    }

    fn output_text_with_spaced_user_input(output_text: &str) -> String {
        let raw_lines = output_text.split('\n').collect::<Vec<_>>();
        let mut formatted_lines = Vec::with_capacity(raw_lines.len());

        for (index, line) in raw_lines.iter().enumerate() {
            let is_user_input = line.starts_with(" › ");
            if is_user_input
                && formatted_lines
                    .last()
                    .is_some_and(|item: &String| !item.is_empty())
            {
                formatted_lines.push(String::new());
            }

            formatted_lines.push((*line).to_string());

            if is_user_input {
                let next_line_is_empty =
                    raw_lines.get(index + 1).is_some_and(|next| next.is_empty());
                if !next_line_is_empty {
                    formatted_lines.push(String::new());
                }
            }
        }

        formatted_lines.join("\n")
    }

    fn status_message(status: Status, active_progress: Option<&str>) -> String {
        if let Some(progress) = active_progress
            .map(str::trim)
            .filter(|progress| !progress.is_empty())
        {
            let base_progress = progress.trim_end_matches('.');

            return format!("{base_progress}{}", Self::animated_progress_suffix());
        }

        match status {
            Status::InProgress => "Thinking...".to_string(),
            Status::Rebasing => "Rebasing...".to_string(),
            Status::Merging => "Merging...".to_string(),
            Status::New | Status::Review | Status::Done | Status::Canceled => String::new(),
        }
    }

    fn animated_progress_suffix() -> &'static str {
        let tick = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            / 300;

        match tick % 4 {
            0 => "",
            1 => ".",
            2 => "..",
            _ => "...",
        }
    }

    fn plan_followup_action_line(selected_action: PlanFollowupAction) -> Line<'static> {
        let implement_style = if selected_action == PlanFollowupAction::ImplementPlan {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let feedback_style = if selected_action == PlanFollowupAction::TypeFeedback {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        Line::from(vec![
            Span::styled(
                format!("[ {} ]", PlanFollowupAction::ImplementPlan.label()),
                implement_style,
            ),
            Span::styled("  ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("[ {} ]", PlanFollowupAction::TypeFeedback.label()),
                feedback_style,
            ),
        ])
    }

    fn final_scroll_offset(&self, output_area: Rect, line_count: usize) -> u16 {
        if let Some(scroll_offset) = self.scroll_offset {
            return scroll_offset;
        }

        let inner_height = output_area.height.saturating_sub(2) as usize;

        u16::try_from(line_count.saturating_sub(inner_height)).unwrap_or(u16::MAX)
    }
}

impl Component for SessionOutput<'_> {
    fn render(&self, f: &mut Frame, output_area: Rect) {
        let status = self.session.status;
        let status_str = status.to_string();
        let max_title_width = output_area
            .width
            .saturating_sub(u16::try_from(status_str.len()).unwrap_or(0))
            .saturating_sub(8) as usize;
        let truncated_title = truncate_with_ellipsis(self.session.display_title(), max_title_width);
        let title = format!(" {status_str} - {truncated_title} ");

        let lines = Self::output_lines(
            self.session,
            output_area,
            status,
            self.plan_followup_action,
            self.active_progress,
        );
        let final_scroll = self.final_scroll_offset(output_area, lines.len());

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(status.color()))
                    .title(Span::styled(title, Style::default().fg(status.color()))),
            )
            .scroll((final_scroll, 0));

        f.render_widget(paragraph, output_area);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::permission::PermissionMode;
    use crate::domain::session::{SessionSize, SessionStats};

    fn session_fixture() -> Session {
        Session {
            base_branch: "main".to_string(),
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            permission_mode: PermissionMode::default(),
            project_name: "project".to_string(),
            prompt: String::new(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::New,
            summary: None,
            title: None,
        }
    }

    #[test]
    fn test_rendered_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        session.output = "word ".repeat(40);
        let raw_line_count = u16::try_from(session.output.lines().count()).unwrap_or(u16::MAX);

        // Act
        let rendered_line_count = SessionOutput::rendered_line_count(&session, 20, None, None);

        // Assert
        assert!(rendered_line_count > raw_line_count);
    }

    #[test]
    fn test_output_lines_uses_summary_for_done_session() {
        // Arrange
        let mut session = session_fixture();
        session.output = "streamed output".to_string();
        session.summary = Some("terminal summary".to_string());
        session.status = Status::Done;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            session.status,
            None,
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("terminal summary"));
        assert!(!text.contains("streamed output"));
    }

    #[test]
    fn test_output_lines_uses_summary_for_canceled_session() {
        // Arrange
        let mut session = session_fixture();
        session.output = "streamed output".to_string();
        session.summary = Some("canceled summary".to_string());
        session.status = Status::Canceled;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            session.status,
            None,
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("canceled summary"));
        assert!(!text.contains("streamed output"));
    }

    #[test]
    fn test_output_lines_render_user_prompt_with_cyan_bold_styling() {
        // Arrange
        let mut session = session_fixture();
        session.output = " › /model gemini".to_string();

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            session.status,
            None,
            None,
        );

        // Assert
        let first_line = lines.first().expect("expected output line");
        assert_eq!(first_line.to_string(), " › /model gemini");
        assert_eq!(first_line.spans[0].style.fg, Some(Color::Cyan));
        assert!(
            first_line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn test_output_lines_appends_empty_line_when_done() {
        // Arrange
        let mut session = session_fixture();
        session.output = "some output".to_string();
        session.status = Status::Done;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            session.status,
            None,
            None,
        );

        // Assert
        assert!(lines.last().expect("lines").to_string().is_empty());
        assert!(lines.len() >= 2);
    }

    #[test]
    fn test_output_lines_appends_empty_line_before_spinner() {
        // Arrange
        let mut session = session_fixture();
        session.output = "some output".to_string();
        session.status = Status::InProgress;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            session.status,
            None,
            None,
        );

        // Assert
        assert!(lines.len() >= 3);
        let len = lines.len();
        assert!(lines[len - 2].to_string().is_empty());
        assert!(lines[len - 1].to_string().contains("Thinking..."));
    }

    #[test]
    fn test_output_lines_include_plan_followup_actions_when_present() {
        // Arrange
        let session = session_fixture();
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            area,
            Status::Review,
            Some(PlanFollowupAction::ImplementPlan),
            None,
        );
        let rendered = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(rendered.contains("Implement the plan"));
        assert!(rendered.contains("Type feedback"));
        assert!(rendered.contains("Use"));
    }

    #[test]
    fn test_output_text_with_spaced_user_input_adds_empty_line_before_and_after() {
        // Arrange
        let output = "assistant output\n › user prompt\nagent response";

        // Act
        let spaced = SessionOutput::output_text_with_spaced_user_input(output);

        // Assert
        assert_eq!(
            spaced,
            "assistant output\n\n › user prompt\n\nagent response"
        );
    }

    #[test]
    fn test_output_text_with_spaced_user_input_keeps_existing_empty_lines() {
        // Arrange
        let output = "assistant output\n\n › user prompt\n\nagent response";

        // Act
        let spaced = SessionOutput::output_text_with_spaced_user_input(output);

        // Assert
        assert_eq!(spaced, output);
    }

    #[test]
    fn test_plan_followup_action_line_contains_both_actions() {
        // Arrange & Act
        let line = SessionOutput::plan_followup_action_line(PlanFollowupAction::TypeFeedback);
        let rendered = line.to_string();

        // Assert
        assert!(rendered.contains("Implement the plan"));
        assert!(rendered.contains("Type feedback"));
    }

    #[test]
    fn test_status_message_for_merging() {
        // Arrange & Act
        let message = SessionOutput::status_message(Status::Merging, None);

        // Assert
        assert_eq!(message, "Merging...");
    }

    #[test]
    fn test_status_message_uses_active_progress_with_animated_suffix() {
        // Arrange & Act
        let message = SessionOutput::status_message(Status::InProgress, Some("Searching the web"));
        let suffix = &message["Searching the web".len()..];

        // Assert
        assert!(message.starts_with("Searching the web"));
        assert!(matches!(suffix, "" | "." | ".." | "..."));
    }
}
