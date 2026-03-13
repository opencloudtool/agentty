use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::{App, FocusedReviewCacheEntry, diff_content_hash};
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, TuiTerminal, mode};
use crate::ui::state::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, DoneSessionOutputMode,
};

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

    if matches!(app.mode, AppMode::OpenCommandSelector { .. }) {
        return handle_open_command_selector_key(app, key).await;
    }

    if matches!(app.mode, AppMode::PublishBranchInput { .. }) {
        return Ok(handle_publish_branch_input_key(app, key));
    }

    match &app.mode {
        AppMode::List => mode::list::handle(app, key).await,
        AppMode::SyncBlockedPopup { .. } => Ok(mode::sync_blocked::handle(app, key)),
        AppMode::ViewInfoPopup { .. } => Ok(handle_view_info_popup_key(app, key)),
        AppMode::Confirmation { .. } => {
            unreachable!("confirmation mode is handled before dispatch matching")
        }
        AppMode::View { .. } => mode::session_view::handle(app, terminal, key).await,
        AppMode::Prompt { .. } => mode::prompt::handle(app, terminal, key).await,
        AppMode::Question { .. } => {
            let size = terminal.size()?;
            let terminal_rect = Rect::new(0, 0, size.width, size.height);

            Ok(mode::question::handle(app, terminal_rect, key).await)
        }
        AppMode::Diff { .. } => Ok(mode::diff::handle(app, key)),
        AppMode::Help { .. } => Ok(mode::help::handle(app, key)),
        AppMode::OpenCommandSelector { .. } => {
            unreachable!("open-command selector mode is handled before dispatch matching")
        }
        AppMode::PublishBranchInput { .. } => {
            unreachable!("publish-branch input mode is handled before dispatch matching")
        }
    }
}

/// Handles key input while a session-scoped informational popup is visible.
fn handle_view_info_popup_key(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::ViewInfoPopup {
        is_loading,
        restore_view,
        ..
    } = &app.mode
    else {
        return EventResult::Continue;
    };

    if *is_loading {
        return EventResult::Continue;
    }

    match key.code {
        KeyCode::Enter | KeyCode::Esc => {
            app.mode = restore_view.clone().into_view_mode();
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = restore_view.clone().into_view_mode();
        }
        _ => {}
    }

    EventResult::Continue
}

/// Handles key input while the publish-branch input overlay is visible.
fn handle_publish_branch_input_key(app: &mut App, key: KeyEvent) -> EventResult {
    let publish_branch_input =
        PublishBranchInputModeState::from_mode(std::mem::replace(&mut app.mode, AppMode::List));
    let input_locked = publish_branch_input.locked_upstream_ref.is_some();

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = publish_branch_input.restore_view.into_view_mode();
        }
        KeyCode::Enter => {
            let remote_branch_name = if input_locked {
                Some(publish_branch_input.input.text().trim().to_string())
            } else {
                (!publish_branch_input.input.text().trim().is_empty())
                    .then(|| publish_branch_input.input.text().trim().to_string())
            };
            let session_id = publish_branch_input.restore_view.session_id.clone();

            app.start_publish_branch_action(
                publish_branch_input.restore_view,
                &session_id,
                publish_branch_input.publish_branch_action,
                remote_branch_name,
            );
        }
        KeyCode::Left if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_left);
        }
        KeyCode::Right if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_right);
        }
        KeyCode::Up if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_up);
        }
        KeyCode::Down if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_down);
        }
        KeyCode::Home if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_home);
        }
        KeyCode::End if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_end);
        }
        KeyCode::Backspace if !input_locked => {
            app.mode = publish_branch_input
                .apply_input_edit(crate::domain::input::InputState::delete_backward);
        }
        KeyCode::Delete if !input_locked => {
            app.mode = publish_branch_input
                .apply_input_edit(crate::domain::input::InputState::delete_forward);
        }
        KeyCode::Char(character) if !input_locked && is_publish_branch_input_text_key(key) => {
            app.mode = publish_branch_input.apply_input_edit(|input| input.insert_char(character));
        }
        _ => {
            app.mode = publish_branch_input.into_mode();
        }
    }

    EventResult::Continue
}

