use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::agent::{AgentKind, AgentSelectionMetadata};
use crate::file_list;
use crate::icon::Icon;
use crate::model::{
    AppMode, PlanFollowupAction, PromptAtMentionState, PromptSlashStage, Session, Status,
    extract_at_mention_query,
};
use crate::ui::components::chat_input::{ChatInput, SlashMenu, SlashMenuOption};
use crate::ui::util::{calculate_input_height, wrap_lines};
use crate::ui::{Component, Page};

/// Chat page renderer for a single session.
pub struct SessionChatPage<'a> {
    pub mode: &'a AppMode,
    pub plan_followup_action: Option<PlanFollowupAction>,
    pub scroll_offset: Option<u16>,
    pub session_index: usize,
    pub sessions: &'a [Session],
}

impl<'a> SessionChatPage<'a> {
    /// Creates a session chat page renderer.
    pub fn new(
        sessions: &'a [Session],
        session_index: usize,
        scroll_offset: Option<u16>,
        mode: &'a AppMode,
        plan_followup_action: Option<PlanFollowupAction>,
    ) -> Self {
        Self {
            mode,
            plan_followup_action,
            scroll_offset,
            session_index,
            sessions,
        }
    }

    /// Returns the rendered output line count for chat content at a given
    /// width.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering so scroll math can stay in sync with what users see.
    pub(crate) fn rendered_output_line_count(
        session: &Session,
        output_width: u16,
        plan_followup_action: Option<PlanFollowupAction>,
    ) -> u16 {
        let output_area = Rect::new(0, 0, output_width, 0);
        let lines = Self::output_lines(session, output_area, session.status, plan_followup_action);

        u16::try_from(lines.len()).unwrap_or(u16::MAX)
    }

    fn build_slash_menu(
        input: &str,
        stage: PromptSlashStage,
        selected_agent: Option<AgentKind>,
        session: &Session,
    ) -> Option<SlashMenu<'static>> {
        if !input.starts_with('/') {
            return None;
        }

        let (title, options): (&'static str, Vec<SlashMenuOption>) = match stage {
            PromptSlashStage::Command => {
                let lowered = input.to_lowercase();
                let commands = ["/clear", "/model"]
                    .iter()
                    .copied()
                    .filter(|command| command.starts_with(&lowered))
                    .map(|command| SlashMenuOption {
                        description: Self::command_description(command).to_string(),
                        label: command.to_string(),
                    })
                    .collect::<Vec<_>>();

                ("Slash Command (j/k move, Enter select)", commands)
            }
            PromptSlashStage::Agent => (
                "/model Agent (j/k move, Enter select)",
                AgentKind::ALL
                    .iter()
                    .map(|agent| SlashMenuOption {
                        description: agent.description().to_string(),
                        label: agent.name().to_string(),
                    })
                    .collect(),
            ),
            PromptSlashStage::Model => {
                let session_agent = selected_agent.unwrap_or_else(|| {
                    session
                        .agent
                        .parse::<AgentKind>()
                        .unwrap_or(AgentKind::Gemini)
                });
                let models = session_agent
                    .models()
                    .iter()
                    .map(|model| SlashMenuOption {
                        description: model.description().to_string(),
                        label: model.name().to_string(),
                    })
                    .collect::<Vec<_>>();

                ("/model Model (j/k move, Enter select)", models)
            }
        };
        if options.is_empty() {
            return None;
        }

