use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::permission::PlanFollowup;
use crate::domain::session::{Session, Status};
use crate::icon::Icon;
use crate::ui::Component;
use crate::ui::markdown::render_markdown;
use crate::ui::util::truncate_with_ellipsis;

/// Session chat output panel renderer.
pub struct SessionOutput<'a> {
    active_progress: Option<&'a str>,
    plan_followup: Option<&'a PlanFollowup>,
    scroll_offset: Option<u16>,
    session: &'a Session,
}

impl<'a> SessionOutput<'a> {
    /// Creates a new session output component.
    pub fn new(
        session: &'a Session,
        scroll_offset: Option<u16>,
        plan_followup: Option<&'a PlanFollowup>,
        active_progress: Option<&'a str>,
    ) -> Self {
        Self {
            active_progress,
            plan_followup,
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
        plan_followup: Option<&PlanFollowup>,
        active_progress: Option<&str>,
    ) -> u16 {
        let output_area = Rect::new(0, 0, output_width, 0);
        let lines = Self::output_lines(
            session,
            output_area,
            session.status,
            plan_followup,
            active_progress,
        );

        u16::try_from(lines.len()).unwrap_or(u16::MAX)
    }

    fn output_lines(
        session: &Session,
        output_area: Rect,
        status: Status,
        plan_followup: Option<&PlanFollowup>,
        active_progress: Option<&str>,
    ) -> Vec<Line<'static>> {
        let output_text = Self::output_text(session, status);
        let output_text = Self::output_text_with_spaced_user_input(output_text);
        let inner_width = output_area.width.saturating_sub(2) as usize;
        let mut lines = render_markdown(&output_text, inner_width);

        if matches!(
            status,
            Status::InProgress | Status::Queued | Status::Rebasing | Status::Merging
        ) {
            while lines.last().is_some_and(|line| line.width() == 0) {
                lines.pop();
            }

            lines.push(Line::from(""));

            let status_icon = Self::status_icon(status);
            let status_message = Self::status_message(status, active_progress);
            lines.push(Line::from(vec![Span::styled(
                format!("{status_icon} {status_message}"),
                Style::default().fg(status.color()),
            )]));
        } else {
            lines.push(Line::from(""));
        }

        if let Some(plan_followup) = plan_followup {
            lines.push(Line::from(""));
            lines.extend(Self::plan_followup_lines(plan_followup));
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

    /// Adds visual spacing around user prompt blocks while preserving pasted
    /// multiline prompts as one contiguous message.
    fn output_text_with_spaced_user_input(output_text: &str) -> String {
        let raw_lines = output_text.split('\n').collect::<Vec<_>>();
        let mut formatted_lines = Vec::with_capacity(raw_lines.len());
        let mut line_index = 0;

        while line_index < raw_lines.len() {
            let line = raw_lines[line_index];
            if !line.starts_with(" › ") {
                formatted_lines.push(line.to_string());
                line_index += 1;

                continue;
            }

            if formatted_lines
                .last()
                .is_some_and(|item: &String| !item.is_empty())
            {
                formatted_lines.push(String::new());
            }

            let block_end_index = Self::user_prompt_block_end_index(&raw_lines, line_index);
            formatted_lines.extend(
                raw_lines[line_index..=block_end_index]
                    .iter()
                    .map(ToString::to_string),
            );
            line_index = block_end_index + 1;

            let next_line_is_empty = raw_lines
                .get(line_index)
                .is_none_or(|next_line| next_line.is_empty());
            if !next_line_is_empty {
                formatted_lines.push(String::new());
            }
        }

        formatted_lines.join("\n")
    }

    /// Returns the final non-empty line index for a user prompt block that
    /// starts at `start_index`.
    fn user_prompt_block_end_index(raw_lines: &[&str], start_index: usize) -> usize {
        let mut candidate_index = start_index + 1;

        while candidate_index < raw_lines.len() {
            let candidate_line = raw_lines[candidate_index];
            if candidate_line.is_empty() || candidate_line.starts_with(" › ") {
                break;
            }

            candidate_index += 1;
        }

        if raw_lines
            .get(candidate_index)
            .is_some_and(|candidate_line| candidate_line.is_empty())
        {
            return candidate_index.saturating_sub(1).max(start_index);
        }

        start_index
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
            Status::Queued => "Waiting in merge queue...".to_string(),
            Status::Rebasing => "Rebasing...".to_string(),
            Status::Merging => "Merging...".to_string(),
            Status::New | Status::Review | Status::Done | Status::Canceled => String::new(),
        }
    }

    /// Returns the status indicator icon used for inline status messages.
    fn status_icon(status: Status) -> Icon {
        match status {
            Status::InProgress | Status::Rebasing | Status::Merging => Icon::current_spinner(),
            Status::Queued | Status::New | Status::Review | Status::Done | Status::Canceled => {
                Icon::Pending
            }
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

    /// Renders plan followup options as a vertical list with optional
    /// question text header.
    fn plan_followup_lines(plan_followup: &PlanFollowup) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        if let Some(question_text) = plan_followup.current_question_text() {
            lines.push(Line::from(vec![Span::styled(
                question_text.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));
        }

        for (option_index, option) in plan_followup.options.iter().enumerate() {
            let is_selected = plan_followup.selected_index() == option_index;
            let option_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let option_label = option.label();

            lines.push(Line::from(vec![Span::styled(
                format!("[ {option_label} ]"),
                option_style,
            )]));
        }

        lines.push(Line::from(vec![Span::styled(
            "Use \u{2191}/\u{2193} to select and Enter to confirm.",
            Style::default().fg(Color::DarkGray),
        )]));

        lines
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
            self.plan_followup,
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
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::permission::{PermissionMode, PlanFollowup};
    use crate::domain::session::{SessionSize, SessionStats};

    fn session_fixture() -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
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
            updated_at: 0,
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
        let followup = PlanFollowup::new(VecDeque::new());
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };

        // Act
        let lines =
            SessionOutput::output_lines(&session, area, Status::Review, Some(&followup), None);
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
    fn test_output_text_with_spaced_user_input_keeps_multiline_user_prompt_together() {
        // Arrange
        let output = "assistant output\n › first line\nsecond line\n\nagent response";

        // Act
        let spaced = SessionOutput::output_text_with_spaced_user_input(output);

        // Assert
        assert_eq!(
            spaced,
            "assistant output\n\n › first line\nsecond line\n\nagent response"
        );
    }

    #[test]
    fn test_plan_followup_lines_always_uses_vertical_layout() {
        // Arrange
        let followup = PlanFollowup::new(VecDeque::new());

        // Act
        let lines = SessionOutput::plan_followup_lines(&followup);
        let rendered = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(rendered.contains("Implement the plan"));
        assert!(rendered.contains("Type feedback"));
        assert!(rendered.contains("\u{2191}/\u{2193}"));
    }

    #[test]
    fn test_plan_followup_lines_shows_question_text_above_answers() {
        // Arrange
        use crate::domain::plan::PlanQuestion;
        let followup = PlanFollowup::new(VecDeque::from(vec![PlanQuestion {
            answers: vec!["30 seconds".to_string(), "60 seconds".to_string()],
            text: "What interval?".to_string(),
        }]));

        // Act
        let lines = SessionOutput::plan_followup_lines(&followup);
        let rendered = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(rendered.contains("What interval?"));
        assert!(rendered.contains("30 seconds"));
        assert!(rendered.contains("60 seconds"));
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
    fn test_status_message_for_queued() {
        // Arrange & Act
        let message = SessionOutput::status_message(Status::Queued, None);

        // Assert
        assert_eq!(message, "Waiting in merge queue...");
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
