use std::io;

use crossterm::event::KeyEvent;

use crate::app::App;
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, TuiTerminal, mode};
use crate::ui::state::app_mode::AppMode;

/// Routes key events to the active mode handler and returns the next runtime
/// action.
pub(crate) async fn handle_key_event(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
) -> io::Result<EventResult> {
    if let AppMode::Confirmation {
        selected_confirmation_index,
        ..
    } = &mut app.mode
    {
        let decision = mode::confirmation::handle(selected_confirmation_index, key);

        return handle_confirmation_decision(app, decision).await;
    }

    match &app.mode {
        AppMode::List => mode::list::handle(app, key).await,
        AppMode::SyncBlockedPopup { .. } => Ok(mode::sync_blocked::handle(app, key)),
        AppMode::Confirmation { .. } => {
            unreachable!("confirmation mode is handled before dispatch matching")
        }
        AppMode::View { .. } => mode::session_view::handle(app, terminal, key).await,
        AppMode::Prompt { .. } => mode::prompt::handle(app, terminal, key).await,
        AppMode::Diff { .. } => Ok(mode::diff::handle(app, key)),
        AppMode::Help { .. } => Ok(mode::help::handle(app, key)),
    }
}

/// Applies the semantic result of a generic confirmation interaction.
async fn handle_confirmation_decision(
    app: &mut App,
    decision: ConfirmationDecision,
) -> io::Result<EventResult> {
    match decision {
        ConfirmationDecision::Confirm => handle_confirmation_confirm(app).await,
        ConfirmationDecision::Cancel => {
            app.mode = AppMode::List;

            Ok(EventResult::Continue)
        }
        ConfirmationDecision::Continue => Ok(EventResult::Continue),
    }
}

/// Resolves a positive confirmation by deleting the selected session when a
/// confirmation session context exists, or quitting otherwise.
async fn handle_confirmation_confirm(app: &mut App) -> io::Result<EventResult> {
    let confirmation_session_id = match &app.mode {
        AppMode::Confirmation {
            session_id: Some(session_id),
            ..
        } => Some(session_id.clone()),
        _ => None,
    };
    app.mode = AppMode::List;

    if let Some(session_id) = confirmation_session_id {
        if let Some(session_index) = app.session_index_for_id(&session_id) {
            app.sessions.table_state.select(Some(session_index));
            app.delete_selected_session().await;
        }

        return Ok(EventResult::Continue);
    }

    Ok(EventResult::Quit)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

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

    /// Returns a mock app-server client wrapped in `Arc` for test injection.
    fn mock_app_server() -> std::sync::Arc<dyn crate::infra::app_server::AppServerClient> {
        std::sync::Arc::new(crate::infra::app_server::MockAppServerClient::new())
    }

    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
        )
        .await;

        (app, base_dir)
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
            mock_app_server(),
        )
        .await;

        (app, base_dir)
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_quits_when_no_session_context() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Confirmation {
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Quit)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_deletes_session_when_context_exists() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_message: "Delete session \"test\"?".to_string(),
            confirmation_title: "Confirm Delete".to_string(),
            session_id: Some(session_id),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
        assert!(app.sessions.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_returns_to_list() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Confirmation {
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }
}
