use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

use crate::domain::agent::{AgentKind, AgentSelectionMetadata};
use crate::domain::input::extract_at_mention_query;
use crate::domain::permission::PlanFollowup;
use crate::domain::session::{Session, Status};
use crate::file_list;
use crate::ui::components::chat_input::{ChatInput, SlashMenu, SlashMenuOption};
use crate::ui::components::session_output::SessionOutput;
use crate::ui::state::app_mode::AppMode;
use crate::ui::state::prompt::{PromptAtMentionState, PromptSlashStage};
use crate::ui::util::calculate_input_height;
use crate::ui::{Component, Page};

/// Chat page renderer for a single session.
pub struct SessionChatPage<'a> {
    pub active_progress: Option<&'a str>,
    pub mode: &'a AppMode,
    pub plan_followup: Option<&'a PlanFollowup>,
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
        plan_followup: Option<&'a PlanFollowup>,
        active_progress: Option<&'a str>,
    ) -> Self {
        Self {
            active_progress,
            mode,
            plan_followup,
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
        plan_followup: Option<&PlanFollowup>,
        active_progress: Option<&str>,
    ) -> u16 {
        SessionOutput::rendered_line_count(session, output_width, plan_followup, active_progress)
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
                let commands = ["/clear", "/model", "/stats"]
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
                let session_agent = selected_agent.unwrap_or_else(|| session.model.kind());
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
            "/stats" => "Check session stats.",
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

        SessionOutput::new(
            session,
            self.scroll_offset,
            self.plan_followup,
            self.active_progress,
        )
        .render(f, chunks[0]);
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

    fn render_bottom_panel(&self, f: &mut Frame, bottom_area: Rect, session: &Session) {
        if let AppMode::Prompt {
            at_mention_state,
            input,
            slash_state,
            ..
        } = self.mode
        {
            let title = format!(
                " [{}] ({}) ",
                session.model.as_str(),
                session.permission_mode.label()
            );

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

            ChatInput::new(
                &title,
                input.text(),
                input.cursor,
                "Type your message",
                menu,
            )
            .render(f, bottom_area);

            return;
        }

        let help_text = Self::view_help_text(session, self.plan_followup);
        let help_message = Paragraph::new(help_text).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, bottom_area);
    }

    fn view_help_text(session: &Session, plan_followup: Option<&PlanFollowup>) -> &'static str {
        if session.status == Status::Done {
            return "q: back | o: open | j/k: scroll | ?: help";
        }

        if let Some(plan_followup) = plan_followup {
            if plan_followup.options.len() >= 4 {
                return "q: back | \u{2191}/\u{2193}: choose action | enter: confirm | d: diff | \
                        m: queue merge | r: rebase | S-Tab: mode | j/k: scroll | ?: help";
            }

            return "q: back | \u{2190}/\u{2192}: choose action | enter: confirm | d: diff | m: \
                    queue merge | r: rebase | S-Tab: mode | j/k: scroll | ?: help";
        }

        "q: back | enter: reply | o: open | d: diff | m: queue merge | r: rebase | S-Tab: mode | \
         j/k: scroll | ?: help"
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
    use crate::agent::AgentModel;
    use crate::domain::file::FileEntry;
    use crate::domain::permission::PermissionMode;
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
        assert!(labels.contains(&"/stats"));
    }

    #[test]
    fn test_command_description_clear() {
        // Arrange & Act
        let description = SessionChatPage::command_description("/clear");

        // Assert
        assert_eq!(description, "Clear chat history and start fresh.");
    }

    #[test]
    fn test_command_description_stats() {
        // Arrange & Act
        let description = SessionChatPage::command_description("/stats");

        // Assert
        assert_eq!(description, "Check session stats.");
    }

    #[test]
    fn test_rendered_output_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        session.output = "word ".repeat(40);
        let raw_line_count = u16::try_from(session.output.lines().count()).unwrap_or(u16::MAX);

        // Act
        let rendered_line_count =
            SessionChatPage::rendered_output_line_count(&session, 20, None, None);

        // Assert
        assert!(rendered_line_count > raw_line_count);
    }

    #[test]
    fn test_view_help_text_includes_git_actions() {
        // Arrange
        let session = session_fixture();

        // Act
        let help_text = SessionChatPage::view_help_text(&session, None);

        // Assert
        assert!(help_text.contains("m: queue merge"));
        assert!(help_text.contains("r: rebase"));
    }

    #[test]
    fn test_view_help_text_plan_followup_includes_git_actions() {
        // Arrange
        let session = session_fixture();
        let followup = PlanFollowup::new(Vec::new());

        // Act
        let help_text = SessionChatPage::view_help_text(&session, Some(&followup));

        // Assert
        assert!(help_text.contains("m: queue merge"));
        assert!(help_text.contains("r: rebase"));
    }

    #[test]
    fn test_view_help_text_question_followup_uses_up_down_hints() {
        // Arrange
        let session = session_fixture();
        let followup = PlanFollowup::new(vec![
            "Question one?".to_string(),
            "Question two?".to_string(),
        ]);

        // Act
        let help_text = SessionChatPage::view_help_text(&session, Some(&followup));

        // Assert
        assert!(help_text.contains("\u{2191}/\u{2193}"));
    }
}
