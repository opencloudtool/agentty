use std::io;

use ag_tui_text::text_util::inline_text;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Tab};
use crate::domain::input::InputCommand;
use crate::domain::session::{Session, Status};
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, HelpContext};
use crate::presentation::help_action::{
    HelpAction, project_list_actions, session_list_actions, settings_actions,
};
use crate::presentation::settings::{SettingsAction, SettingsInput};
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;
use crate::runtime::mode::input_key;

/// Handles key input while the app is in list mode.
///
/// Pressing `q` opens a confirmation overlay instead of quitting immediately,
/// with `No` selected by default. Pressing `Enter` on the `Projects` tab
/// selects the active project and then moves focus to `Tab::Sessions`.
/// `c` opens a cancel confirmation overlay for running sessions, review
/// sessions, unstarted draft sessions, and draft orchestrators, and `Tab`
/// cycles tabs forward while `Shift+Tab` cycles backward.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    if app.tabs.current() == Tab::Settings
        && (app
            .settings_presentation
            .is_launch_configuration_list_editor_open()
            || app.settings_presentation.is_selector_dropdown_open())
    {
        if let Some(input) = settings_input_for_key(key)
            && let Some(action) = app.settings_presentation.action_for_input(input)
        {
            apply_settings_action(app, action).await;
        }

        return Ok(EventResult::Continue);
    }

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                confirmation_message: "Quit agentty?".to_string(),
                confirmation_title: "Confirm Quit".to_string(),
                restore_view: None,
                session_id: None,
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            };

            return Ok(EventResult::Continue);
        }
        KeyCode::Tab => {
            app.next_tab();
            app.persist_current_tab().await;
        }
        KeyCode::BackTab => {
            app.previous_tab();
            app.persist_current_tab().await;
        }
        KeyCode::Char('a')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::NONE =>
        {
            open_session_creation_flow(app).await;
        }
        KeyCode::Char('p')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::NONE =>
        {
            app.mode = AppMode::ProjectSwitcher {
                selected_option_index: 0,
            };
        }
        KeyCode::Char('j') | KeyCode::Down => match app.tabs.current() {
            Tab::Projects => app.next_project(),
            Tab::Sessions => app.next(),
            Tab::Settings => apply_settings_action(app, SettingsAction::Next).await,
        },
        KeyCode::Char('k') | KeyCode::Up => match app.tabs.current() {
            Tab::Projects => app.previous_project(),
            Tab::Sessions => app.previous(),
            Tab::Settings => apply_settings_action(app, SettingsAction::Previous).await,
        },
        KeyCode::Enter => return handle_enter_key(app).await,
        KeyCode::Char('c') if app.tabs.current() == Tab::Sessions => {
            let selected_session = app.selected_session().and_then(|session| {
                session
                    .allows_cancel_action()
                    .then(|| (session.id.clone(), inline_text(session.display_title())))
            });
            if let Some((session_id, session_title)) = selected_session {
                let running_child_count = app.orchestration_running_child_count(&session_id).await;
                let confirmation_message =
                    cancel_confirmation_message(&session_title, running_child_count);
                app.mode = AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::CancelSession,
                    confirmation_message,
                    confirmation_title: "Confirm Cancel".to_string(),
                    restore_view: None,
                    session_id: Some(session_id),
                    selected_confirmation_index: DEFAULT_OPTION_INDEX,
                };
            }
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'s') => sync_list_context(app),
        KeyCode::Char('?') => {
            open_list_help_overlay(app);
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Translates terminal-specific keys into frontend-neutral settings input.
fn settings_input_for_key(key: KeyEvent) -> Option<SettingsInput> {
    let input_command = input_key::command_for_key(key, input_key::InputCapabilities::SINGLE_LINE);

    match (key.code, input_command) {
        (KeyCode::Esc, _) => Some(SettingsInput::Cancel),
        (KeyCode::Enter, _) => Some(SettingsInput::Confirm),
        (KeyCode::Down, _) => Some(SettingsInput::MoveDown),
        (KeyCode::Up, _) => Some(SettingsInput::MoveUp),
        (KeyCode::Char(character), Some(InputCommand::Insert(_))) => {
            Some(SettingsInput::Character(character))
        }
        (_, Some(command)) => Some(SettingsInput::Edit(command)),
        (_, None) => None,
    }
}

fn cancel_confirmation_message(session_title: &str, running_child_count: usize) -> String {
    if running_child_count > 0 {
        return format!("Cancel orchestration and its {running_child_count} running children?");
    }

    format!("Cancel session \"{session_title}\"?")
}

/// Opens the session selector, preceded by an advisory when configured
/// pre-commit validation has no executable Git hook.
async fn open_session_creation_flow(app: &mut App) {
    if let Some(warning) = app.pre_commit_hook_warning().await {
        app.mode = AppMode::PreCommitHookWarning {
            message: format!(
                "{warning}\n\nPress Enter to continue to session options, or Esc to cancel."
            ),
        };

        return;
    }

    app.mode = AppMode::SessionCreation {
        selected_option_index: 0,
    };
}

/// Handles `Enter` in list mode and triggers the selected tab primary action.
///
/// On the sessions tab, any selected session can be opened in view mode.
async fn handle_enter_key(app: &mut App) -> io::Result<EventResult> {
    match app.tabs.current() {
        Tab::Projects => {
            if app.switch_selected_project().await.is_ok() {
                app.tabs.set(Tab::Sessions);
                app.persist_current_tab().await;
            }
        }
        Tab::Sessions => {
            if let Some(session_index) = app.sessions.selected_session_index() {
                let Some(session_id) = app
                    .sessions
                    .session_at(session_index)
                    .map(|session| session.id.clone())
                else {
                    return Ok(EventResult::Continue);
                };

                app.sessions
                    .load_session_detail_into_state(app.services.db(), session_id.as_str())
                    .await;
                app.restore_review_output(&session_id);

                let questions = app
                    .sessions
                    .session_at(session_index)
                    .filter(|session| session.status == Status::Question)
                    .map(|session| session.questions.clone());
                if let Some(questions) = questions {
                    app.enter_question_mode(session_id.as_str(), questions);
                } else if !app.restore_prompt_progress(session_id.as_str()).await {
                    app.mode = AppMode::View {
                        session_id,
                        scroll_offset: None,
                    };
                }
            }
        }
        Tab::Settings => {
            apply_settings_action(app, SettingsAction::Activate).await;
        }
    }

    Ok(EventResult::Continue)
}

/// Applies a semantic settings-screen action and persists any requested value
/// change through the narrow settings application service.
async fn apply_settings_action(app: &mut App, action: SettingsAction) {
    let operation = {
        let view = app.settings.view();

        app.settings_presentation.apply(&view, action)
    };

    if let Some(operation) = operation {
        app.settings.apply_operation(operation).await;
    }
}

/// Starts the sync action for the active project.
fn sync_list_context(app: &mut App) {
    app.start_sync_main();
}

/// Opens the help overlay with list-mode action availability projection.
fn open_list_help_overlay(app: &mut App) {
    let keybindings = list_keybindings(app);

    app.mode = AppMode::Help {
        context: HelpContext::List { keybindings },
        scroll_offset: 0,
    };
}

/// Projects current list-mode action availability into keybinding entries.
fn list_keybindings(app: &App) -> Vec<HelpAction> {
    if app.tabs.current() == Tab::Projects {
        return project_list_actions();
    }

    if app.tabs.current() == Tab::Settings {
        return settings_actions();
    }

    let is_sessions_tab = app.tabs.current() == Tab::Sessions;
    let selected_session = app.selected_session();
    let can_cancel_selected_session =
        is_sessions_tab && selected_session.is_some_and(Session::allows_cancel_action);
    let can_open_selected_session = is_sessions_tab
        && app
            .sessions
            .selected_session_index()
            .and_then(|selected_index| app.sessions.session_at(selected_index))
            .is_some();
    session_list_actions(can_cancel_selected_session, can_open_selected_session)
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::{
        AppEvent, MockSyncMainRunner, ProjectSyncPhase, SyncMainOutcome, SyncSessionStartError,
    };
    use crate::domain::input::InputState;
    use crate::domain::question::QuestionItem;
    use crate::domain::theme::ColorTheme;
    use crate::presentation::app_mode::{ChatFocus, PromptModeSnapshot};
    use crate::presentation::prompt::{
        PromptAttachmentState, PromptHistoryState, PromptSlashState,
    };

    /// Builds a settings-focused test app with the `Launch Configurations` row
    /// selected.
    async fn new_test_app_for_settings() -> (App, tempfile::TempDir) {
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        app.create_session()
            .await
            .expect("failed to create session for settings tests");
        app.tabs.set(Tab::Settings);
        let launch_configuration_row_index = app
            .settings_presentation
            .snapshot(&app.settings.view())
            .project_rows
            .iter()
            .position(|(setting_name, _)| *setting_name == "Launch Configurations")
            .map(|project_index| {
                project_index
                    + app
                        .settings_presentation
                        .snapshot(&app.settings.view())
                        .global_rows
                        .len()
            })
            .expect("missing Launch Configurations setting row");
        for _ in 0..launch_configuration_row_index {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            )
            .await
            .expect("failed to select Launch Configurations setting row");
        }

        (app, base_dir)
    }

    /// Replaces sync background execution with one immediate completion event.
    fn mock_sync_main_completion(
        app: &mut App,
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    ) {
        let mut mock_sync_main_runner = MockSyncMainRunner::new();
        mock_sync_main_runner
            .expect_start_sync_main()
            .times(1)
            .returning(move |app_event_tx, operation, _, _| {
                let _ = app_event_tx.send(AppEvent::SyncMainCompleted {
                    completion: crate::app::SyncMainCompletion {
                        operation,
                        result: result.clone(),
                        review_request_updates: Vec::new(),
                    },
                });
            });
        app.sync_main_runner = std::sync::Arc::new(mock_sync_main_runner);
    }

    /// Builds one saved composer snapshot for list-reopen tests.
    fn saved_prompt_snapshot(session_id: &str, input_text: &str) -> PromptModeSnapshot {
        PromptModeSnapshot {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::new(Vec::new()),
            input: InputState::with_text(input_text.to_string()),
            scroll_offset: Some(3),
            session_id: session_id.into(),
            slash_state: PromptSlashState::default(),
        }
    }

    #[test]
    fn settings_input_for_key_maps_terminal_keys() {
        // Arrange
        let mappings = [
            (
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                Some(SettingsInput::Cancel),
            ),
            (
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                Some(SettingsInput::Confirm),
            ),
            (
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                Some(SettingsInput::MoveDown),
            ),
            (
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                Some(SettingsInput::MoveUp),
            ),
            (
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
                Some(SettingsInput::Character('x')),
            ),
            (
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
                Some(SettingsInput::Edit(InputCommand::DeleteWordBackward)),
            ),
            (KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE), None),
        ];

        // Act / Assert
        for (key, expected_input) in mappings {
            assert_eq!(settings_input_for_key(key), expected_input);
        }
    }

    #[tokio::test]
    async fn test_handle_quit_key_shows_confirm_quit_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: None,
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if confirmation_title == "Confirm Quit" && confirmation_message == "Quit agentty?"
        ));
    }

    #[tokio::test]
    async fn test_handle_backtab_key_cycles_tabs_backward() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Projects);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.tabs.current(), Tab::Settings);
    }

    #[tokio::test]
    async fn test_handle_add_key_opens_session_creation_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Sessions);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(
            app.mode,
            AppMode::SessionCreation {
                selected_option_index: 0,
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_add_key_warns_before_options_when_pre_commit_hook_is_missing() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        std::fs::write(
            base_dir.path().join(".pre-commit-config.yaml"),
            "repos: []\n",
        )
        .expect("failed to write pre-commit configuration");
        let git_config_output = std::process::Command::new("git")
            .args(["config", "core.hooksPath", ".missing-hooks"])
            .current_dir(base_dir.path())
            .output()
            .expect("failed to configure missing hooks path");
        assert!(git_config_output.status.success());
        app.tabs.set(Tab::Sessions);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(
            &app.mode,
            AppMode::PreCommitHookWarning { message }
                if message.contains("prek install")
                    && message.contains("pre-commit install")
                    && message.contains("will become an error in a future release")
                    && message.contains("Press Enter to continue")
        ));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_opens_switcher_on_sessions_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Sessions);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ProjectSwitcher {
                selected_option_index: 0,
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_ignored_on_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Projects);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_add_key_ignored_on_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Projects);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_add_key_ignored_on_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Settings);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_selected_session_in_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_restores_cached_review_output() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.review_cache.insert(
            expected_session_id.clone().into(),
            crate::app::ReviewCacheEntry::Ready {
                diff_hash: 7,
                text: "Focused review".to_string(),
            },
        );
        crate::test_support::set_session_status_for_test(
            &mut app,
            &expected_session_id,
            Status::Review,
        );
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == &expected_session_id
        ));
        assert_eq!(
            app.sessions.sessions()[0]
                .transient_messages
                .get(crate::domain::transient_message::TransientMessageSlot::Review)
                .map(|message| message.body.text()),
            Some("Focused review")
        );
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_selected_question_session_in_question_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let expected_questions: Vec<QuestionItem> = vec![
            QuestionItem {
                options: vec!["main".to_string(), "develop".to_string()],
                text: "Need a target branch?".to_string(),
            },
            QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Need migration notes?".to_string(),
            },
        ];
        if let Some(session) = app.sessions.sessions_mut().first_mut() {
            session.status = Status::Question;
            session.questions = expected_questions.clone();
        }
        app.review_cache.insert(
            expected_session_id.clone().into(),
            crate::app::ReviewCacheEntry::Ready {
                text: "Focused review".to_string(),
                diff_hash: 42,
            },
        );
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                ref questions,
                current_index: 0,
                ref responses,
                ref input,
                selected_option_index: Some(0),
                ..
            } if session_id == &expected_session_id
                && questions == &expected_questions
                && responses.is_empty()
                && input.text().is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_restores_saved_prompt_progress() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.save_prompt_progress(saved_prompt_snapshot(&session_id, "saved draft"));
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            &app.mode,
            AppMode::Prompt {
                focus: ChatFocus::Input,
                input,
                scroll_offset: Some(3),
                session_id: restored_session_id,
                ..
            } if input.text() == "saved draft" && restored_session_id == &session_id
        ));
        assert!(app.prompt_progress.is_empty());
    }

    #[tokio::test]
    async fn test_handle_enter_key_discards_saved_prompt_for_terminal_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.save_prompt_progress(saved_prompt_snapshot(&session_id, "stale draft"));
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Canceled);
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            &app.mode,
            AppMode::View {
                session_id: restored_session_id,
                ..
            } if restored_session_id == &session_id
        ));
        assert!(app.prompt_progress.is_empty());
    }

    #[tokio::test]
    async fn test_handle_enter_key_keeps_persisted_size_until_turn_completion() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.services
            .db()
            .sessions()
            .update_session_diff_stats(0, 0, false, &expected_session_id, "XS")
            .await
            .expect("failed to set stale size");
        let session_index = app
            .session_index_for_id(&expected_session_id)
            .expect("missing created session");
        let session_folder = app.sessions.sessions()[session_index].folder.clone();
        let changed_lines = "line\n".repeat(40);
        std::fs::write(session_folder.join("open-size-test.txt"), changed_lines)
            .expect("failed to write test file");
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");
        let db_sessions = app
            .services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        let db_size = db_sessions
            .iter()
            .find(|db_session| db_session.id == expected_session_id)
            .map(|db_session| db_session.size.clone())
            .expect("missing persisted session");
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == &expected_session_id
        ));
        assert_eq!(db_size, "XS");
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_done_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Some(session) = app.sessions.sessions_mut().first_mut() {
            session.status = Status::Done;
        }
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_canceled_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Some(session) = app.sessions.sessions_mut().first_mut() {
            session.status = Status::Canceled;
        }
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_switches_to_sessions_tab_from_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Projects);
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.tabs.current(), Tab::Sessions);
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_launch_configuration_list_editor_in_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");
        let editor = app
            .settings_presentation
            .snapshot(&app.settings.view())
            .launch_configuration_list_editor
            .expect("expected launch-configuration list editor");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(
            editor.commands,
            vec!["cargo test".to_string(), "npm run dev".to_string()]
        );
    }

    #[tokio::test]
    async fn test_settings_previous_key_wraps_to_default_response_style_row() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to move settings selection");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(
            app.settings_presentation
                .snapshot(&app.settings.view())
                .selected_row_index,
            Some(8)
        );
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_settings_selector_dropdown() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");
        let selector_dropdown = app
            .settings_presentation
            .snapshot(&app.settings.view())
            .selector_dropdown
            .expect("expected settings selector dropdown");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(selector_dropdown.row_index, 0);
        assert_eq!(selector_dropdown.selected_index, 0);
        assert_eq!(selector_dropdown.options[0].label, "Agentty Default");
    }

    #[tokio::test]
    async fn test_settings_selector_dropdown_ignores_unmapped_key() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings selector dropdown");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))
            .await
            .expect("failed to ignore unmapped settings key");
        let selector_dropdown = app
            .settings_presentation
            .snapshot(&app.settings.view())
            .selector_dropdown
            .expect("settings selector dropdown should remain open");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(selector_dropdown.selected_index, 0);
    }

    #[tokio::test]
    async fn test_settings_selector_dropdown_keys_select_theme_value() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings selector dropdown");

        // Act
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to move dropdown selection");
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to select dropdown option");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.theme, ColorTheme::Green);
        assert!(!app.settings_presentation.is_selector_dropdown_open());
    }

    #[tokio::test]
    async fn test_settings_selector_dropdown_q_closes_without_quit_confirmation() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings selector dropdown");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to close settings selector dropdown");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(!app.settings_presentation.is_selector_dropdown_open());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_settings_selector_previous_and_escape_close_dropdown() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings selector dropdown");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to move selector forward");

        // Act
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to move selector backward");
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to close selector dropdown");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(!app.settings_presentation.is_selector_dropdown_open());
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_adds_command_on_confirm() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start adding launch configuration");

        // Act
        for character in "nvim".chars() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .await
            .expect("failed to type launch configuration");
        }
        assert_eq!(app.settings.launch_configuration, "");

        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to confirm launch configuration");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "nvim");
        assert_eq!(app.sessions.sessions().len(), 1);
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_backspace_updates_pending_input_only() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start adding launch configuration");
        for character in "abc".chars() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .await
            .expect("failed to type launch configuration");
        }

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");
        let editor = app
            .settings_presentation
            .snapshot(&app.settings.view())
            .launch_configuration_list_editor
            .expect("expected launch-configuration list editor");
        let input = editor.input.expect("expected active input");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(input.text(), "ab");
        assert_eq!(app.settings.launch_configuration, "");
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_edits_selected_command() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to select second command");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start editing launch configuration");
        for _ in 0.."npm run dev".chars().count() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            )
            .await
            .expect("failed to clear command");
        }

        // Act
        for character in "lazygit".chars() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .await
            .expect("failed to type replacement command");
        }
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to confirm edited launch configuration");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "cargo test\nlazygit");
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_delete_removes_selected_command() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to select second command");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "cargo test");
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_reorders_selected_command() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to reorder launch configuration");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "npm run dev\ncargo test");
    }

    #[tokio::test]
    async fn test_launch_configuration_editor_previous_move_up_and_q_close() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to select second command");

        // Act
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to select previous command");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to reselect second command");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to move command up");
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to close launch-configuration editor");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "npm run dev\ncargo test");
        assert!(
            !app.settings_presentation
                .is_launch_configuration_list_editor_open()
        );
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_esc_closes_from_browse() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle Esc key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(
            !app.settings_presentation
                .is_launch_configuration_list_editor_open()
        );
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_esc_cancels_input_without_closing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start adding launch configuration");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to type launch configuration");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle Esc key");
        let editor = app
            .settings_presentation
            .snapshot(&app.settings.view())
            .launch_configuration_list_editor
            .expect("expected launch-configuration list editor");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(
            app.settings_presentation
                .is_launch_configuration_list_editor_open()
        );
        assert!(editor.input.is_none());
        assert_eq!(app.settings.launch_configuration, "");
    }

    #[tokio::test]
    async fn test_handle_enter_key_without_session_selection_keeps_list_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Sessions);
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_opens_cancel_confirmation_for_review_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.sessions_mut()[0].status = Status::Review;
        let expected_session_title = app.sessions.sessions()[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::CancelSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if mode_session_id == &expected_session_id
                && confirmation_title == "Confirm Cancel"
                && confirmation_message == &format!("Cancel session \"{expected_session_title}\"?")
        ));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_opens_cancel_confirmation_for_draft_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        let expected_session_title = app.sessions.sessions()[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::CancelSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if mode_session_id == &expected_session_id
                && confirmation_title == "Confirm Cancel"
                && confirmation_message == &format!("Cancel session \"{expected_session_title}\"?")
        ));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_opens_cancel_confirmation_for_running_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let expected_session_title = app.sessions.sessions()[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::CancelSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if mode_session_id == &expected_session_id
                && confirmation_title == "Confirm Cancel"
                && confirmation_message == &format!("Cancel session \"{expected_session_title}\"?")
        ));
    }

    #[test]
    fn cancel_confirmation_names_orchestration_child_count() {
        // Arrange / Act
        let orchestration_message = cancel_confirmation_message("Controller", 5);
        let regular_message = cancel_confirmation_message("Worker", 0);

        // Assert
        assert_eq!(
            orchestration_message,
            "Cancel orchestration and its 5 running children?"
        );
        assert_eq!(regular_message, "Cancel session \"Worker\"?");
    }

    #[tokio::test]
    async fn test_handle_cancel_key_ignores_non_review_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let _session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_question_mark_opens_help_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::List { ref keybindings },
                scroll_offset: 0,
            } if keybindings.iter().any(|action| action.key == "q")
                && keybindings.iter().any(|action| action.key == "?")
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_shows_failure_when_upstream_is_missing() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::Other("missing upstream".to_string())),
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
        assert!(matches!(
            app.project_sync_status.as_ref().map(|status| &status.phase),
            Some(ProjectSyncPhase::Running)
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(matches!(
            app.project_sync_status.as_ref().map(|status| &status.phase),
            Some(ProjectSyncPhase::Failed { message }) if message == "missing upstream"
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_is_case_insensitive() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::Other("missing upstream".to_string())),
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
        assert!(matches!(
            app.project_sync_status.as_ref().map(|status| &status.phase),
            Some(ProjectSyncPhase::Running)
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(matches!(
            app.project_sync_status.as_ref().map(|status| &status.phase),
            Some(ProjectSyncPhase::Failed { message }) if message == "missing upstream"
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_coalesces_duplicate_running_requests() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let mut mock_sync_main_runner = MockSyncMainRunner::new();
        mock_sync_main_runner
            .expect_start_sync_main()
            .times(1)
            .returning(|_, _, _, _| {});
        app.sync_main_runner = std::sync::Arc::new(mock_sync_main_runner);
        let sync_key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);

        // Act
        handle(&mut app, sync_key)
            .await
            .expect("failed to handle first sync key");
        let operation_id = app
            .project_sync_status
            .as_ref()
            .expect("sync should be running")
            .context
            .operation_id;
        handle(&mut app, sync_key)
            .await
            .expect("failed to handle duplicate sync key");

        // Assert
        assert_eq!(
            app.project_sync_status
                .as_ref()
                .expect("sync should remain running")
                .context
                .operation_id,
            operation_id
        );
    }

    #[tokio::test]
    async fn test_handle_sync_key_uses_captured_project_name_and_branch() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: "develop".to_string(),
            }),
        );
        let expected_project_name = base_dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("expected temp dir file name")
            .to_string();
        app.projects.update_active_project_context(
            app.active_project_id(),
            app.projects.project_name().to_string(),
            Some("develop".to_string()),
            None,
            base_dir.path().to_path_buf(),
        );
        let mut sync_context = app.sync_handle.context_snapshot();
        sync_context.project_branch_name = Some("develop".to_string());
        sync_context.project_name.clone_from(&expected_project_name);
        app.sync_handle.publish_context(sync_context);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));

        // Act
        app.process_pending_app_events().await;

        // Assert
        let status = app
            .project_sync_status
            .as_ref()
            .expect("sync status should remain visible");
        assert_eq!(status.context.default_branch, "develop");
        assert_eq!(status.context.project_name, expected_project_name);
        assert!(matches!(
            &status.phase,
            ProjectSyncPhase::Blocked { message }
                if message.contains("cannot run while `develop` has uncommitted changes")
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_shows_blocked_status_for_uncommitted_main() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: "main".to_string(),
            }),
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(matches!(
            app.project_sync_status.as_ref().map(|status| &status.phase),
            Some(ProjectSyncPhase::Blocked { message })
                if message.contains("cannot run while `main` has uncommitted changes")
        ));
    }
}
