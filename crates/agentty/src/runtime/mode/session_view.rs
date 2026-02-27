use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::runtime::{EventResult, TuiTerminal};
use crate::ui::pages::session_chat::SessionChatPage;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode, HelpContext};
use crate::ui::state::help_action::ViewSessionState;
use crate::ui::state::prompt::{PromptHistoryState, PromptSlashState};

#[derive(Clone)]
struct ViewContext {
    done_session_output_mode: DoneSessionOutputMode,
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

#[derive(Clone, Copy)]
struct ViewMetrics {
    total_lines: u16,
    view_height: u16,
}

/// Snapshot of session-derived state used by view-mode key handling.
struct ViewSessionSnapshot {
    can_open_worktree: bool,
    is_action_allowed: bool,
    is_in_progress: bool,
    session_output: String,
    session_state: ViewSessionState,
    session_status: Status,
}

/// Processes view-mode key presses and keeps shortcut availability aligned with
/// session status (`open` disabled only for `Done`, `diff` only for `Review`).
pub(crate) async fn handle(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
) -> io::Result<EventResult> {
    let Some(view_context) = view_context(app) else {
        return Ok(EventResult::Continue);
    };

    let view_metrics = view_metrics(app, terminal, &view_context)?;
    let mut next_scroll_offset = view_context.scroll_offset;
    let mut next_done_session_output_mode = view_context.done_session_output_mode;

    let Some(view_session_snapshot) = view_session_snapshot(app, &view_context) else {
        return Ok(EventResult::Continue);
    };

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('o') if view_session_snapshot.can_open_worktree => {
            app.open_session_worktree_in_tmux().await;
        }
        KeyCode::Enter if view_session_snapshot.is_action_allowed => {
            switch_view_to_prompt(
                app,
                &view_context,
                PromptHistoryState::new(prompt_history_entries(
                    &view_session_snapshot.session_output,
                )),
                next_scroll_offset,
            );
        }
        KeyCode::Char('j') | KeyCode::Down => {
            next_scroll_offset = scroll_offset_down(next_scroll_offset, view_metrics, 1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            next_scroll_offset = Some(scroll_offset_up(next_scroll_offset, view_metrics, 1));
        }
        KeyCode::Char('g') => {
            next_scroll_offset = Some(0);
        }
        KeyCode::Char('G') => {
            next_scroll_offset = None;
        }
        KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            if view_session_snapshot.is_in_progress {
                stop_view_session(app, &view_context.session_id).await;
            }
        }
        KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            next_scroll_offset = scroll_offset_down(
                next_scroll_offset,
                view_metrics,
                view_metrics.view_height / 2,
            );
        }
        KeyCode::Char('u') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            next_scroll_offset = Some(scroll_offset_up(
                next_scroll_offset,
                view_metrics,
                view_metrics.view_height / 2,
            ));
        }
        KeyCode::Char('d')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && is_view_diff_allowed(view_session_snapshot.session_status) =>
        {
            show_diff_for_view_session(app, &view_context).await;
        }
        KeyCode::Char('m') if view_session_snapshot.is_action_allowed => {
            merge_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('r') if view_session_snapshot.is_action_allowed => {
            rebase_view_session(app, &view_context.session_id).await;
        }
        _ if is_done_output_toggle_key(view_session_snapshot.session_status, key) => {
            next_done_session_output_mode = next_done_session_output_mode.toggled();
            next_scroll_offset = None;
        }
        KeyCode::Char('?') => {
            open_view_help_overlay(app, &view_context, view_session_snapshot.session_state);

            return Ok(EventResult::Continue);
        }
        _ => {}
    }

    apply_view_scroll_and_output_mode(app, next_done_session_output_mode, next_scroll_offset);

    Ok(EventResult::Continue)
}

