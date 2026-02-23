use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Tab};
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;
use crate::ui::state::app_mode::{AppMode, HelpContext};
use crate::ui::state::help_action::{
    HelpAction, onboarding_actions, session_list_actions, settings_actions, stats_actions,
};
use crate::ui::state::palette::PaletteFocus;
use crate::ui::state::prompt::{PromptHistoryState, PromptSlashState};

/// Handles key input while the app is in list mode.
///
/// Pressing `q` opens a confirmation overlay instead of quitting immediately,
/// with `No` selected by default.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    if app.tabs.current() == Tab::Settings && app.settings.is_editing_text_input() {
        return handle_settings_text_input(app, key).await;
    }

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::Confirmation {
                confirmation_message: "Quit agentty?".to_string(),
                confirmation_title: "Confirm Quit".to_string(),
                session_id: None,
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            };

            return Ok(EventResult::Continue);
        }
        KeyCode::Tab => {
            app.tabs.next();
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::CommandPalette {
                input: String::new(),
                selected_index: 0,
                focus: PaletteFocus::Dropdown,
            };
        }
        KeyCode::Char('a') => {
            open_new_session_prompt(app).await?;
        }
        KeyCode::Char('j') | KeyCode::Down => match app.tabs.current() {
            Tab::Sessions => app.next(),
            Tab::Stats => {}
            Tab::Settings => app.settings.next(),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.tabs.current() {
            Tab::Sessions => app.previous(),
            Tab::Stats => {}
            Tab::Settings => app.settings.previous(),
        },
        KeyCode::Enter => {
            if app.should_show_onboarding() {
                open_new_session_prompt(app).await?;

                return Ok(EventResult::Continue);
            }

            match app.tabs.current() {
                Tab::Sessions => {
                    if let Some(session_index) = app.sessions.table_state.selected()
                        && let Some(session) = app.sessions.sessions.get(session_index)
                        && !matches!(session.status, Status::Canceled)
                        && let Some(session_id) = app.session_id_for_index(session_index)
                    {
                        app.mode = AppMode::View {
                            session_id,
                            scroll_offset: None,
                        };
                    }
                }
                Tab::Settings => {
                    app.settings.handle_enter(&app.services).await;
                }
                Tab::Stats => {}
            }
        }
        KeyCode::Char('d') if app.tabs.current() == Tab::Sessions => {
            let selected_session = app
                .selected_session()
                .map(|session| (session.id.clone(), session.display_title().to_string()));
            if let Some((session_id, session_title)) = selected_session {
                app.mode = AppMode::Confirmation {
                    confirmation_message: format!("Delete session \"{session_title}\"?"),
                    confirmation_title: "Confirm Delete".to_string(),
                    session_id: Some(session_id),
                    selected_confirmation_index: DEFAULT_OPTION_INDEX,
                };
            }
        }
        KeyCode::Char('c') if app.tabs.current() == Tab::Sessions => {
            if let Some(session_id) = app.selected_session().map(|s| s.id.clone()) {
                let _ = app.cancel_session(&session_id).await;
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

/// Handles text input while a settings editor is active.
async fn handle_settings_text_input(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Enter => {
            app.settings.handle_enter(&app.services).await;
        }
        KeyCode::Esc => {
            app.settings.stop_text_input_editing();
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
    if app.should_show_onboarding() {
        return onboarding_actions();
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
            .is_some_and(|session| !matches!(session.status, Status::Canceled));

    session_list_actions(
        can_cancel_selected_session,
        can_delete_selected_session,
        can_open_selected_session,
    )
}

/// Creates a new session and switches the UI to prompt mode for it.
async fn open_new_session_prompt(app: &mut App) -> io::Result<()> {
    let session_id = app.create_session().await.map_err(io::Error::other)?;

    app.mode = AppMode::Prompt {
        at_mention_state: None,
        history_state: PromptHistoryState::new(Vec::new()),
        slash_state: PromptSlashState::new(),
        session_id,
        input: InputState::new(),
        scroll_offset: None,
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;
    use tokio::time::sleep;

    use super::*;
    use crate::db::Database;

    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(base_path.clone(), base_path, None, database).await;

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
            base_path.clone(),
            base_path,
            Some("main".to_string()),
            database,
        )
        .await;

        (app, base_dir)
    }

    async fn new_test_app_for_settings() -> (App, tempfile::TempDir) {
        let (mut app, base_dir) = new_test_app_with_git().await;
        app.create_session()
            .await
            .expect("failed to create session for settings tests");
        app.tabs.set(Tab::Settings);
        app.settings.next();

        (app, base_dir)
    }

    /// Waits until sync popup transitions from loading to final state.
    async fn wait_for_sync_popup_result(app: &mut App) {
        let mut sync_finished = false;
        for _ in 0..200 {
            app.process_pending_app_events().await;

            if matches!(
                app.mode,
                AppMode::SyncBlockedPopup {
                    is_loading: false,
                    ..
                }
            ) {
                sync_finished = true;
                break;
            }

            sleep(Duration::from_millis(10)).await;
        }

        assert!(sync_finished, "timed out waiting for sync popup result");
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
                ref confirmation_message,
                ref confirmation_title,
                session_id: None,
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if confirmation_title == "Confirm Quit" && confirmation_message == "Quit agentty?"
        ));
    }

    #[tokio::test]
    async fn test_handle_slash_key_opens_command_palette_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::CommandPalette {
                ref input,
                selected_index: 0,
                focus: PaletteFocus::Dropdown
            } if input.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_add_key_creates_session_and_opens_prompt_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;

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
    async fn test_handle_enter_key_opens_selected_session_in_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
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
                scroll_offset: None
            } if session_id == &expected_session_id
        ));
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
                scroll_offset: None
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_does_not_open_canceled_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let _session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Some(session) = app.sessions.sessions.first_mut() {
            session.status = Status::Canceled;
        }
        app.sessions.table_state.select(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_enter_key_starts_dev_server_editing_in_settings_tab() {
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
    async fn test_handle_char_key_appends_dev_server_value_while_editing() {
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
        assert_eq!(app.settings.dev_server, "a");
        assert_eq!(app.sessions.sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_handle_backspace_key_removes_dev_server_character_while_editing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.dev_server = "abc".to_string();
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
        assert_eq!(app.settings.dev_server, "ab");
    }

    #[tokio::test]
    async fn test_handle_enter_key_stops_dev_server_editing_when_already_editing() {
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
    async fn test_handle_enter_key_starts_session_from_onboarding() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.sessions.sessions.len(), 1);
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
    async fn test_handle_delete_key_opens_delete_confirmation() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let expected_session_title = app.sessions.sessions[0].display_title().to_string();
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
                ref confirmation_message,
                ref confirmation_title,
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
        let (mut app, base_dir) = new_test_app_with_git().await;
        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(base_dir.path())
            .output()
            .expect("failed to switch branch");

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
        wait_for_sync_popup_result(&mut app).await;

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
        let (mut app, base_dir) = new_test_app_with_git().await;
        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(base_dir.path())
            .output()
            .expect("failed to switch branch");

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
        wait_for_sync_popup_result(&mut app).await;

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
        let expected_project_name = base_dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("expected temp dir file name")
            .to_string();
        app.projects.replace_context(
            app.active_project_id(),
            Some("develop".to_string()),
            base_dir.path().to_path_buf(),
        );
        std::fs::write(base_dir.path().join("README.md"), "dirty develop")
            .expect("failed to write");

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
        wait_for_sync_popup_result(&mut app).await;

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
        let (mut app, base_dir) = new_test_app_with_git().await;
        std::fs::write(base_dir.path().join("README.md"), "dirty main").expect("failed to write");

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
        wait_for_sync_popup_result(&mut app).await;

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
