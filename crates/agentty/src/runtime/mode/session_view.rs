use std::io;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::domain::agent::AgentModel;
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
    focused_review_status_message: Option<String>,
    focused_review_text: Option<String>,
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

#[derive(Clone, Copy)]
struct ViewMetrics {
    total_lines: u16,
    view_height: u16,
}

/// Pending output-mode and scroll updates produced by one key event in
/// session-view mode.
struct ViewPendingUpdate {
    done_session_output_mode: DoneSessionOutputMode,
    focused_review_status_message: Option<String>,
    focused_review_text: Option<String>,
    scroll_offset: Option<u16>,
}

impl ViewPendingUpdate {
    /// Builds update state seeded from the current view mode values.
    fn from_context(view_context: &ViewContext) -> Self {
        Self {
            done_session_output_mode: view_context.done_session_output_mode,
            focused_review_status_message: view_context.focused_review_status_message.clone(),
            focused_review_text: view_context.focused_review_text.clone(),
            scroll_offset: view_context.scroll_offset,
        }
    }
}

/// Borrowed per-key context used while processing one session-view key event.
struct ViewKeyContext<'a> {
    context: &'a ViewContext,
    metrics: ViewMetrics,
    session_snapshot: &'a ViewSessionSnapshot,
}

/// Snapshot of session-derived state used by view-mode key handling.
struct ViewSessionSnapshot {
    can_open_worktree: bool,
    is_action_allowed: bool,
    is_in_progress: bool,
    session_folder: PathBuf,
    session_output: String,
    session_summary: Option<String>,
    session_state: ViewSessionState,
    session_status: Status,
}

/// Prefix for the focused-review loading status while assist output is being
/// prepared.
const FOCUSED_REVIEW_LOADING_MESSAGE_PREFIX: &str = "Preparing focused review with agent help";
/// Fallback copy used when no changes exist for focused review.
const FOCUSED_REVIEW_NO_DIFF_MESSAGE: &str = "No diff changes found for focused review.";

/// Processes view-mode key presses and keeps shortcut availability aligned with
/// session status (`o`/`e` disabled for `Done`/`Canceled`, diff/focused review
/// only for `Review`).
pub(crate) async fn handle(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
    event_reader_pause: &AtomicBool,
) -> io::Result<EventResult> {
    let Some(view_context) = view_context(app) else {
        return Ok(EventResult::Continue);
    };
    let view_metrics = view_metrics(app, terminal, &view_context)?;
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    let Some(view_session_snapshot) = view_session_snapshot(app, &view_context) else {
        return Ok(EventResult::Continue);
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        metrics: view_metrics,
        session_snapshot: &view_session_snapshot,
    };

    if !handle_view_key(
        app,
        terminal,
        key,
        event_reader_pause,
        view_key_context,
        &mut pending_update,
    )
    .await
    {
        return Ok(EventResult::Continue);
    }

    apply_view_scroll_and_output_mode(
        app,
        pending_update.done_session_output_mode,
        pending_update.focused_review_status_message,
        pending_update.focused_review_text,
        pending_update.scroll_offset,
    );

    Ok(EventResult::Continue)
}

