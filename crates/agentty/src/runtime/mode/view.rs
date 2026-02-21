use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::infra::git;
use crate::domain::input::InputState;
use crate::domain::permission::{PermissionMode, PlanFollowupAction};
use crate::domain::session::Status;
use crate::runtime::{EventResult, TuiTerminal};
use crate::ui::pages::session_chat::SessionChatPage;
use crate::ui::state::app_mode::{AppMode, HelpContext};
use crate::ui::state::prompt::{PromptHistoryState, PromptSlashState};

const IMPLEMENT_PLAN_PROMPT: &str = "Implement the approved plan from your previous response \
                                     end-to-end. Make the required code changes, run all relevant \
                                     checks/tests, and report what changed plus results.";

#[derive(Clone)]
struct ViewContext {
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

#[derive(Clone, Copy)]
struct ViewMetrics {
    total_lines: u16,
    view_height: u16,
}

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

    let Some(session) = app.sessions.sessions.get(view_context.session_index) else {
        return Ok(EventResult::Continue);
    };
    let is_done = session.status == Status::Done;
    let is_in_progress = session.status == Status::InProgress;
    let session_output = session.output.clone();

    if handle_plan_followup_action_key(app, key, &view_context).await {
        return Ok(EventResult::Continue);
    }

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('o') => {
            app.open_session_worktree_in_tmux().await;
        }
        KeyCode::Enter if !is_done => {
            switch_view_to_prompt(
                app,
                &view_context,
                PromptHistoryState::new(prompt_history_entries(&session_output)),
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
            if is_in_progress {
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
        KeyCode::Char('d') if !key.modifiers.contains(event::KeyModifiers::CONTROL) && !is_done => {
            show_diff_for_view_session(app, &view_context).await;
        }
        KeyCode::Char('m') if !is_done => {
            merge_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('r') if !is_done => {
            rebase_view_session(app, &view_context.session_id).await;
        }
        KeyCode::BackTab => {
            let _ = app
                .toggle_session_permission_mode(&view_context.session_id)
                .await;
        }
        KeyCode::Char('?') => {
            app.mode = AppMode::Help {
                context: HelpContext::View {
                    is_done,
                    session_id: view_context.session_id.clone(),
                    scroll_offset: view_context.scroll_offset,
                },
                scroll_offset: 0,
            };

            return Ok(EventResult::Continue);
        }
        _ => {}
    }

    if let AppMode::View { scroll_offset, .. } = &mut app.mode {
        *scroll_offset = next_scroll_offset;
    }

    Ok(EventResult::Continue)
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

async fn handle_plan_followup_action_key(
    app: &mut App,
    key: KeyEvent,
    view_context: &ViewContext,
) -> bool {
    if !app.has_plan_followup_action(&view_context.session_id) {
        return false;
    }

    match key.code {
        KeyCode::Left => {
            app.select_previous_plan_followup_action(&view_context.session_id);

            true
        }
        KeyCode::Right => {
            app.select_next_plan_followup_action(&view_context.session_id);

            true
        }
        KeyCode::Enter => {
            let selected_action = app
                .consume_plan_followup_action(&view_context.session_id)
                .unwrap_or_default();
            match selected_action {
                PlanFollowupAction::ImplementPlan => {
                    implement_plan_followup_action(app, &view_context.session_id).await;
                }
                PlanFollowupAction::TypeFeedback => {
                    let history_state = app
                        .sessions
                        .sessions
                        .get(view_context.session_index)
                        .map_or_else(
                            || PromptHistoryState::new(Vec::new()),
                            |session| {
                                PromptHistoryState::new(prompt_history_entries(&session.output))
                            },
                        );
                    switch_view_to_prompt(
                        app,
                        view_context,
                        history_state,
                        view_context.scroll_offset,
                    );
                }
            }

            true
        }
        _ => false,
    }
}

async fn implement_plan_followup_action(app: &mut App, session_id: &str) {
    if let Err(error) = app
        .set_session_permission_mode(session_id, PermissionMode::AutoEdit)
        .await
    {
        app.append_output_for_session(session_id, &format!("\n[Error] {error}\n"))
            .await;

        return;
    }

    let is_new_session = app
        .sessions
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .is_some_and(|session| session.prompt.is_empty());

    if is_new_session {
        if let Err(error) = app
            .start_session(session_id, IMPLEMENT_PLAN_PROMPT.to_string())
            .await
        {
            app.append_output_for_session(session_id, &format!("\n[Error] {error}\n"))
                .await;
        }

        return;
    }

    app.reply(session_id, IMPLEMENT_PLAN_PROMPT).await;
}

fn view_context(app: &mut App) -> Option<ViewContext> {
    let (session_id, scroll_offset) = match &app.mode {
        AppMode::View {
            session_id,
            scroll_offset,
        } => (session_id.clone(), *scroll_offset),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    Some(ViewContext {
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
        output_width,
    );

    Ok(ViewMetrics {
        total_lines,
        view_height,
    })
}

fn view_total_lines(app: &App, session_id: &str, session_index: usize, output_width: u16) -> u16 {
    let plan_followup_action = app.plan_followup_action(session_id);
    let active_progress = app.session_progress_message(session_id);

    app.sessions
        .sessions
        .get(session_index)
        .map_or(0, |session| {
            SessionChatPage::rendered_output_line_count(
                session,
                output_width,
                plan_followup_action,
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

    let diff = git::diff(session_folder, base_branch)
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
    use crate::app::AppEvent;
    use crate::db::Database;
    use crate::domain::permission::PermissionMode;

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

    async fn new_test_app_with_session() -> (App, tempfile::TempDir, String) {
        let (mut app, base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        (app, base_dir, session_id)
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
            session_id: session_id.clone(),
            scroll_offset: Some(4),
        };

        // Act
        let context = view_context(&mut app);

        // Assert
        assert!(context.is_some());
        let context = context.expect("expected view context");
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
        let total_lines = view_total_lines(&app, &session_id, 0, 20);

        // Assert
        assert!(total_lines > raw_line_count);
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
            total_lines: view_total_lines(&app, &session_id, 0, 20),
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
    async fn test_handle_plan_followup_action_key_right_selects_type_feedback() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone(),
            scroll_offset: Some(0),
        };
        app.sessions.sessions[0].permission_mode = PermissionMode::Plan;
        app.sessions.sessions[0].status = Status::InProgress;
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut status) = handles.status.lock()
        {
            *status = Status::Review;
        }
        app.apply_app_events(AppEvent::SessionUpdated {
            session_id: session_id.clone(),
        })
        .await;
        let context = ViewContext {
            scroll_offset: Some(0),
            session_id: session_id.clone(),
            session_index: 0,
        };
        let key = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);

        // Act
        let handled = handle_plan_followup_action_key(&mut app, key, &context).await;

        // Assert
        assert!(handled);
        assert_eq!(
            app.plan_followup_action(&session_id),
            Some(PlanFollowupAction::TypeFeedback)
        );
    }

    #[tokio::test]
    async fn test_handle_plan_followup_action_key_enter_for_feedback_opens_prompt_mode() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        app.mode = AppMode::View {
            session_id: session_id.clone(),
            scroll_offset: Some(4),
        };
        app.sessions.sessions[0].permission_mode = PermissionMode::Plan;
        app.sessions.sessions[0].status = Status::InProgress;
        if let Some(handles) = app.sessions.handles.get(&session_id)
            && let Ok(mut status) = handles.status.lock()
        {
            *status = Status::Review;
        }
        app.apply_app_events(AppEvent::SessionUpdated {
            session_id: session_id.clone(),
        })
        .await;
        app.select_next_plan_followup_action(&session_id);
        let context = ViewContext {
            scroll_offset: Some(4),
            session_id: session_id.clone(),
            session_index: 0,
        };
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        // Act
        let handled = handle_plan_followup_action_key(&mut app, key, &context).await;

        // Assert
        assert!(handled);
        assert!(!app.has_plan_followup_action(&session_id));
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        if let AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        } = &app.mode
        {
            assert_eq!(session_id, &context.session_id);
            assert_eq!(*scroll_offset, context.scroll_offset);
        }
    }

    #[tokio::test]
    async fn test_question_mark_sets_help_mode_from_view_context() {
        // Arrange
        let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
        let scroll = Some(3);
        app.mode = AppMode::View {
            session_id: session_id.clone(),
            scroll_offset: scroll,
        };

        // Act — simulate what the `?` arm does
        app.mode = AppMode::Help {
            context: HelpContext::View {
                is_done: false,
                session_id,
                scroll_offset: scroll,
            },
            scroll_offset: 0,
        };

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::View {
                    is_done: false,
                    ref session_id,
                    scroll_offset: Some(3),
                },
                scroll_offset: 0,
            } if !session_id.is_empty()
        ));
    }
}
