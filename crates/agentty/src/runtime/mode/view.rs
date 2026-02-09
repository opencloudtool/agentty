use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::git;
use crate::model::{AppMode, InputState, PromptSlashState};
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
        KeyCode::Char('r') => {
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
        KeyCode::Char('c') => {
            commit_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('m') => {
            merge_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('p') => {
            create_pr_for_view_session(app, &view_context.session_id).await;
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
    let diff = tokio::task::spawn_blocking(move || git::diff(&session_folder))
        .await
        .unwrap_or_else(|join_error| Err(join_error.to_string()))
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"));
    app.mode = AppMode::Diff {
        session_id: view_context.session_id.clone(),
        diff,
        scroll_offset: 0,
    };
}

async fn commit_view_session(app: &mut App, session_id: &str) {
    let result_message = match app.commit_session(session_id).await {
        Ok(message) => format!("\n[Commit] {message}\n"),
        Err(error) => format!("\n[Commit Error] {error}\n"),
    };

    append_output_for_session(app, session_id, &result_message);
}

async fn merge_view_session(app: &mut App, session_id: &str) {
    let result_message = match app.merge_session(session_id).await {
        Ok(message) => format!("\n[Merge] {message}\n"),
        Err(error) => format!("\n[Merge Error] {error}\n"),
    };

    append_output_for_session(app, session_id, &result_message);
}

async fn create_pr_for_view_session(app: &mut App, session_id: &str) {
    if let Err(error) = app.create_pr_session(session_id).await {
        append_output_for_session(app, session_id, &format!("\n[PR Error] {error}\n"));
    }
}

fn append_output_for_session(app: &App, session_id: &str, output: &str) {
    let Some(session_index) = app.session_index_for_id(session_id) else {
        return;
    };
    let Some(session) = app.session_state.sessions.get(session_index) else {
        return;
    };

    session.append_output(output);
}