        Some(SlashMenu {
            options,
            selected_index: 0,
            title,
        })
    }

    fn command_description(command: &str) -> &'static str {
        match command {
            "/clear" => "Clear chat history and start fresh.",
            "/model" => "Choose an agent and model for this session.",
            _ => "Prompt slash command.",
        }
    }

    fn build_at_mention_menu(
        input_text: &str,
        cursor: usize,
        at_mention_state: &PromptAtMentionState,
    ) -> Option<SlashMenu<'static>> {
        let (_, query) = extract_at_mention_query(input_text, cursor)?;
        let filtered = file_list::filter_entries(&at_mention_state.all_entries, &query);

        if filtered.is_empty() {
            return None;
        }

        let max_visible = 10;
        let window_start = at_mention_state
            .selected_index
            .saturating_sub(max_visible / 2);
        let window_end = filtered.len().min(window_start + max_visible);
        let window_start = window_end.saturating_sub(max_visible);

        let options: Vec<SlashMenuOption> = filtered[window_start..window_end]
            .iter()
            .map(|entry| {
                let label = if entry.is_dir {
                    format!("{}/", entry.path)
                } else {
                    entry.path.clone()
                };

                SlashMenuOption {
                    description: if entry.is_dir {
                        "folder".to_string()
                    } else {
                        String::new()
                    },
                    label,
                }
            })
            .collect();

        let display_index = at_mention_state
            .selected_index
            .min(filtered.len().saturating_sub(1))
            .saturating_sub(window_start);

        Some(SlashMenu {
            options,
            selected_index: display_index,
            title: "Files (\u{2191}\u{2193} move, Enter select, Esc dismiss)",
        })
    }

    fn render_session(&self, f: &mut Frame, area: Rect, session: &Session) {
        let bottom_height = self.bottom_height(area, session);
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
            .margin(1)
            .split(area);

        self.render_output_panel(f, chunks[0], session);
        self.render_bottom_panel(f, chunks[1], session);
    }

    fn bottom_height(&self, area: Rect, session: &Session) -> u16 {
        if let AppMode::Prompt {
            at_mention_state,
            input,
            slash_state,
            ..
        } = self.mode
        {
            let dropdown_option_count = if input.text().starts_with('/') {
                Self::build_slash_menu(
                    input.text(),
                    slash_state.stage,
                    slash_state.selected_agent,
                    session,
                )
                .map_or(0, |menu| menu.options.len().saturating_add(2))
            } else if let Some(at_state) = at_mention_state {
                Self::build_at_mention_menu(input.text(), input.cursor, at_state)
                    .map_or(0, |menu| menu.options.len().saturating_add(2))
            } else {
                0
            };

            return calculate_input_height(area.width.saturating_sub(2), input.text())
                .saturating_add(u16::try_from(dropdown_option_count).unwrap_or(u16::MAX));
        }

        1
    }

    fn render_output_panel(&self, f: &mut Frame, output_area: Rect, session: &Session) {
        let status = session.status;
        let commit_count = session.commit_count;
        let commits_label = if commit_count == 1 {
            "commit"
        } else {
            "commits"
        };
        let title = format!(
            " {} — {status} - {commit_count} {commits_label} [{}] ({}) ",
            session.display_title(),
            session.model,
            session.permission_mode.label()
        );
        let lines = Self::output_lines(session, output_area, status, self.plan_followup_action);
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

    fn output_lines(
        session: &Session,
        output_area: Rect,
        status: Status,
        plan_followup_action: Option<PlanFollowupAction>,
    ) -> Vec<Line<'static>> {
        let output_text = Self::output_text(session, status);
        let inner_width = output_area.width.saturating_sub(2) as usize;
        let mut lines = wrap_lines(output_text, inner_width)
            .into_iter()
            .map(|line| Line::from(line.to_string()))
            .collect::<Vec<_>>();

        if matches!(
            status,
            Status::InProgress
                | Status::Rebasing
                | Status::Merging
                | Status::PullRequest
                | Status::CreatingPullRequest
        ) {
            while lines.last().is_some_and(|line| line.width() == 0) {
                lines.pop();
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "{} {}",
                    Icon::current_spinner(),
                    Self::status_message(status)
                ),
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

    fn plan_followup_action_line(selected_action: PlanFollowupAction) -> Line<'static> {
        let implement_style = if selected_action == PlanFollowupAction::ImplementPlan {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let feedback_style = if selected_action == PlanFollowupAction::TypeFeedback {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD)
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

    fn status_message(status: Status) -> &'static str {
        match status {
            Status::InProgress => "Thinking...",
            Status::Rebasing => "Rebasing...",
            Status::Merging => "Merging...",
            Status::CreatingPullRequest => "Creating PR...",
            Status::PullRequest => "Waiting for PR merge...",
            Status::New | Status::Review | Status::Done | Status::Canceled => "",
        }
    }

    fn final_scroll_offset(&self, output_area: Rect, line_count: usize) -> u16 {
        if let Some(scroll_offset) = self.scroll_offset {
            return scroll_offset;
        }

        let inner_height = output_area.height.saturating_sub(2) as usize;

        u16::try_from(line_count.saturating_sub(inner_height)).unwrap_or(u16::MAX)
    }

    fn render_bottom_panel(&self, f: &mut Frame, bottom_area: Rect, session: &Session) {
        if let AppMode::Prompt {
            at_mention_state,
            input,
            slash_state,
            ..
        } = self.mode
        {
            let title = if session.prompt.is_empty() {
                " New Chat "
            } else {
                " Reply "
            };

            let menu = if input.text().starts_with('/') {
                Self::build_slash_menu(
                    input.text(),
                    slash_state.stage,
                    slash_state.selected_agent,
                    session,
                )
                .map(|mut menu| {
                    let max_index = menu.options.len().saturating_sub(1);
                    menu.selected_index = slash_state.selected_index.min(max_index);
                    menu
                })
            } else if let Some(at_state) = at_mention_state {
                Self::build_at_mention_menu(input.text(), input.cursor, at_state)
            } else {
                None
            };

            ChatInput::new(title, input.text(), input.cursor, "Type your message", menu)
                .render(f, bottom_area);

            return;
        }

        let help_text = Self::view_help_text(session, self.plan_followup_action);
        let help_message = Paragraph::new(help_text).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, bottom_area);
    }

    fn view_help_text(
        session: &Session,
        plan_followup_action: Option<PlanFollowupAction>,
    ) -> &'static str {
        if session.status == Status::Done {
            return "q: back | o: open | j/k: scroll | ?: help";
        }

        if plan_followup_action.is_some() {
            return if session.commit_count > 0 {
                "q: back | \u{2190}/\u{2192}: choose action | enter: confirm | d: diff | p: pr | \
                 m: merge | r: rebase | S-Tab: mode | j/k: scroll | ?: help"
            } else {
                "q: back | \u{2190}/\u{2192}: choose action | enter: confirm | d: diff | S-Tab: \
                 mode | j/k: scroll | ?: help"
            };
        }

        if session.commit_count > 0 {
            "q: back | enter: reply | o: open | d: diff | p: pr | m: merge | r: rebase | S-Tab: \
             mode | j/k: scroll | ?: help"
        } else {
            "q: back | enter: reply | o: open | d: diff | S-Tab: mode | j/k: scroll | ?: help"
        }
    }
}