/// Returns whether one key event should insert text into the publish-branch
/// input field.
fn is_publish_branch_input_text_key(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT
}

/// Captures `AppMode::PublishBranchInput` fields so key handlers can rebuild
/// the overlay consistently after input edits.
struct PublishBranchInputModeState {
    default_branch_name: String,
    input: crate::domain::input::InputState,
    locked_upstream_ref: Option<String>,
    publish_branch_action: crate::domain::session::PublishBranchAction,
    restore_view: ConfirmationViewMode,
}

impl PublishBranchInputModeState {
    /// Extracts publish-branch overlay fields from an app mode value.
    fn from_mode(mode: AppMode) -> Self {
        let AppMode::PublishBranchInput {
            default_branch_name,
            input,
            locked_upstream_ref,
            publish_branch_action,
            restore_view,
        } = mode
        else {
            unreachable!("mode must be publish-branch input in this handler");
        };

        Self {
            default_branch_name,
            input,
            locked_upstream_ref,
            publish_branch_action,
            restore_view,
        }
    }

    /// Applies one input edit and rebuilds the publish-branch overlay mode.
    fn apply_input_edit(
        mut self,
        edit: impl FnOnce(&mut crate::domain::input::InputState),
    ) -> AppMode {
        edit(&mut self.input);

        self.into_mode()
    }

    /// Rebuilds `AppMode::PublishBranchInput` from the stored overlay fields.
    fn into_mode(self) -> AppMode {
        AppMode::PublishBranchInput {
            default_branch_name: self.default_branch_name,
            input: self.input,
            locked_upstream_ref: self.locked_upstream_ref,
            publish_branch_action: self.publish_branch_action,
            restore_view: self.restore_view,
        }
    }
}

/// Handles key input while the app is in open-command selector overlay mode.
async fn handle_open_command_selector_key(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::OpenCommandSelector {
        commands,
        restore_view,
        selected_command_index,
    } = mode
    else {
        unreachable!("mode must be open-command selector in this handler");
    };

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = restore_view.into_view_mode();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.mode = AppMode::OpenCommandSelector {
                selected_command_index: next_open_command_index(selected_command_index, &commands),
                commands,
                restore_view,
            };
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.mode = AppMode::OpenCommandSelector {
                selected_command_index: previous_open_command_index(
                    selected_command_index,
                    &commands,
                ),
                commands,
                restore_view,
            };
        }
        KeyCode::Enter => {
            let selected_open_command = commands
                .get(selected_command_index)
                .map(std::string::String::as_str);
            app.mode = restore_view.into_view_mode();
            app.open_session_worktree_in_tmux_with_command(selected_open_command)
                .await;
        }
        _ => {
            app.mode = AppMode::OpenCommandSelector {
                commands,
                restore_view,
                selected_command_index,
            };
        }
    }

    Ok(EventResult::Continue)
}

/// Returns the next command index with wrap-around.
fn next_open_command_index(current_index: usize, commands: &[String]) -> usize {
    if commands.is_empty() {
        return 0;
    }

    (current_index + 1) % commands.len()
}

/// Returns the previous command index with wrap-around.
fn previous_open_command_index(current_index: usize, commands: &[String]) -> usize {
    if commands.is_empty() {
        return 0;
    }

    if current_index == 0 {
        commands.len() - 1
    } else {
        current_index - 1
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
            app.mode = confirmation_cancel_mode(&app.mode);

            Ok(EventResult::Continue)
        }
        ConfirmationDecision::Continue => Ok(EventResult::Continue),
    }
}