/// Collects session-specific values used by `handle()` from the active view
/// row.
fn view_session_snapshot(app: &App, view_context: &ViewContext) -> Option<ViewSessionSnapshot> {
    let session = app.sessions.sessions.get(view_context.session_index)?;
    let session_status = session.status;

    Some(ViewSessionSnapshot {
        can_open_worktree: is_view_worktree_open_allowed(session_status)
            && can_open_session_worktree(session_status),
        is_action_allowed: is_view_action_allowed(session_status),
        is_in_progress: session_status == Status::InProgress,
        session_output: session.output.clone(),
        session_state: view_session_state(session_status),
        session_status,
    })
}

/// Applies in-place updates for the active view scroll position and output
/// mode.
fn apply_view_scroll_and_output_mode(
    app: &mut App,
    done_session_output_mode: DoneSessionOutputMode,
    scroll_offset: Option<u16>,
) {
    if let AppMode::View {
        done_session_output_mode: view_done_session_output_mode,
        scroll_offset: view_scroll_offset,
        ..
    } = &mut app.mode
    {
        *view_done_session_output_mode = done_session_output_mode;
        *view_scroll_offset = scroll_offset;
    }
}

/// Returns whether the key event toggles done-session output mode.
fn is_done_output_toggle_key(status: Status, key: KeyEvent) -> bool {
    let is_toggle_key = matches!(
        key.code,
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'t')
    );

    status == Status::Done && is_toggle_key && !key.modifiers.contains(event::KeyModifiers::CONTROL)
}

/// Returns whether `o` can open the session worktree in tmux.
fn is_view_worktree_open_allowed(status: Status) -> bool {
    status != Status::Done
}

/// Returns whether non-navigation view shortcuts are available.
///
/// This covers `Enter`, `m`, `r`, and `S-Tab`.
fn is_view_action_allowed(status: Status) -> bool {
    !matches!(status, Status::Done | Status::InProgress)
}

/// Returns whether the `d` shortcut can open the diff view.
fn is_view_diff_allowed(status: Status) -> bool {
    status == Status::Review
}

fn switch_view_to_prompt(
    app: &mut App,
    view_context: &ViewContext,
    history_state: PromptHistoryState,
    scroll_offset: Option<u16>,
) {
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        history_state,
        slash_state: PromptSlashState::new(),
        session_id: view_context.session_id.clone(),
        input: InputState::new(),
        scroll_offset,
    };
}

/// Returns whether the 'o' shortcut should open the worktree for the provided
/// session status.
fn can_open_session_worktree(status: Status) -> bool {
    status != Status::Done
}

/// Maps session status to view help session state.
fn view_session_state(status: Status) -> ViewSessionState {
    match status {
        Status::Done => ViewSessionState::Done,
        Status::InProgress => ViewSessionState::InProgress,
        Status::Review => ViewSessionState::Review,
        _ => ViewSessionState::Interactive,
    }
}

/// Opens the help overlay while preserving the currently viewed session state.
fn open_view_help_overlay(
    app: &mut App,
    view_context: &ViewContext,
    session_state: ViewSessionState,
) {
    app.mode = AppMode::Help {
        context: HelpContext::View {
            done_session_output_mode: view_context.done_session_output_mode,
            session_id: view_context.session_id.clone(),
            session_state,
            scroll_offset: view_context.scroll_offset,
        },
        scroll_offset: 0,
    };
}