/// Applies one view-mode key press and updates pending output/scroll state.
///
/// Returns `false` when key handling already transitioned mode and should skip
/// applying pending view updates.
async fn handle_view_key(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
    event_reader_pause: &AtomicBool,
    view_key_context: ViewKeyContext<'_>,
    pending_update: &mut ViewPendingUpdate,
) -> bool {
    let view_context = view_key_context.context;
    let view_metrics = view_key_context.metrics;
    let view_session_snapshot = view_key_context.session_snapshot;

    match key.code {
        KeyCode::Char('q') => app.mode = AppMode::List,
        KeyCode::Char('o') if view_session_snapshot.can_open_worktree => {
            app.open_session_worktree_in_tmux().await;
        }
        KeyCode::Char('e') if view_session_snapshot.can_open_worktree => {
            open_external_editor_for_view_session(
                terminal,
                event_reader_pause,
                &view_session_snapshot.session_folder,
            )
            .await;
        }
        KeyCode::Enter if view_session_snapshot.is_action_allowed => {
            switch_view_to_prompt(
                app,
                view_context,
                PromptHistoryState::new(prompt_history_entries(
                    &view_session_snapshot.session_output,
                )),
                pending_update.scroll_offset,
            );
        }
        KeyCode::Char('j') | KeyCode::Down => {
            pending_update.scroll_offset =
                scroll_offset_down(pending_update.scroll_offset, view_metrics, 1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            pending_update.scroll_offset = Some(scroll_offset_up(
                pending_update.scroll_offset,
                view_metrics,
                1,
            ));
        }
        KeyCode::Char('g') => pending_update.scroll_offset = Some(0),
        KeyCode::Char('G') => pending_update.scroll_offset = None,
        KeyCode::Char('c')
            if key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.is_in_progress =>
        {
            stop_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            pending_update.scroll_offset =
                scroll_offset_half_page_down(pending_update.scroll_offset, view_metrics);
        }
        KeyCode::Char('u') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            pending_update.scroll_offset = Some(scroll_offset_half_page_up(
                pending_update.scroll_offset,
                view_metrics,
            ));
        }
        KeyCode::Char('d')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && is_view_diff_allowed(view_session_snapshot.session_status) =>
        {
            show_diff_for_view_session(app, view_context).await;
        }
        KeyCode::Char(character)
            if character.eq_ignore_ascii_case(&'f')
                && !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && is_view_focused_review_allowed(view_session_snapshot.session_status) =>
        {
            toggle_focused_review_for_pending(
                app,
                view_context,
                view_session_snapshot,
                pending_update,
            )
            .await;
        }
        KeyCode::Char('m') if view_session_snapshot.is_action_allowed => {
            merge_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('r') if view_session_snapshot.is_action_allowed => {
            rebase_view_session(app, &view_context.session_id).await;
        }
        _ if is_done_output_toggle_key(view_session_snapshot.session_status, key) => {
            pending_update.done_session_output_mode =
                pending_update.done_session_output_mode.toggled();
            pending_update.scroll_offset = None;
        }
        KeyCode::Char('?') => {
            open_view_help_overlay(app, view_context, view_session_snapshot.session_state);
            return false;
        }
        _ => {}
    }

    true
}

/// Applies focused-review output toggles into the mutable pending view state.
async fn toggle_focused_review_for_pending(
    app: &mut App,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
    pending_update: &mut ViewPendingUpdate,
) {
    toggle_focused_review(
        app,
        view_context,
        view_session_snapshot,
        &mut pending_update.done_session_output_mode,
        &mut pending_update.focused_review_status_message,
        &mut pending_update.focused_review_text,
        &mut pending_update.scroll_offset,
    )
    .await;
}

/// Toggles focused-review output and resets scroll to the bottom-aligned mode.
async fn toggle_focused_review(
    app: &mut App,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
    next_done_session_output_mode: &mut DoneSessionOutputMode,
    next_focused_review_status_message: &mut Option<String>,
    next_focused_review_text: &mut Option<String>,
    next_scroll_offset: &mut Option<u16>,
) {
    toggle_focused_review_output_mode(
        app,
        view_context,
        view_session_snapshot,
        next_done_session_output_mode,
        next_focused_review_status_message,
        next_focused_review_text,
    )
    .await;

    *next_scroll_offset = None;
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
        session_folder: session.folder.clone(),
        session_output: session.output.clone(),
        session_summary: session.summary.clone(),
        session_state: view_session_state(session_status),
        session_status,
    })
}