impl Page for SessionChatPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        if let Some(session) = self.sessions.get(self.session_index) {
            self.render_session(f, area, session);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::file_list::FileEntry;
    use crate::model::{PermissionMode, SessionSize, SessionStats};

    fn session_fixture() -> Session {
        Session {
            agent: "gemini".to_string(),
            base_branch: "main".to_string(),
            commit_count: 0,
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: "gemini-3-flash-preview".to_string(),
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
    fn test_build_slash_menu_for_command_stage_has_description() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu =
            SessionChatPage::build_slash_menu("/m", PromptSlashStage::Command, None, &session)
                .expect("expected slash menu");

        // Assert
        assert_eq!(menu.options.len(), 1);
        assert_eq!(menu.options[0].label, "/model");
        assert_eq!(
            menu.options[0].description,
            "Choose an agent and model for this session."
        );
    }

    #[test]
    fn test_build_slash_menu_for_agent_stage_has_agent_descriptions() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu =
            SessionChatPage::build_slash_menu("/model", PromptSlashStage::Agent, None, &session)
                .expect("expected slash menu");

        // Assert
        assert_eq!(menu.options.len(), AgentKind::ALL.len());
        assert_eq!(menu.options[0].label, "gemini");
        assert_eq!(menu.options[0].description, "Google Gemini CLI agent.");
    }

