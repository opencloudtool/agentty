use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Tab};
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;
use crate::runtime::mode::question;
use crate::ui::state::app_mode::{
    AppMode, ConfirmationIntent, DoneSessionOutputMode, HelpContext, QuestionFocus,
};
use crate::ui::state::help_action::{
    HelpAction, project_list_actions, session_list_actions, settings_actions, stats_actions,
};
use crate::ui::state::prompt::{PromptAttachmentState, PromptHistoryState};

/// Handles key input while the app is in list mode.
///
/// Pressing `q` opens a confirmation overlay instead of quitting immediately,
/// with `No` selected by default. Pressing `Enter` on the `Projects` tab
/// selects the active project and then moves focus to `Tab::Sessions`.
/// `d` and `c` open delete/cancel confirmation overlays for the selected
/// session, and `Tab` cycles tabs forward while `Shift+Tab` cycles backward.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    if app.tabs.current() == Tab::Settings && app.settings.is_editing_text_input() {
        return handle_settings_text_input(app, key).await;
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
            app.tabs.next();
        }
        KeyCode::BackTab => {
            app.tabs.previous();
        }
        KeyCode::Char('a')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::NONE =>
        {
            open_new_session_prompt(app).await?;
        }
        KeyCode::Char('A')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::SHIFT =>
        {
            open_new_draft_session_prompt(app).await?;
        }
        KeyCode::Char('j') | KeyCode::Down => match app.tabs.current() {
            Tab::Projects => app.next_project(),
            Tab::Sessions => app.next(),
            Tab::Stats => {}
            Tab::Settings => app.settings.next(),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.tabs.current() {
            Tab::Projects => app.previous_project(),
            Tab::Sessions => app.previous(),
            Tab::Stats => {}
            Tab::Settings => app.settings.previous(),
        },
        KeyCode::Enter => return handle_enter_key(app).await,
        KeyCode::Char('d') if app.tabs.current() == Tab::Sessions => {
            let selected_session = app
                .selected_session()
                .map(|session| (session.id.clone(), session.display_title().to_string()));
            if let Some((session_id, session_title)) = selected_session {
                app.mode = AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::DeleteSession,
                    confirmation_message: format!("Delete session \"{session_title}\"?"),
                    confirmation_title: "Confirm Delete".to_string(),
                    restore_view: None,
                    session_id: Some(session_id),
                    selected_confirmation_index: DEFAULT_OPTION_INDEX,
                };
            }
        }
        KeyCode::Char('c') if app.tabs.current() == Tab::Sessions => {
            let selected_session = app.selected_session().and_then(|session| {
                (session.status == Status::Review)
                    .then(|| (session.id.clone(), session.display_title().to_string()))
            });
            if let Some((session_id, session_title)) = selected_session {
                app.mode = AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::CancelSession,
                    confirmation_message: format!("Cancel session \"{session_title}\"?"),
                    confirmation_title: "Confirm Cancel".to_string(),
                    restore_view: None,
                    session_id: Some(session_id),
                    selected_confirmation_index: DEFAULT_OPTION_INDEX,
                };
            }
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'s') => {
            sync_main_branch(app);
        }
        KeyCode::Char('?') => {
            open_list_help_overlay(app);
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Handles `Enter` in list mode and triggers the selected tab primary action.
///
/// On the sessions tab, any selected session can be opened in view mode.
async fn handle_enter_key(app: &mut App) -> io::Result<EventResult> {
    match app.tabs.current() {
        Tab::Projects => {
            if app.switch_selected_project().await.is_ok() {
                app.tabs.set(Tab::Sessions);
            }
        }
        Tab::Sessions => {
            if let Some(session_index) = app.sessions.table_state.selected()
                && let Some(session) = app.sessions.sessions.get(session_index)
            {
                let session_id = session.id.clone();

                if session.status == Status::Question {
                    let questions = session.questions.clone();
                    let selected_option_index = question::default_option_index(&questions, 0);
                    app.mode = AppMode::Question {
                        at_mention_state: None,
                        session_id,
                        questions,
                        responses: Vec::new(),
                        current_index: 0,
                        focus: QuestionFocus::Answer,
                        input: InputState::default(),
                        scroll_offset: None,
                        selected_option_index,
                    };
                } else {
                    app.mode = AppMode::View {
                        done_session_output_mode: DoneSessionOutputMode::Summary,
                        review_status_message: None,
                        review_text: None,
                        session_id,
                        scroll_offset: None,
                    };
                }
            }
        }
        Tab::Settings => {
            app.settings.handle_enter(&app.services).await;
        }
        Tab::Stats => {}
    }

    Ok(EventResult::Continue)
}

/// Handles text input while a settings editor is active.
///
/// The `Open Commands` editor is multiline: `Alt+Enter`/`Shift+Enter` insert
/// a newline. Terminals that emit `\r`/`\n` as character keys are also
/// treated as newline insertion to match prompt input behavior. Plain
/// `Enter` finishes editing.
/// Arrow keys move the cursor.
async fn handle_settings_text_input(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        _ if should_insert_settings_newline(app, key) => {
            app.settings
                .append_selected_text_character(&app.services, '\n')
                .await;
        }
        code if is_settings_enter_key(code) => {
            app.settings.stop_text_input_editing();
        }
        KeyCode::Esc => {
            app.settings.stop_text_input_editing();
        }
        KeyCode::Left => {
            app.settings.move_selected_text_cursor_left();
        }
        KeyCode::Right => {
            app.settings.move_selected_text_cursor_right();
        }
        KeyCode::Up => {
            app.settings.move_selected_text_cursor_up();
        }
        KeyCode::Down => {
            app.settings.move_selected_text_cursor_down();
        }
        KeyCode::Backspace => {
            app.settings
                .remove_selected_text_character(&app.services)
                .await;
        }
        KeyCode::Char(character) if is_settings_text_key(key) => {
            app.settings
                .append_selected_text_character(&app.services, character)
                .await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Returns whether settings text editing should insert a newline.
fn should_insert_settings_newline(app: &App, key: KeyEvent) -> bool {
    app.settings.is_editing_open_commands()
        && (is_settings_newline_character_key(key.code)
            || is_settings_modified_enter_key(key)
            || is_settings_control_newline_key(key))
}

/// Returns whether the key code is a newline character emitted as a typed key.
fn is_settings_newline_character_key(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Char('\r' | '\n'))
}

/// Returns whether the key event is a modified Enter key that should insert a
/// newline in settings text input.
fn is_settings_modified_enter_key(key: KeyEvent) -> bool {
    is_settings_enter_key(key.code)
        && key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
}

/// Returns whether the key event is a control-key newline variant.
fn is_settings_control_newline_key(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::CONTROL
        && matches!(key.code, KeyCode::Char('j' | 'm' | '\n' | '\r'))
}

/// Returns whether a key code is the Enter key.
fn is_settings_enter_key(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Enter)
}

/// Returns whether a key event should insert text into a settings string value.
fn is_settings_text_key(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT
}

/// Starts selected-project branch sync and immediately opens a loading popup.
fn sync_main_branch(app: &mut App) {
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

    if app.tabs.current() == Tab::Stats {
        return stats_actions();
    }

    let is_sessions_tab = app.tabs.current() == Tab::Sessions;
    let selected_session = app.selected_session();
    let can_delete_selected_session = is_sessions_tab && selected_session.is_some();
    let can_cancel_selected_session =
        is_sessions_tab && selected_session.is_some_and(|session| session.status == Status::Review);
    let can_open_selected_session = is_sessions_tab
        && app
            .sessions
            .table_state
            .selected()
            .and_then(|selected_index| app.sessions.sessions.get(selected_index))
            .is_some();
    session_list_actions(
        can_cancel_selected_session,
        can_delete_selected_session,
        can_open_selected_session,
    )
}

/// Creates a new session and opens prompt mode.
async fn open_new_session_prompt(app: &mut App) -> io::Result<()> {
    let session_id = app.create_session().await.map_err(io::Error::other)?;
    open_session_prompt(app, session_id);

    Ok(())
}

/// Creates a new draft session and opens prompt mode.
async fn open_new_draft_session_prompt(app: &mut App) -> io::Result<()> {
    let session_id = app.create_draft_session().await.map_err(io::Error::other)?;
    open_session_prompt(app, session_id);

    Ok(())
}

/// Opens prompt mode for the provided session identifier.
fn open_session_prompt(app: &mut App, session_id: String) {
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::new(Vec::new()),
        slash_state: app.prompt_slash_state(),
        session_id,
        input: InputState::new(),
        scroll_offset: None,
    };
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::app::{AppEvent, MockSyncMainRunner, SyncMainOutcome, SyncSessionStartError};
    use crate::db::Database;
    use crate::infra::agent::protocol::QuestionItem;

    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(true, base_path.clone(), base_path, None, database)
            .await
            .expect("failed to build app");

        (app, base_dir)
    }

    fn setup_test_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        std::fs::write(path.join("README.md"), "test").expect("write failed");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("git commit failed");
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(path)
            .output()
            .expect("git branch failed");
    }

    async fn new_test_app_with_git() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        setup_test_git_repo(base_dir.path());
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            true,
            base_path.clone(),
            base_path,
            Some("main".to_string()),
            database,
        )
        .await
        .expect("failed to build app");

        (app, base_dir)
    }

    /// Builds a settings-focused test app with the `Open Commands` row
    /// selected.
    async fn new_test_app_for_settings() -> (App, tempfile::TempDir) {
        let (mut app, base_dir) = new_test_app_with_git().await;
        app.create_session()
            .await
            .expect("failed to create session for settings tests");
        app.tabs.set(Tab::Settings);
        let open_command_row_index = app
            .settings
            .settings_rows()
            .iter()
            .position(|(setting_name, _)| *setting_name == "Open Commands")
            .expect("missing Open Commands setting row");
        app.settings
            .table_state
            .select(Some(open_command_row_index));

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
            .returning(move |app_event_tx, _, _, _, _| {
                let _ = app_event_tx.send(AppEvent::SyncMainCompleted {
                    result: result.clone(),
                });
            });
        app.sync_main_runner = std::sync::Arc::new(mock_sync_main_runner);
    }

    #[tokio::test]
    async fn test_handle_quit_key_shows_confirm_quit_overlay() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;

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
        let (mut app, _base_dir) = new_test_app().await;
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
    async fn test_handle_add_key_creates_session_and_opens_prompt_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
        assert_eq!(app.sessions.sessions.len(), 1);
        assert!(!app.sessions.sessions[0].is_draft_session());
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref session_id,
                scroll_offset: None,
                ..
            } if !session_id.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_add_key_ignored_on_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
        assert!(app.sessions.sessions.is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_shift_add_key_creates_draft_session_and_opens_prompt_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        app.tabs.set(Tab::Sessions);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.sessions.sessions.len(), 1);
        assert!(app.sessions.sessions[0].is_draft_session());
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref session_id,
                scroll_offset: None,
                ..
            } if !session_id.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_add_key_ignored_on_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
        assert!(app.sessions.sessions.is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_selected_session_in_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));
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
    async fn test_handle_enter_key_opens_selected_question_session_in_question_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
        if let Some(session) = app.sessions.sessions.first_mut() {
            session.status = Status::Question;
            session.questions = expected_questions.clone();
        }
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));
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
    async fn test_handle_enter_key_keeps_persisted_size_until_turn_completion() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.services
            .db()
            .update_session_diff_stats(0, 0, &expected_session_id, "XS")
            .await
            .expect("failed to set stale size");
        let session_index = app
            .session_index_for_id(&expected_session_id)
            .expect("missing created session");
        let session_folder = app.sessions.sessions[session_index].folder.clone();
        let changed_lines = "line\n".repeat(40);
        std::fs::write(session_folder.join("open-size-test.txt"), changed_lines)
            .expect("failed to write test file");
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");
        let db_sessions = app
            .services
            .db()
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
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Some(session) = app.sessions.sessions.first_mut() {
            session.status = Status::Done;
        }
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));
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
                done_session_output_mode: DoneSessionOutputMode::Summary,
                review_status_message: None,
                review_text: None,
                ref session_id,
                scroll_offset: None,
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_canceled_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Some(session) = app.sessions.sessions.first_mut() {
            session.status = Status::Canceled;
        }
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));
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
                done_session_output_mode: DoneSessionOutputMode::Summary,
                review_status_message: None,
                review_text: None,
                ref session_id,
                scroll_offset: None,
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_switches_to_sessions_tab_from_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
    async fn test_handle_enter_key_starts_open_command_editing_in_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.settings.is_editing_text_input());
    }

    #[tokio::test]
    async fn test_handle_char_key_appends_open_command_value_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.open_command, "a");
        assert_eq!(app.sessions.sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_handle_backspace_key_removes_open_command_character_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "abc".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.open_command, "ab");
    }

    #[tokio::test]
    async fn test_handle_shift_enter_key_inserts_open_command_newline_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "cargo test".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
            .await
            .expect("failed to handle Shift+Enter key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.settings.is_editing_text_input());
        assert_eq!(app.settings.open_command, "cargo test\n");
    }

    #[tokio::test]
    async fn test_handle_alt_enter_key_inserts_open_command_newline_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "cargo test".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))
            .await
            .expect("failed to handle Alt+Enter key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.settings.is_editing_text_input());
        assert_eq!(app.settings.open_command, "cargo test\n");
    }

    #[tokio::test]
    async fn test_handle_shift_carriage_return_key_inserts_open_command_newline_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "cargo test".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to handle Shift+carriage-return key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.settings.is_editing_text_input());
        assert_eq!(app.settings.open_command, "cargo test\n");
    }

    #[tokio::test]
    async fn test_handle_control_j_key_inserts_open_command_newline_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "cargo test".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        )
        .await
        .expect("failed to handle Control+j key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.settings.is_editing_text_input());
        assert_eq!(app.settings.open_command, "cargo test\n");
    }

    #[tokio::test]
    async fn test_handle_line_feed_key_inserts_open_command_newline_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "cargo test".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle line-feed key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.settings.is_editing_text_input());
        assert_eq!(app.settings.open_command, "cargo test\n");
    }

    #[tokio::test]
    async fn test_handle_enter_key_stops_open_command_editing_when_already_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(!app.settings.is_editing_text_input());
    }

    #[tokio::test]
    async fn test_handle_esc_key_stops_open_command_editing_when_already_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle Esc key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(!app.settings.is_editing_text_input());
    }

    #[tokio::test]
    async fn test_handle_left_key_moves_open_command_cursor_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "ac".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .await
            .expect("failed to handle left key");
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to insert character after cursor move");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.open_command, "abc");
    }

    #[tokio::test]
    async fn test_handle_up_key_moves_open_command_cursor_to_previous_line() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.open_command = "ab\nxy".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to start settings editing");

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .await
            .expect("failed to handle up key");
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to insert character after up move");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.open_command, "abZ\nxy");
    }

    #[tokio::test]
    async fn test_handle_enter_key_without_session_selection_keeps_list_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        app.tabs.set(Tab::Sessions);
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions.is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }
    #[tokio::test]
    async fn test_handle_delete_key_opens_delete_confirmation() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let expected_session_title = app.sessions.sessions[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.sessions.sessions.len(), 1);
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::DeleteSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if mode_session_id == &expected_session_id
                && confirmation_title == "Confirm Delete"
                && confirmation_message == &format!("Delete session \"{expected_session_title}\"?")
        ));
    }

    #[tokio::test]
    async fn test_handle_delete_key_without_selection_does_nothing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let _session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.table_state.select(None);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.sessions.sessions.len(), 1);
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_opens_cancel_confirmation_for_review_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.sessions[0].status = Status::Review;
        let expected_session_title = app.sessions.sessions[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));

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
    async fn test_handle_cancel_key_ignores_non_review_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let _session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.tabs.set(Tab::Sessions);
        app.sessions.table_state.select(Some(0));

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
        let (mut app, _base_dir) = new_test_app().await;

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
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: false,
                ref title,
                ..
            } if title == "Sync failed"
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_is_case_insensitive() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: false,
                ref title,
                ..
            } if title == "Sync failed"
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_uses_project_name_and_branch_in_popup_message() {
        // Arrange
        let (mut app, base_dir) = new_test_app_with_git().await;
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

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                ref default_branch,
                is_loading: false,
                ref title,
                ref message,
                ref project_name,
            } if title == "Sync blocked"
                && default_branch.as_deref() == Some("develop")
                && message.contains("cannot run while `develop` has uncommitted changes")
                && project_name.as_deref() == Some(expected_project_name.as_str())
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_opens_popup_when_main_has_uncommitted_changes() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
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
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                ref default_branch,
                is_loading: false,
                ref title,
                ref message,
                ref project_name,
            } if title == "Sync blocked" && message.contains("cannot run while `main` has uncommitted changes")
                && default_branch.as_deref() == Some("main")
                && project_name.is_some()
        ));
    }
}