/// Resolves target mode for `Cancel` in confirmation overlays.
fn confirmation_cancel_mode(mode: &AppMode) -> AppMode {
    if let AppMode::Confirmation {
        confirmation_intent:
            ConfirmationIntent::MergeSession | ConfirmationIntent::RegenerateFocusedReview,
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
        ConfirmationIntent::RegenerateFocusedReview => {
            handle_regenerate_focused_review_confirmation(
                app,
                confirmation_session_id,
                restore_view,
            )
            .await
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
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

    if let Some(session_id) = confirmation_session_id
        && let Err(error) = app.merge_session(&session_id).await
    {
        app.append_output_for_session(&session_id, &format!("\n[Merge Error] {error}\n"))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Clears the focused review cache and restarts generation for the confirmed
/// session, then restores focused review mode with loading state.
async fn handle_regenerate_focused_review_confirmation(
    app: &mut App,
    confirmation_session_id: Option<String>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = AppMode::List;

        return Ok(EventResult::Continue);
    };

    app.focused_review_cache.remove(&session_id);

    let session = app
        .sessions
        .sessions
        .iter()
        .find(|session| session.id == session_id);
    let Some(session) = session else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };

    let session_folder = session.folder.clone();
    let session_summary = session.summary.clone();
    let base_branch = session.base_branch.clone();

    let diff = app
        .services
        .git_client()
        .diff(session_folder.clone(), base_branch)
        .await
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"));

    if diff.trim().is_empty() || diff.starts_with("Failed to run git diff:") {
        let mut view_mode = restore_view.unwrap_or(ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::FocusedReview,
            focused_review_status_message: None,
            focused_review_text: None,
            scroll_offset: None,
            session_id: session_id.clone(),
        });
        view_mode.focused_review_status_message = None;
        view_mode.focused_review_text = if diff.trim().is_empty() {
            Some("No diff changes found for review.".to_string())
        } else {
            Some(diff)
        };
        app.mode = view_mode.into_view_mode();

        return Ok(EventResult::Continue);
    }

    let diff_hash = diff_content_hash(&diff);
    let review_model = app.settings.default_review_model;
    app.focused_review_cache.insert(
        session_id.clone(),
        FocusedReviewCacheEntry::Loading { diff_hash },
    );
    app.start_focused_review_assist(
        &session_id,
        &session_folder,
        diff_hash,
        &diff,
        session_summary.as_deref(),
    );

    let mut view_mode = restore_view.unwrap_or(ConfirmationViewMode {
        done_session_output_mode: DoneSessionOutputMode::FocusedReview,
        focused_review_status_message: None,
        focused_review_text: None,
        scroll_offset: None,
        session_id,
    });
    view_mode.done_session_output_mode = DoneSessionOutputMode::FocusedReview;
    view_mode.focused_review_status_message =
        Some(crate::runtime::mode::session_view::focused_review_loading_message(review_model));
    view_mode.focused_review_text = None;
    app.mode = view_mode.into_view_mode();

    Ok(EventResult::Continue)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use mockall::predicate::eq;
    use tempfile::tempdir;

    use super::*;
    use crate::app::AppClients;
    use crate::db::Database;
    use crate::infra::app_server;
    use crate::infra::tmux::{MockTmuxClient, TmuxClient};
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
    fn mock_app_server() -> std::sync::Arc<dyn app_server::AppServerClient> {
        std::sync::Arc::new(app_server::MockAppServerClient::new())
    }

    /// Builds one test app with an injected tmux boundary.
    async fn new_test_app_with_tmux_client(
        tmux_client: Arc<dyn TmuxClient>,
    ) -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = AppClients::new(mock_app_server()).with_tmux_client(tmux_client);
        let app = App::new_with_clients(base_path.clone(), base_path, None, database, clients)
            .await
            .expect("failed to build app");

        (app, base_dir)
    }

    /// Builds one test app with a strict mocked tmux boundary.
    async fn new_test_app() -> (App, tempfile::TempDir) {
        new_test_app_with_tmux_client(Arc::new(MockTmuxClient::new())).await
    }

    /// Builds one git-backed test app with an injected tmux boundary.
    async fn new_test_app_with_git_and_tmux_client(
        tmux_client: Arc<dyn TmuxClient>,
    ) -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        setup_test_git_repo(base_dir.path());
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = AppClients::new(mock_app_server()).with_tmux_client(tmux_client);
        let app = App::new_with_clients(
            base_path.clone(),
            base_path,
            Some("main".to_string()),
            database,
            clients,
        )
        .await
        .expect("failed to build app");

        (app, base_dir)
    }

    /// Builds one git-backed test app with a strict mocked tmux boundary.
    async fn new_test_app_with_git() -> (App, tempfile::TempDir) {
        new_test_app_with_git_and_tmux_client(Arc::new(MockTmuxClient::new())).await
    }

    #[tokio::test]
    async fn test_handle_view_info_popup_key_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::ViewInfoPopup {
            is_loading: false,
            loading_label: "Refreshing review request...".to_string(),
            message: "Review request refreshed.".to_string(),
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: Some(2),
                session_id: "session-id".to_string(),
            },
            title: "Review request refreshed".to_string(),
        };

        // Act
        let event_result =
            handle_view_info_popup_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if session_id == "session-id"
        ));
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

    #[test]
    fn test_confirmation_cancel_mode_returns_list_for_delete_confirmation() {
        // Arrange
        let mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::DeleteSession,
            confirmation_message: "Delete session?".to_string(),
            confirmation_title: "Confirm Delete".to_string(),
            restore_view: None,
            session_id: Some("session-id".to_string()),
            selected_confirmation_index: 0,
        };

        // Act
        let cancel_mode = confirmation_cancel_mode(&mode);

        // Assert
        assert!(matches!(cancel_mode, AppMode::List));
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

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_escape_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "agentty/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some("Preparing focused review".to_string()),
                focused_review_text: Some("Critical finding".to_string()),
                scroll_offset: Some(7),
                session_id: "session-id".to_string(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some(ref status_message),
                focused_review_text: Some(ref review_text),
                ref session_id,
                scroll_offset: Some(7),
            } if session_id == "session-id"
                && status_message == "Preparing focused review"
                && review_text == "Critical finding"
        ));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_char_updates_input_state() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "agentty/session".to_string(),
            input: crate::domain::input::InputState::default(),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: None,
                session_id: "session-id".to_string(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                input: ref input_state,
                ..
            } if input_state.cursor == 1 && input_state.text() == "r"
        ));
        let AppMode::PublishBranchInput { input, .. } = &app.mode else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(input.text(), "r");
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_left_moves_cursor() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "agentty/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: None,
                session_id: "session-id".to_string(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        let AppMode::PublishBranchInput { input, .. } = &app.mode else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(input.text(), "review/custom");
        assert_eq!(input.cursor, "review/custom".chars().count() - 1);
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_char_keeps_locked_branch_name() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "agentty/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: Some("origin/review/custom".to_string()),
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: None,
                session_id: "session-id".to_string(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        let AppMode::PublishBranchInput {
            input,
            locked_upstream_ref,
            ..
        } = &app.mode
        else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(locked_upstream_ref.as_deref(), Some("origin/review/custom"));
        assert_eq!(input.text(), "review/custom");
    }

    #[test]
    fn test_next_open_command_index_wraps_to_start() {
        // Arrange
        let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

        // Act
        let index = next_open_command_index(1, &commands);

        // Assert
        assert_eq!(index, 0);
    }

    #[test]
    fn test_previous_open_command_index_wraps_to_end() {
        // Arrange
        let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

        // Act
        let index = previous_open_command_index(0, &commands);

        // Assert
        assert_eq!(index, 1);
    }

    #[tokio::test]
    async fn test_handle_open_command_selector_key_escape_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::OpenCommandSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some("Preparing focused review".to_string()),
                focused_review_text: Some("Critical finding".to_string()),
                scroll_offset: Some(3),
                session_id: "session-id".to_string(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_open_command_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some(ref status_message),
                focused_review_text: Some(ref review_text),
                ref session_id,
                scroll_offset: Some(3),
            } if session_id == "session-id"
                && status_message == "Preparing focused review"
                && review_text == "Critical finding"
        ));
    }

    #[tokio::test]
    async fn test_handle_open_command_selector_key_j_updates_selected_index() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::OpenCommandSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: None,
                session_id: "session-id".to_string(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_open_command_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::OpenCommandSelector {
                selected_command_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_open_command_selector_key_with_empty_commands_keeps_index_zero() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::OpenCommandSelector {
            commands: Vec::new(),
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: None,
                session_id: "session-id".to_string(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_open_command_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::OpenCommandSelector {
                selected_command_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_open_command_selector_key_enter_restores_view_without_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::OpenCommandSelector {
            commands: vec!["cargo test".to_string()],
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: Some(4),
                session_id: "session-id".to_string(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_open_command_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                ref session_id,
                scroll_offset: Some(4),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_open_command_selector_key_enter_runs_selected_command_in_tmux() {
        // Arrange
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client
            .expect_open_window_for_folder()
            .times(1)
            .returning(|_| Box::pin(async { Some("@24".to_string()) }));
        mock_tmux_client
            .expect_run_command_in_window()
            .with(eq("@24".to_string()), eq("npm run dev".to_string()))
            .times(1)
            .returning(|_, _| Box::pin(async {}));
        let (mut app, _base_dir) =
            new_test_app_with_git_and_tmux_client(Arc::new(mock_tmux_client)).await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::OpenCommandSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                scroll_offset: Some(2),
                session_id: expected_session_id.clone(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_open_command_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
                ref session_id,
                scroll_offset: Some(2),
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_open_command_selector_key_unknown_key_preserves_state() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::OpenCommandSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some("Preparing focused review".to_string()),
                focused_review_text: Some("Critical finding".to_string()),
                scroll_offset: Some(1),
                session_id: "session-id".to_string(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_open_command_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::OpenCommandSelector {
                selected_command_index: 1,
                ref commands,
                restore_view:
                    ConfirmationViewMode {
                        done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                        focused_review_status_message: Some(ref status_message),
                        focused_review_text: Some(ref review_text),
                        scroll_offset: Some(1),
                        ref session_id,
                    },
            } if commands == &vec!["cargo test".to_string(), "npm run dev".to_string()]
                && session_id == "session-id"
                && status_message == "Preparing focused review"
                && review_text == "Critical finding"
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_regenerate_confirmation() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateFocusedReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: None,
                focused_review_text: Some("Previous review".to_string()),
                scroll_offset: Some(4),
                session_id: session_id.clone(),
            }),
            session_id: Some(session_id.clone()),
            selected_confirmation_index: 1,
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
                focused_review_text: Some(ref review_text),
                scroll_offset: Some(4),
                ..
            } if review_text == "Previous review"
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_regenerates_focused_review() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_folder = app.sessions.sessions[0].folder.clone();
        std::fs::write(session_folder.join("README.md"), "regenerate test\n")
            .expect("failed to write");
        app.focused_review_cache.insert(
            session_id.clone(),
            FocusedReviewCacheEntry::Ready {
                text: "Old review".to_string(),
                diff_hash: 99,
            },
        );
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateFocusedReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: None,
                focused_review_text: Some("Old review".to_string()),
                scroll_offset: None,
                session_id: session_id.clone(),
            }),
            session_id: Some(session_id.clone()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert — view is restored with loading state, cache shows new Loading entry
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some(_),
                focused_review_text: None,
                ..
            }
        ));
        assert!(matches!(
            app.focused_review_cache.get(&session_id),
            Some(FocusedReviewCacheEntry::Loading { .. })
        ));
    }
}