/// Applies in-place updates for active view output mode, focused-review
/// status/text, and scroll position.
fn apply_view_scroll_and_output_mode(
    app: &mut App,
    done_session_output_mode: DoneSessionOutputMode,
    focused_review_status_message: Option<String>,
    focused_review_text: Option<String>,
    scroll_offset: Option<u16>,
) {
    if let AppMode::View {
        done_session_output_mode: view_done_session_output_mode,
        focused_review_status_message: view_focused_review_status_message,
        focused_review_text: view_focused_review_text,
        scroll_offset: view_scroll_offset,
        ..
    } = &mut app.mode
    {
        *view_done_session_output_mode = done_session_output_mode;
        *view_focused_review_status_message = focused_review_status_message;
        *view_focused_review_text = focused_review_text;
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

/// Returns whether `o` and `e` can access the session worktree.
fn is_view_worktree_open_allowed(status: Status) -> bool {
    !matches!(status, Status::Done | Status::Canceled)
}

/// Returns whether non-navigation view shortcuts are available.
///
/// This covers `Enter`, `m`, `r`, and `S-Tab`.
fn is_view_action_allowed(status: Status) -> bool {
    !matches!(status, Status::Done | Status::InProgress | Status::Canceled)
}

/// Returns whether the `d` shortcut can open the diff view.
fn is_view_diff_allowed(status: Status) -> bool {
    status == Status::Review
}

/// Returns whether the `f` shortcut can open focused review content.
fn is_view_focused_review_allowed(status: Status) -> bool {
    status == Status::Review
}

/// Opens `nvim` from the currently viewed session worktree root.
///
/// Editor launch failures are ignored here so view-mode state remains stable.
async fn open_external_editor_for_view_session(
    terminal: &mut TuiTerminal,
    event_reader_pause: &AtomicBool,
    session_folder: &std::path::Path,
) {
    let _ = crate::runtime::terminal::open_nvim(terminal, event_reader_pause, session_folder).await;
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

/// Returns whether worktree-dependent shortcuts (`o` and `e`) are enabled for
/// the provided session status.
fn can_open_session_worktree(status: Status) -> bool {
    !matches!(status, Status::Done | Status::Canceled)
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
            focused_review_status_message: view_context.focused_review_status_message.clone(),
            focused_review_text: view_context.focused_review_text.clone(),
            session_id: view_context.session_id.clone(),
            session_state,
            scroll_offset: view_context.scroll_offset,
        },
        scroll_offset: 0,
    };
}

fn view_context(app: &mut App) -> Option<ViewContext> {
    let (
        done_session_output_mode,
        focused_review_status_message,
        focused_review_text,
        session_id,
        scroll_offset,
    ) = match &app.mode {
        AppMode::View {
            done_session_output_mode,
            focused_review_status_message,
            focused_review_text,
            session_id,
            scroll_offset,
        } => (
            *done_session_output_mode,
            focused_review_status_message.clone(),
            focused_review_text.clone(),
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
        focused_review_status_message,
        focused_review_text,
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
        view_context.focused_review_status_message.as_deref(),
        view_context.focused_review_text.as_deref(),
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
    focused_review_status_message: Option<&str>,
    focused_review_text: Option<&str>,
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
                focused_review_status_message,
                focused_review_text,
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

/// Computes the next scroll offset for half-page downward navigation.
fn scroll_offset_half_page_down(scroll_offset: Option<u16>, metrics: ViewMetrics) -> Option<u16> {
    scroll_offset_down(scroll_offset, metrics, half_page_scroll_step(metrics))
}

/// Computes the next scroll offset for half-page upward navigation.
fn scroll_offset_half_page_up(scroll_offset: Option<u16>, metrics: ViewMetrics) -> u16 {
    scroll_offset_up(scroll_offset, metrics, half_page_scroll_step(metrics))
}

/// Returns the number of lines used for half-page scroll shortcuts.
fn half_page_scroll_step(metrics: ViewMetrics) -> u16 {
    metrics.view_height / 2
}

/// Toggles focused-review mode and optionally starts focused-review assist.
///
/// When focused-review content is missing, this loads diff text and either
/// starts async assist generation or stores the diff directly when assist is
/// not applicable.
async fn toggle_focused_review_output_mode(
    app: &mut App,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
    done_session_output_mode: &mut DoneSessionOutputMode,
    focused_review_status_message: &mut Option<String>,
    focused_review_text: &mut Option<String>,
) {
    if *done_session_output_mode == DoneSessionOutputMode::FocusedReview {
        *done_session_output_mode = DoneSessionOutputMode::Summary;

        return;
    }

    *done_session_output_mode = DoneSessionOutputMode::FocusedReview;

    let focused_review_is_loading = focused_review_status_message
        .as_deref()
        .is_some_and(is_focused_review_loading_status_message);
    if focused_review_text.is_some() || focused_review_is_loading {
        return;
    }

    let focused_review_diff = focused_review_diff_for_view_session(app, view_context).await;
    if should_request_focused_review_assist(&focused_review_diff) {
        let review_model = focused_review_assist_model(app);
        *focused_review_status_message =
            focused_review_initial_status_message(&focused_review_diff, review_model);
        *focused_review_text = None;
        app.start_focused_review_assist(
            &view_context.session_id,
            view_session_snapshot.session_folder.as_path(),
            &focused_review_diff,
            view_session_snapshot.session_summary.as_deref(),
        );

        return;
    }

    *focused_review_status_message = None;
    *focused_review_text = Some(focused_review_diff);
}

async fn show_diff_for_view_session(app: &mut App, view_context: &ViewContext) {
    let diff = load_view_session_diff(app, view_context).await;

    app.mode = AppMode::Diff {
        session_id: view_context.session_id.clone(),
        diff,
        scroll_offset: 0,
        file_explorer_selected_index: 0,
    };
}

/// Loads unified diff text for focused-review mode and falls back to a
/// user-facing message when loading fails.
async fn focused_review_diff_for_view_session(app: &App, view_context: &ViewContext) -> String {
    let diff = load_view_session_diff(app, view_context).await;
    if diff.trim().is_empty() {
        return FOCUSED_REVIEW_NO_DIFF_MESSAGE.to_string();
    }

    diff
}

/// Returns whether focused-review assist should run for the current diff text.
fn should_request_focused_review_assist(diff: &str) -> bool {
    let trimmed_diff = diff.trim();
    if trimmed_diff.is_empty() || trimmed_diff == FOCUSED_REVIEW_NO_DIFF_MESSAGE {
        return false;
    }

    !trimmed_diff.starts_with("Failed to run git diff:")
}

/// Returns whether a focused-review status line indicates assist is still
/// loading.
fn is_focused_review_loading_status_message(status_message: &str) -> bool {
    status_message.starts_with(FOCUSED_REVIEW_LOADING_MESSAGE_PREFIX)
}

/// Returns the configured model used for focused-review assist generation.
fn focused_review_assist_model(app: &App) -> AgentModel {
    app.settings.default_review_model
}

/// Returns the status line shown while focused-review assist is pending.
fn focused_review_initial_status_message(diff: &str, review_model: AgentModel) -> Option<String> {
    if should_request_focused_review_assist(diff) {
        return Some(focused_review_loading_message(review_model));
    }

    None
}

/// Formats the focused-review loading status line with the active model name.
fn focused_review_loading_message(review_model: AgentModel) -> String {
    format!(
        "{FOCUSED_REVIEW_LOADING_MESSAGE_PREFIX} with model {}...",
        review_model.as_str(),
    )
}

/// Loads the session worktree diff against its base branch.
async fn load_view_session_diff(app: &App, view_context: &ViewContext) -> String {
    let Some(session) = app.sessions.sessions.get(view_context.session_index) else {
        return String::new();
    };

    let session_folder = session.folder.clone();
    let base_branch = session.base_branch.clone();

    app.services
        .git_client()
        .diff(session_folder, base_branch)
        .await
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"))
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
    fn test_is_view_worktree_open_allowed_returns_false_for_canceled() {
        // Arrange
        let status = Status::Canceled;

        // Act
        let can_open = is_view_worktree_open_allowed(status);

        // Assert
        assert!(!can_open);
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
        let canceled_status = Status::Canceled;
        let review_status = Status::Review;
        let in_progress_status = Status::InProgress;
        let done_status = Status::Done;

        // Act
        let canceled_allowed = is_view_action_allowed(canceled_status);
        let review_allowed = is_view_action_allowed(review_status);
        let in_progress_allowed = is_view_action_allowed(in_progress_status);
        let done_allowed = is_view_action_allowed(done_status);

        // Assert
        assert!(!canceled_allowed);
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
    fn test_is_view_focused_review_allowed_only_for_review_status() {
        // Arrange
        let review_status = Status::Review;
        let done_status = Status::Done;
        let in_progress_status = Status::InProgress;

        // Act
        let review_allowed = is_view_focused_review_allowed(review_status);
        let done_allowed = is_view_focused_review_allowed(done_status);
        let in_progress_allowed = is_view_focused_review_allowed(in_progress_status);

        // Assert
        assert!(review_allowed);
        assert!(!done_allowed);
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
            focused_review_status_message: None,
            focused_review_text: None,
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
            focused_review_status_message: None,
            focused_review_text: None,
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
        let total_lines = view_total_lines(
            &app,
            &session_id,
            0,
            DoneSessionOutputMode::Summary,
            None,
            None,
            20,
        );

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
        let summary_lines = view_total_lines(
            &app,
            &session_id,
            0,
            DoneSessionOutputMode::Summary,
            None,
            None,
            20,
        );
        let output_lines = view_total_lines(
            &app,
            &session_id,
            0,
            DoneSessionOutputMode::Output,
            None,
            None,
            20,
        );

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
            total_lines: view_total_lines(
                &app,
                &session_id,
                0,
                DoneSessionOutputMode::Summary,
                None,
                None,
                20,
            ),
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
    async fn test_apply_view_scroll_and_output_mode_updates_focused_review_state() {
        // Arrange
        let (mut app, _base_dir, expected_session_id) = new_test_app_with_session().await;
        let expected_status_message = focused_review_loading_message(AgentModel::Gpt53Codex);
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            session_id: expected_session_id.clone(),
            scroll_offset: Some(3),
        };

        // Act
        apply_view_scroll_and_output_mode(
            &mut app,
            DoneSessionOutputMode::FocusedReview,
            Some(expected_status_message.clone()),
            None,
            Some(1),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some(ref actual_status_message),
                focused_review_text: None,
                ref session_id,
                scroll_offset: Some(1),
            } if session_id == &expected_session_id
                && actual_status_message == &expected_status_message
        ));
    }

    #[test]
    fn test_is_focused_review_loading_status_message_matches_model_aware_message() {
        // Arrange
        let status_message = focused_review_loading_message(AgentModel::ClaudeOpus46);

        // Act
        let is_loading = is_focused_review_loading_status_message(&status_message);

        // Assert
        assert!(is_loading);
    }

    #[test]
    fn test_is_focused_review_loading_status_message_rejects_unrelated_message() {
        // Arrange
        let status_message = "Focused review complete.";

        // Act
        let is_loading = is_focused_review_loading_status_message(status_message);

        // Assert
        assert!(!is_loading);
    }

    #[tokio::test]
    async fn test_focused_review_assist_model_returns_default_review_model_setting() {
        // Arrange
        let (mut app, _base_dir, _session_id) = new_test_app_with_session().await;
        app.settings.default_review_model = AgentModel::ClaudeOpus46;

        // Act
        let review_model = focused_review_assist_model(&app);

        // Assert
        assert_eq!(review_model, AgentModel::ClaudeOpus46);
    }

    #[tokio::test]
    async fn test_focused_review_diff_for_view_session_returns_empty_diff_message() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            scroll_offset: None,
            session_id,
            session_index: 0,
        };

        // Act
        let focused_review_diff = focused_review_diff_for_view_session(&app, &context).await;

        // Assert
        assert_eq!(focused_review_diff, FOCUSED_REVIEW_NO_DIFF_MESSAGE);
    }

    #[tokio::test]
    async fn test_show_diff_for_view_session_switches_mode_to_diff() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
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
    async fn test_focused_review_diff_for_view_session_returns_git_diff_text() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let session_folder = app.sessions.sessions[0].folder.clone();
        std::fs::write(
            session_folder.join("README.md"),
            "focused review test content\n",
        )
        .expect("failed to update readme");
        let context = ViewContext {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            scroll_offset: Some(0),
            session_id,
            session_index: 0,
        };

        // Act
        let diff = focused_review_diff_for_view_session(&app, &context).await;

        // Assert
        assert!(diff.contains("README.md"));
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
            focused_review_status_message: None,
            focused_review_text: None,
            session_id: session_id.clone(),
            scroll_offset: scroll,
        };

        // Act — simulate what the `?` arm does
        app.mode = AppMode::Help {
            context: HelpContext::View {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                focused_review_status_message: None,
                focused_review_text: None,
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
                    focused_review_status_message: None,
                    focused_review_text: None,
                    ref session_id,
                    session_state: ViewSessionState::Interactive,
                    scroll_offset: Some(3),
                },
                scroll_offset: 0,
            } if !session_id.is_empty()
        ));
    }

    #[test]
    fn test_can_open_session_worktree_disables_canceled_state() {
        // Arrange
        let status = Status::Canceled;

        // Act
        let result = can_open_session_worktree(status);

        // Assert
        assert!(!result);
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
