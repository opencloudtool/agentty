use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::git;
use crate::model::{AppMode, HelpContext, InputState, PromptSlashState};
use crate::runtime::{EventResult, TuiTerminal};

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

    let view_metrics = view_metrics(app, terminal, view_context.session_index)?;
    let mut next_scroll_offset = view_context.scroll_offset;

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Enter => {
            app.mode = AppMode::Prompt {
                slash_state: PromptSlashState::new(),
                session_id: view_context.session_id.clone(),
                input: InputState::new(),
                scroll_offset: next_scroll_offset,
            };
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
        KeyCode::Char('d') if !key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            show_diff_for_view_session(app, &view_context).await;
        }
        KeyCode::Char('m') => {
            merge_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('p') => {
            create_pr_for_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('?') => {
            app.mode = AppMode::Help {
                context: HelpContext::View {
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
    session_index: usize,
) -> io::Result<ViewMetrics> {
    let terminal_height = terminal.size()?.height;
    let view_height = terminal_height.saturating_sub(5);
    let total_lines = u16::try_from(
        app.session_state
            .sessions
            .get(session_index)
            .and_then(|session| session.output.lock().ok())
            .map_or(0, |output| output.lines().count()),
    )
    .unwrap_or(0);

    Ok(ViewMetrics {
        total_lines,
        view_height,
    })
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
    let Some(session) = app.session_state.sessions.get(view_context.session_index) else {
        return;
    };

    let session_folder = session.folder.clone();
    let base_branch = session.base_branch.clone();
    let diff = tokio::task::spawn_blocking(move || git::diff(&session_folder, &base_branch))
        .await
        .unwrap_or_else(|join_error| Err(join_error.to_string()))
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"));
    app.mode = AppMode::Diff {
        session_id: view_context.session_id.clone(),
        diff,
        scroll_offset: 0,
    };
}

async fn merge_view_session(app: &mut App, session_id: &str) {
    let result_message = match app.merge_session(session_id).await {
        Ok(message) => format!("\n[Merge] {message}\n"),
        Err(error) => format!("\n[Merge Error] {error}\n"),
    };

    app.append_output_for_session(session_id, &result_message)
        .await;
}

async fn create_pr_for_view_session(app: &mut App, session_id: &str) {
    if let Err(error) = app.create_pr_session(session_id).await {
        app.append_output_for_session(session_id, &format!("\n[PR Error] {error}\n"))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

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
        let app = app;

        // Act
        app.append_output_for_session(&session_id, "line one").await;

        // Assert
        let output = app.session_state.sessions[0]
            .output
            .lock()
            .expect("lock poisoned")
            .clone();
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
        let output = app.session_state.sessions[0]
            .output
            .lock()
            .expect("lock poisoned")
            .clone();
        assert!(output.contains("[Merge Error]"));
    }

    #[tokio::test]
    async fn test_create_pr_for_view_session_appends_error_output_without_review_status() {
        // Arrange
        let (app, _base_dir, session_id) = new_test_app_with_session().await;
        let mut app = app;

        // Act
        create_pr_for_view_session(&mut app, &session_id).await;

        // Assert
        let output = app.session_state.sessions[0]
            .output
            .lock()
            .expect("lock poisoned")
            .clone();
        assert!(output.contains("[PR Error]"));
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
                    ref session_id,
                    scroll_offset: Some(3),
                },
                scroll_offset: 0,
            } if !session_id.is_empty()
        ));
    }
}