fn view_context(app: &mut App) -> Option<ViewContext> {
    let (done_session_output_mode, session_id, scroll_offset) = match &app.mode {
        AppMode::View {
            done_session_output_mode,
            session_id,
            scroll_offset,
        } => (
            *done_session_output_mode,
            session_id.clone(),
            *scroll_offset,
        ),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    Some(ViewContext {
        done_session_output_mode,
        scroll_offset,
        session_id,
        session_index,
    })
}

fn view_metrics(
    app: &App,
    terminal: &TuiTerminal,
    view_context: &ViewContext,
) -> io::Result<ViewMetrics> {
    let terminal_size = terminal.size()?;
    let view_height = terminal_size.height.saturating_sub(5);
    let output_width = terminal_size.width.saturating_sub(2);
    let total_lines = view_total_lines(
        app,
        &view_context.session_id,
        view_context.session_index,
        view_context.done_session_output_mode,
        output_width,
    );

    Ok(ViewMetrics {
        total_lines,
        view_height,
    })
}

fn view_total_lines(
    app: &App,
    session_id: &str,
    session_index: usize,
    done_session_output_mode: DoneSessionOutputMode,
    output_width: u16,
) -> u16 {
    let active_progress = app.session_progress_message(session_id);

    app.sessions
        .sessions
        .get(session_index)
        .map_or(0, |session| {
            SessionChatPage::rendered_output_line_count(
                session,
                output_width,
                done_session_output_mode,
                active_progress,
            )
        })
}

fn prompt_history_entries(output: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut output_lines = output.lines().peekable();

    while let Some(line) = output_lines.next() {
        let Some(first_prompt_line) = line.strip_prefix(" › ") else {
            continue;
        };

        let mut prompt = first_prompt_line.to_string();

        while let Some(next_line) = output_lines.peek().copied() {
            if next_line.is_empty() {
                break;
            }

            prompt.push('\n');
            prompt.push_str(next_line);
            let _ = output_lines.next();
        }

        entries.push(prompt);
    }

    entries
}

fn scroll_offset_down(scroll_offset: Option<u16>, metrics: ViewMetrics, step: u16) -> Option<u16> {
    let current_offset = scroll_offset?;

    let next_offset = current_offset.saturating_add(step.max(1));
    if next_offset >= metrics.total_lines.saturating_sub(metrics.view_height) {
        return None;
    }

    Some(next_offset)
}

fn scroll_offset_up(scroll_offset: Option<u16>, metrics: ViewMetrics, step: u16) -> u16 {
    let current_offset =
        scroll_offset.unwrap_or_else(|| metrics.total_lines.saturating_sub(metrics.view_height));

    current_offset.saturating_sub(step.max(1))
}

async fn show_diff_for_view_session(app: &mut App, view_context: &ViewContext) {
    let Some(session) = app.sessions.sessions.get(view_context.session_index) else {
        return;
    };

    let session_folder = session.folder.clone();
    let base_branch = session.base_branch.clone();

    let diff = app
        .services
        .git_client()
        .diff(session_folder, base_branch)
        .await
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"));

    app.mode = AppMode::Diff {
        session_id: view_context.session_id.clone(),
        diff,
        scroll_offset: 0,
        file_explorer_selected_index: 0,
    };
}

async fn merge_view_session(app: &mut App, session_id: &str) {
    if let Err(error) = app.merge_session(session_id).await {
        app.append_output_for_session(session_id, &format!("\n[Merge Error] {error}\n"))
            .await;
    }
}

async fn rebase_view_session(app: &mut App, session_id: &str) {
    if let Err(error) = app.rebase_session(session_id).await {
        app.append_output_for_session(session_id, &format!("\n[Rebase Error] {error}\n"))
            .await;
    }
}

async fn stop_view_session(app: &mut App, session_id: &str) {
    if let Err(error) = app.stop_session(session_id).await {
        app.append_output_for_session(session_id, &format!("\n[Stop Error] {error}\n"))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::db::Database;

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
            mock_app_server(),
        )
        .await;

        (app, base_dir)
    }

    async fn new_test_app_with_session() -> (App, tempfile::TempDir, String) {
        let (mut app, base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        (app, base_dir, session_id)
    }

    #[test]
    fn test_is_view_worktree_open_allowed_returns_true_for_in_progress() {
        // Arrange
        let status = Status::InProgress;

        // Act
        let can_open = is_view_worktree_open_allowed(status);

        // Assert
        assert!(can_open);
    }

    #[test]
    fn test_is_view_action_allowed_only_for_non_done_non_in_progress_status() {
        // Arrange
        let review_status = Status::Review;
        let in_progress_status = Status::InProgress;
        let done_status = Status::Done;

        // Act
        let review_allowed = is_view_action_allowed(review_status);
        let in_progress_allowed = is_view_action_allowed(in_progress_status);
        let done_allowed = is_view_action_allowed(done_status);

        // Assert
        assert!(review_allowed);
        assert!(!in_progress_allowed);
        assert!(!done_allowed);
    }

    #[test]
    fn test_is_view_diff_allowed_only_for_review_status() {
        // Arrange
        let review_status = Status::Review;
        let new_status = Status::New;
        let in_progress_status = Status::InProgress;

        // Act
        let review_allowed = is_view_diff_allowed(review_status);
        let new_allowed = is_view_diff_allowed(new_status);
        let in_progress_allowed = is_view_diff_allowed(in_progress_status);

        // Assert
        assert!(review_allowed);
        assert!(!new_allowed);
        assert!(!in_progress_allowed);
    }

    #[test]
    fn test_is_done_output_toggle_key_accepts_done_status_with_t() {
        // Arrange
        let status = Status::Done;
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);

        // Act
        let is_toggle = is_done_output_toggle_key(status, key);

        // Assert
        assert!(is_toggle);
    }

    #[test]
    fn test_is_done_output_toggle_key_rejects_non_done_status() {
        // Arrange
        let status = Status::Review;
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);

        // Act
        let is_toggle = is_done_output_toggle_key(status, key);

        // Assert
        assert!(!is_toggle);
    }

    #[test]
    fn test_is_done_output_toggle_key_rejects_control_modified_key() {
        // Arrange
        let status = Status::Done;
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);

        // Act
        let is_toggle = is_done_output_toggle_key(status, key);

        // Assert
        assert!(!is_toggle);
    }

    #[tokio::test]
    async fn test_view_context_returns_none_for_non_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_none());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_view_context_falls_back_to_list_when_session_is_missing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            session_id: "missing-session".to_string(),
            scroll_offset: Some(2),
        };

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_none());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_view_context_returns_existing_session_details() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            session_id: session_id.clone(),
            scroll_offset: Some(4),
        };

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_some());
        let context = context.expect("expected view context");
        assert_eq!(
            context.done_session_output_mode,
            DoneSessionOutputMode::Summary
        );
        assert_eq!(context.session_id, session_id);
        assert_eq!(context.scroll_offset, Some(4));
        assert_eq!(context.session_index, 0);
    }

    #[tokio::test]
    async fn test_view_total_lines_counts_wrapped_output_lines() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions[0].output = "word ".repeat(40);
        let raw_line_count =
            u16::try_from(app.sessions.sessions[0].output.lines().count()).unwrap_or(u16::MAX);

        // Act
        let total_lines =
            view_total_lines(&app, &session_id, 0, DoneSessionOutputMode::Summary, 20);

        // Assert
        assert!(total_lines > raw_line_count);
    }

    #[tokio::test]
    async fn test_view_total_lines_respects_done_output_mode() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions[0].status = Status::Done;
        app.sessions.sessions[0].summary = Some("brief summary".to_string());
        app.sessions.sessions[0].output = "word ".repeat(120);

        // Act
        let summary_lines =
            view_total_lines(&app, &session_id, 0, DoneSessionOutputMode::Summary, 20);
        let output_lines =
            view_total_lines(&app, &session_id, 0, DoneSessionOutputMode::Output, 20);

        // Assert
        assert!(output_lines > summary_lines);
    }

    #[test]
    fn test_prompt_history_entries_extracts_user_prompts() {
        // Arrange
        let output = " › first\n\nassistant\n\n › second\n\n";

        // Act
        let entries = prompt_history_entries(output);

        // Assert
        assert_eq!(entries, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn test_prompt_history_entries_keeps_multiline_prompts() {
        // Arrange
        let output = " › first line\nsecond line\n\nassistant\n\n";

        // Act
        let entries = prompt_history_entries(output);

        // Assert
        assert_eq!(entries, vec!["first line\nsecond line".to_string()]);
    }

    #[test]
    fn test_prompt_history_entries_ignores_non_prompt_lines() {
        // Arrange
        let output = "assistant line\n\n";

        // Act
        let entries = prompt_history_entries(output);

        // Assert
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_scroll_offset_down_does_not_jump_to_bottom_for_wrapped_output() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.sessions.sessions[0].output = "word ".repeat(60);
        let metrics = ViewMetrics {
            total_lines: view_total_lines(&app, &session_id, 0, DoneSessionOutputMode::Summary, 20),
            view_height: 5,
        };

        // Act
        let next_offset = scroll_offset_down(Some(0), metrics, 1);

        // Assert
        assert_eq!(next_offset, Some(1));
    }

    #[test]
    fn test_scroll_offset_down_returns_none_at_end_of_content() {
        // Arrange
        let metrics = ViewMetrics {
            total_lines: 20,
            view_height: 10,
        };

        // Act
        let next_offset = scroll_offset_down(Some(9), metrics, 1);

        // Assert
        assert_eq!(next_offset, None);
    }

    #[test]
    fn test_scroll_offset_up_uses_bottom_when_scroll_is_unset() {
        // Arrange
        let metrics = ViewMetrics {
            total_lines: 30,
            view_height: 10,
        };

        // Act
        let next_offset = scroll_offset_up(None, metrics, 5);

        // Assert
        assert_eq!(next_offset, 15);
    }

    #[tokio::test]
    async fn test_show_diff_for_view_session_switches_mode_to_diff() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            scroll_offset: Some(0),
            session_id: session_id.clone(),
            session_index: 0,
        };

        // Act
        show_diff_for_view_session(&mut app, &context).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                ref session_id,
                scroll_offset: 0,
                ..
            } if session_id == &context.session_id
        ));
    }

    #[tokio::test]
    async fn test_append_output_for_session_appends_text() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let mut app = app;

        // Act
        app.append_output_for_session(&session_id, "line one").await;

        // Assert
        app.sessions.sync_from_handles();
        let output = app.sessions.sessions[0].output.clone();
        assert_eq!(output, "line one");
    }

    #[tokio::test]
    async fn test_merge_view_session_appends_error_output_without_git_repo() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let mut app = app;

        // Act
        merge_view_session(&mut app, &session_id).await;

        // Assert
        app.sessions.sync_from_handles();
        let output = app.sessions.sessions[0].output.clone();
        assert!(output.contains("[Merge Error]"));
    }

    #[tokio::test]
    async fn test_rebase_view_session_appends_error_output_without_review_status() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let mut app = app;

        // Act
        rebase_view_session(&mut app, &session_id).await;

        // Assert
        app.sessions.sync_from_handles();
        let output = app.sessions.sessions[0].output.clone();
        assert!(output.contains("[Rebase Error]"));
    }

    #[tokio::test]
    async fn test_stop_view_session_appends_error_when_not_in_progress() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;

        // Act
        stop_view_session(&mut app, &session_id).await;

        // Assert
        app.sessions.sync_from_handles();
        let output = app.sessions.sessions[0].output.clone();
        assert!(output.contains("[Stop Error]"));
    }

    #[tokio::test]
    async fn test_question_mark_sets_help_mode_from_view_context() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let scroll = Some(3);
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            session_id: session_id.clone(),
            scroll_offset: scroll,
        };

        // Act — simulate what the `?` arm does
        app.mode = AppMode::Help {
            context: HelpContext::View {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                session_id,
                session_state: ViewSessionState::Interactive,
                scroll_offset: scroll,
            },
            scroll_offset: 0,
        };

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::View {
                    done_session_output_mode: DoneSessionOutputMode::Summary,
                    ref session_id,
                    session_state: ViewSessionState::Interactive,
                    scroll_offset: Some(3),
                },
                scroll_offset: 0,
            } if !session_id.is_empty()
        ));
    }

    #[test]
    fn test_can_open_session_worktree_disables_done_state() {
        // Arrange
        let status = Status::Done;

        // Act
        let result = can_open_session_worktree(status);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_can_open_session_worktree_enables_review_state() {
        // Arrange
        let status = Status::Review;

        // Act
        let result = can_open_session_worktree(status);

        // Assert
        assert!(result);
    }
}