    #[test]
    fn test_build_slash_menu_for_model_stage_has_model_descriptions() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu = SessionChatPage::build_slash_menu(
            "/model",
            PromptSlashStage::Model,
            Some(AgentKind::Codex),
            &session,
        )
        .expect("expected slash menu");

        // Assert
        assert_eq!(menu.options.len(), AgentKind::Codex.models().len());
        assert_eq!(menu.options[0].label, "gpt-5.3-codex");
        assert_eq!(
            menu.options[0].description,
            "Latest Codex model for coding quality."
        );
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
    fn test_build_at_mention_menu_with_matches() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu =
            SessionChatPage::build_at_mention_menu("@src", 4, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options.len(), 3);
        assert_eq!(menu.options[0].label, "src/");
        assert_eq!(menu.options[0].description, "folder");
        assert_eq!(menu.options[1].label, "src/lib.rs");
        assert_eq!(menu.options[2].label, "src/main.rs");
    }

    #[test]
    fn test_build_at_mention_menu_with_trailing_slash_includes_exact_directory() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu =
            SessionChatPage::build_at_mention_menu("@src/", 5, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options[0].label, "src/");
        assert_eq!(menu.options[0].description, "folder");
        assert_eq!(menu.options[1].label, "src/lib.rs");
        assert_eq!(menu.options[2].label, "src/main.rs");
    }

    #[test]
    fn test_build_at_mention_menu_no_matches() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu = SessionChatPage::build_at_mention_menu("@nonexistent", 12, &state);

        // Assert
        assert!(menu.is_none());
    }

    #[test]
    fn test_build_at_mention_menu_empty_query_returns_all() {
        // Arrange
        let state = PromptAtMentionState::new(file_entries_fixture());

        // Act
        let menu = SessionChatPage::build_at_mention_menu("@", 1, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options.len(), 7);
    }

    #[test]
    fn test_build_at_mention_menu_caps_at_10() {
        // Arrange
        let entries: Vec<FileEntry> = (0..20)
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:02}.rs"),
            })
            .collect();
        let state = PromptAtMentionState::new(entries);

        // Act
        let menu = SessionChatPage::build_at_mention_menu("@", 1, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options.len(), 10);
    }

    #[test]
    fn test_build_at_mention_menu_clamps_selected_index() {
        // Arrange
        let mut state = PromptAtMentionState::new(file_entries_fixture());
        state.selected_index = 100; // Way beyond bounds

        // Act
        let menu =
            SessionChatPage::build_at_mention_menu("@src", 4, &state).expect("expected menu");

        // Assert — should clamp to last visible item
        assert_eq!(menu.selected_index, 2);
    }

    #[test]
    fn test_build_at_mention_menu_scroll_window() {
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
        let menu = SessionChatPage::build_at_mention_menu("@", 1, &state).expect("expected menu");

        // Assert — window should be centered around index 15
        assert_eq!(menu.options.len(), 10);
        assert_eq!(menu.options[0].label, "file_10.rs");
        assert_eq!(menu.options[9].label, "file_19.rs");
        assert_eq!(menu.selected_index, 5); // 15 - 10 = 5
    }

    #[test]
    fn test_build_at_mention_menu_directory_has_trailing_slash() {
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
            SessionChatPage::build_at_mention_menu("@src", 4, &state).expect("expected menu");

        // Assert
        assert_eq!(menu.options[0].label, "src/");
        assert_eq!(menu.options[0].description, "folder");
        assert_eq!(menu.options[1].label, "src/main.rs");
        assert_eq!(menu.options[1].description, "");
    }

    #[test]
    fn test_build_slash_menu_for_command_stage_includes_clear() {
        // Arrange
        let session = session_fixture();

        // Act
        let menu =
            SessionChatPage::build_slash_menu("/", PromptSlashStage::Command, None, &session)
                .expect("expected slash menu");

        // Assert
        let labels: Vec<&str> = menu.options.iter().map(|opt| opt.label.as_str()).collect();
        assert!(labels.contains(&"/clear"));
        assert!(labels.contains(&"/model"));
    }

    #[test]
    fn test_command_description_clear() {
        // Arrange & Act
        let description = SessionChatPage::command_description("/clear");

        // Assert
        assert_eq!(description, "Clear chat history and start fresh.");
    }

    #[test]
    fn test_output_lines_uses_summary_for_done_session() {
        // Arrange
        let mut session = session_fixture();
        session.output = "streamed output".to_string();
        session.summary = Some("terminal summary".to_string());
        session.status = Status::Done;

        // Act
        let lines =
            SessionChatPage::output_lines(&session, Rect::new(0, 0, 80, 5), session.status, None);
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
        let lines =
            SessionChatPage::output_lines(&session, Rect::new(0, 0, 80, 5), session.status, None);
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
    fn test_status_message_for_merging() {
        // Arrange & Act
        let message = SessionChatPage::status_message(Status::Merging);

        // Assert
        assert_eq!(message, "Merging...");
    }

    #[test]
    fn test_output_lines_appends_empty_line_when_done() {
        // Arrange
        let mut session = session_fixture();
        session.output = "some output".to_string();
        session.status = Status::Done;

        // Act
        let lines =
            SessionChatPage::output_lines(&session, Rect::new(0, 0, 80, 5), session.status, None);

        // Assert
        assert!(lines.last().expect("lines").to_string().is_empty());
        // wrap_lines might produce 1 line for "some output". +1 empty line = 2.
        assert!(lines.len() >= 2);
    }

    #[test]
    fn test_output_lines_appends_empty_line_before_spinner() {
        // Arrange
        let mut session = session_fixture();
        session.output = "some output".to_string();
        session.status = Status::InProgress;

        // Act
        let lines =
            SessionChatPage::output_lines(&session, Rect::new(0, 0, 80, 5), session.status, None);

        // Assert
        // Expected: "some output", "", "Spinner..."
        assert!(lines.len() >= 3);
        let len = lines.len();
        assert!(lines[len - 2].to_string().is_empty());
        assert!(lines[len - 1].to_string().contains("Thinking..."));
    }

    #[test]
    fn test_rendered_output_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        session.output = "word ".repeat(40);
        let raw_line_count = u16::try_from(session.output.lines().count()).unwrap_or(u16::MAX);

        // Act
        let rendered_line_count = SessionChatPage::rendered_output_line_count(&session, 20, None);

        // Assert
        assert!(rendered_line_count > raw_line_count);
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
        let lines = SessionChatPage::output_lines(
            &session,
            area,
            Status::Review,
            Some(PlanFollowupAction::ImplementPlan),
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
    fn test_plan_followup_action_line_contains_both_actions() {
        // Arrange & Act
        let line = SessionChatPage::plan_followup_action_line(PlanFollowupAction::TypeFeedback);
        let rendered = line.to_string();

        // Assert
        assert!(rendered.contains("Implement the plan"));
        assert!(rendered.contains("Type feedback"));
    }

    #[test]
    fn test_view_help_text_without_commits_excludes_git_actions() {
        // Arrange
        let session = session_fixture();

        // Act
        let help_text = SessionChatPage::view_help_text(&session, None);

        // Assert
        assert!(!help_text.contains("p: pr"));
        assert!(!help_text.contains("m: merge"));
        assert!(!help_text.contains("r: rebase"));
    }

    #[test]
    fn test_view_help_text_with_commits_includes_git_actions() {
        // Arrange
        let mut session = session_fixture();
        session.commit_count = 1;

        // Act
        let help_text = SessionChatPage::view_help_text(&session, None);

        // Assert
        assert!(help_text.contains("p: pr"));
        assert!(help_text.contains("m: merge"));
        assert!(help_text.contains("r: rebase"));
    }

    #[test]
    fn test_view_help_text_plan_followup_without_commits_excludes_git_actions() {
        // Arrange
        let session = session_fixture();

        // Act
        let help_text =
            SessionChatPage::view_help_text(&session, Some(PlanFollowupAction::ImplementPlan));

        // Assert
        assert!(!help_text.contains("p: pr"));
        assert!(!help_text.contains("m: merge"));
        assert!(!help_text.contains("r: rebase"));
    }
}
