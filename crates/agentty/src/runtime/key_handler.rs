use std::io;
use std::sync::atomic::AtomicBool;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Tab};
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, TuiTerminal, mode};
use crate::ui::state::app_mode::{AppMode, ConfirmationIntent};

/// Routes key events to the active mode handler and returns the next runtime
/// action.
pub(crate) async fn handle_key_event(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
    event_reader_pause: &AtomicBool,
) -> io::Result<EventResult> {
    if let AppMode::Confirmation {
        selected_confirmation_index,
        ..
    } = &mut app.mode
    {
        let decision = mode::confirmation::handle(selected_confirmation_index, key);

        return handle_confirmation_decision(app, decision).await;
    }

    if let Some(event_result) =
        handle_list_external_editor_key(app, terminal, key, event_reader_pause).await
    {
        return Ok(event_result);
    }

    match &app.mode {
        AppMode::List => mode::list::handle(app, key).await,
        AppMode::SyncBlockedPopup { .. } => Ok(mode::sync_blocked::handle(app, key)),
        AppMode::Confirmation { .. } => {
            unreachable!("confirmation mode is handled before dispatch matching")
        }
        AppMode::View { .. } => {
            mode::session_view::handle(app, terminal, key, event_reader_pause).await
        }
        AppMode::Prompt { .. } => mode::prompt::handle(app, terminal, key).await,
        AppMode::Question { .. } => Ok(mode::question::handle(app, key).await),
        AppMode::Diff { .. } => Ok(mode::diff::handle(app, key)),
        AppMode::Help { .. } => Ok(mode::help::handle(app, key)),
    }
}

/// Handles list-mode external open shortcuts for the sessions tab.
///
/// The action is available only on the sessions tab when a session row is
/// selected. Lowercase `e` opens `nvim` in the active project root.
async fn handle_list_external_editor_key(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
    event_reader_pause: &AtomicBool,
) -> Option<EventResult> {
    if !matches!(app.mode, AppMode::List) {
        return None;
    }

    if !matches!(key.code, KeyCode::Char('e')) || app.tabs.current() != Tab::Sessions {
        return None;
    }

    let selected_session_id = app
        .sessions
        .table_state
        .selected()
        .and_then(|selected_index| app.session_id_for_index(selected_index));
    if selected_session_id.is_none() {
        return Some(EventResult::Continue);
    }

    let project_root = app.projects.working_dir().to_path_buf();
    let _ = crate::runtime::terminal::open_nvim(terminal, event_reader_pause, &project_root).await;

    Some(EventResult::Continue)
}

/// Applies the semantic result of a generic confirmation interaction.
async fn handle_confirmation_decision(
    app: &mut App,
    decision: ConfirmationDecision,
) -> io::Result<EventResult> {
    match decision {
        ConfirmationDecision::Confirm => handle_confirmation_confirm(app).await,
        ConfirmationDecision::Cancel => {
            app.mode = confirmation_cancel_mode(&app.mode);

            Ok(EventResult::Continue)
        }
        ConfirmationDecision::Continue => Ok(EventResult::Continue),
    }
}

/// Resolves target mode for `Cancel` in confirmation overlays.
fn confirmation_cancel_mode(mode: &AppMode) -> AppMode {
    if let AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeSession,
        restore_view: Some(restore_view),
        ..
    } = mode
    {
        return restore_view.clone().into_view_mode();
    }

    AppMode::List
}

/// Resolves a positive confirmation by dispatching the configured action
/// intent.
async fn handle_confirmation_confirm(app: &mut App) -> io::Result<EventResult> {
    let (confirmation_intent, confirmation_session_id, restore_view) = match &app.mode {
        AppMode::Confirmation {
            confirmation_intent,
            restore_view,
            session_id,
            ..
        } => (
            *confirmation_intent,
            session_id.clone(),
            restore_view.clone(),
        ),
        _ => return Ok(EventResult::Continue),
    };

    match confirmation_intent {
        ConfirmationIntent::Quit => {
            app.mode = AppMode::List;

            Ok(EventResult::Quit)
        }
        ConfirmationIntent::DeleteSession => {
            handle_delete_confirmation(app, confirmation_session_id).await
        }
        ConfirmationIntent::MergeSession => {
            handle_merge_confirmation(app, confirmation_session_id, restore_view).await
        }
    }
}

/// Deletes the confirmed session, when still present, and returns to list
/// mode.
async fn handle_delete_confirmation(
    app: &mut App,
    confirmation_session_id: Option<String>,
) -> io::Result<EventResult> {
    app.mode = AppMode::List;

    if let Some(session_id) = confirmation_session_id
        && let Some(session_index) = app.session_index_for_id(&session_id)
    {
        app.sessions.table_state.select(Some(session_index));
        app.delete_selected_session().await;
    }

    Ok(EventResult::Continue)
}

/// Restores view mode and attempts to add confirmed session to merge queue.
async fn handle_merge_confirmation(
    app: &mut App,
    confirmation_session_id: Option<String>,
    restore_view: Option<crate::ui::state::app_mode::ConfirmationViewMode>,
) -> io::Result<EventResult> {
    app.mode = restore_view.map_or(
        AppMode::List,
        crate::ui::state::app_mode::ConfirmationViewMode::into_view_mode,
    );

    if let Some(session_id) = confirmation_session_id
        && let Err(error) = app.merge_session(&session_id).await
    {
        app.append_output_for_session(&session_id, &format!("\n[Merge Error] {error}\n"))
            .await;
    }

    Ok(EventResult::Continue)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::db::Database;
    use crate::ui::state::app_mode::{ConfirmationViewMode, DoneSessionOutputMode};

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
            confirmation_intent: ConfirmationIntent::Quit,
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            restore_view: None,
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
            confirmation_intent: ConfirmationIntent::DeleteSession,
            confirmation_message: "Delete session \"test\"?".to_string(),
            confirmation_title: "Confirm Delete".to_string(),
            restore_view: None,
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
            confirmation_intent: ConfirmationIntent::Quit,
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            restore_view: None,
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

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_merge_confirmation() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            confirmation_message: "Add this session to merge queue?".to_string(),
            confirmation_title: "Confirm Merge".to_string(),
            restore_view: Some(ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some("Preparing focused review".to_string()),
                focused_review_text: Some("Review output".to_string()),
                scroll_offset: Some(6),
                session_id: session_id.clone(),
            }),
            session_id: Some(session_id.clone()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some(ref focused_review_status_message),
                focused_review_text: Some(ref focused_review_text),
                session_id: ref session_id_in_mode,
                scroll_offset: Some(6),
            } if session_id_in_mode == &session_id
                && focused_review_status_message == "Preparing focused review"
                && focused_review_text == "Review output"
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_queues_merge_with_view_restore() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            confirmation_message: "Add this session to merge queue?".to_string(),
            confirmation_title: "Confirm Merge".to_string(),
            restore_view: Some(ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: Some(2),
                session_id: session_id.clone(),
            }),
            session_id: Some(session_id.clone()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                session_id: ref session_id_in_mode,
                scroll_offset: Some(2),
            } if session_id_in_mode == &session_id
        ));
        app.sessions.sync_from_handles();
        let output = app.sessions.sessions[0].output.clone();
        assert!(output.contains("[Merge Error]"));
    }
}
