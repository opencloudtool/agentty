use std::io;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::model::AppMode;
use crate::runtime::EventResult;

/// Handles key input while delete confirmation is visible.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    let session_id = match &app.mode {
        AppMode::ConfirmDeleteSession { session_id, .. } => session_id.clone(),
        _ => return Ok(EventResult::Continue),
    };

    match key.code {
        KeyCode::Enter | KeyCode::Char('y') => {
            if let Some(session_index) = app.session_index_for_id(&session_id) {
                app.sessions.table_state.select(Some(session_index));
                app.mode = AppMode::List;
                app.delete_selected_session().await;
            } else {
                app.mode = AppMode::List;
            }
        }
        KeyCode::Esc | KeyCode::Char('n' | 'q') => {
            app.mode = AppMode::List;
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
    async fn test_handle_enter_deletes_confirmed_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_title = app.sessions.sessions[0].display_title().to_string();
        app.mode = AppMode::ConfirmDeleteSession {
            session_id: session_id.clone(),
            session_title,
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
        assert!(app.sessions.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_handle_n_cancels_delete_confirmation() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_title = app.sessions.sessions[0].display_title().to_string();
        app.mode = AppMode::ConfirmDeleteSession {
            session_id,
            session_title,
        };

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
        assert_eq!(app.sessions.sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_handle_esc_cancels_delete_confirmation() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_title = app.sessions.sessions[0].display_title().to_string();
        app.mode = AppMode::ConfirmDeleteSession {
            session_id,
            session_title,
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
        assert_eq!(app.sessions.sessions.len(), 1);
    }
}
