use std::io;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::model::{
    AppMode, HelpContext, InputState, PaletteFocus, PromptHistoryState, PromptSlashState, Status,
};
use crate::runtime::EventResult;

/// Handles key input while the app is in list mode.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Char('q') => return Ok(EventResult::Quit),
        KeyCode::Tab => {
            app.next_tab();
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::CommandPalette {
                input: String::new(),
                selected_index: 0,
                focus: PaletteFocus::Dropdown,
            };
        }
        KeyCode::Char('a') => {
            let session_id = app.create_session().await.map_err(io::Error::other)?;

            app.mode = AppMode::Prompt {
                at_mention_state: None,
                history_state: PromptHistoryState::new(Vec::new()),
                slash_state: PromptSlashState::new(),
                session_id,
                input: InputState::new(),
                scroll_offset: None,
            };
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.next();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.previous();
        }
        KeyCode::Enter => {
            if app.should_show_onboarding() {
                let session_id = app.create_session().await.map_err(io::Error::other)?;

                app.mode = AppMode::Prompt {
                    at_mention_state: None,
                    history_state: PromptHistoryState::new(Vec::new()),
                    slash_state: PromptSlashState::new(),
                    session_id,
                    input: InputState::new(),
                    scroll_offset: None,
                };

                return Ok(EventResult::Continue);
            }

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
        KeyCode::Char('d') => {
            let selected_session = app
                .selected_session()
                .map(|session| (session.id.clone(), session.display_title().to_string()));
            if let Some((session_id, session_title)) = selected_session {
                app.mode = AppMode::ConfirmDeleteSession {
                    session_id,
                    session_title,
                };
            }
        }
        KeyCode::Char('?') => {
            app.mode = AppMode::Help {
                context: HelpContext::List,
                scroll_offset: 0,
            };
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

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

    #[tokio::test]
    async fn test_handle_quit_key_returns_quit_result() {
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
        assert!(matches!(event_result, EventResult::Quit));
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
            AppMode::ConfirmDeleteSession {
                ref session_id,
                ref session_title,
            } if session_id == &expected_session_id && session_title == &expected_session_title
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
                context: HelpContext::List,
                scroll_offset: 0,
            }
        ));
    }
}
