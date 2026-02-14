use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::agent::{AgentKind, AgentSelectionMetadata};
use crate::icon::Icon;
use crate::model::{AppMode, PromptSlashStage, Session, Status};
use crate::ui::components::chat_input::{ChatInput, SlashMenu, SlashMenuOption};
use crate::ui::util::{calculate_input_height, wrap_lines};
use crate::ui::{Component, Page};

/// Chat page renderer for a single session.
pub struct SessionChatPage<'a> {
    pub mode: &'a AppMode,
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
    ) -> Self {
        Self {
            mode,
            scroll_offset,
            session_index,
            sessions,
        }
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
                let commands = ["/model"]
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
            "/model" => "Choose an agent and model for this session.",
            _ => "Prompt slash command.",
        }
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
            input, slash_state, ..
        } = self.mode
        {
            let slash_option_count = Self::build_slash_menu(
                input.text(),
                slash_state.stage,
                slash_state.selected_agent,
                session,
            )
            .map_or(0, |menu| menu.options.len().saturating_add(2));

            return calculate_input_height(area.width.saturating_sub(2), input.text())
                .saturating_add(u16::try_from(slash_option_count).unwrap_or(u16::MAX));
        }

        1
    }

    fn render_output_panel(&self, f: &mut Frame, output_area: Rect, session: &Session) {
        let status = session.status();
        let commit_count = session.commit_count();
        let commits_label = if commit_count == 1 {
            "commit"
        } else {
            "commits"
        };
        let title = format!(
            " {} — {status} - {commit_count} {commits_label} [{}:{}] ",
            session.display_title(),
            session.agent,
            session.model
        );
        let lines = Self::output_lines(session, output_area, status);
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

    fn output_lines(session: &Session, output_area: Rect, status: Status) -> Vec<Line<'static>> {
        let output_text = session
            .output
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default();
        let inner_width = output_area.width.saturating_sub(2) as usize;
        let mut lines = wrap_lines(&output_text, inner_width)
            .into_iter()
            .map(|line| Line::from(line.to_string()))
            .collect::<Vec<_>>();

        if matches!(
            status,
            Status::InProgress | Status::PullRequest | Status::CreatingPullRequest
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
        }

        lines
    }

    fn status_message(status: Status) -> &'static str {
        match status {
            Status::InProgress => "Thinking...",
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
            input, slash_state, ..
        } = self.mode
        {
            let title = if session.prompt.is_empty() {
                " New Chat "
            } else {
                " Reply "
            };
            let slash_menu = Self::build_slash_menu(
                input.text(),
                slash_state.stage,
                slash_state.selected_agent,
                session,
            )
            .map(|mut menu| {
                let max_index = menu.options.len().saturating_sub(1);
                menu.selected_index = slash_state.selected_index.min(max_index);
                menu
            });

            ChatInput::new(
                title,
                input.text(),
                input.cursor,
                "Type your message",
                slash_menu,
            )
            .render(f, bottom_area);

            return;
        }

        let help_message = Paragraph::new(
            "q: back | enter: reply | d: diff | p: pr | m: merge | j/k: scroll | ?: help",
        )
        .style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, bottom_area);
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
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::model::SessionStats;

    fn session_fixture() -> Session {
        Session {
            agent: "gemini".to_string(),
            base_branch: "main".to_string(),
            commit_count: Arc::new(Mutex::new(0)),
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: "project".to_string(),
            prompt: String::new(),
            stats: SessionStats::default(),
            status: Arc::new(Mutex::new(Status::New)),
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
}
