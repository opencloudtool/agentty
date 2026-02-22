use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::App;
use crate::ui;

mod event;
mod key_handler;
pub mod mode;
mod terminal;

pub(crate) type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub(crate) enum EventResult {
    Continue,
    Quit,
}

/// Runs the TUI event/render loop until the user exits.
///
/// # Errors
/// Returns an error if terminal setup, rendering, or event processing fails.
pub async fn run(app: &mut App) -> io::Result<()> {
    let _terminal_guard = terminal::TerminalGuard;
    let mut terminal = terminal::setup_terminal()?;

    // Spawn a dedicated thread for crossterm event reading so the main async
    // loop can yield to tokio between iterations.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    event::spawn_event_reader(event_tx, shutdown.clone());

    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    run_main_loop(app, &mut terminal, &mut event_rx, &mut tick).await?;

    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    terminal.show_cursor()?;

    Ok(())
}

async fn run_main_loop(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<()> {
    loop {
        app.sessions.sync_from_handles();
        render_frame(app, terminal)?;

        if matches!(
            event::process_events(app, terminal, event_rx, tick).await?,
            EventResult::Quit
        ) {
            break;
        }
    }

    Ok(())
}

fn render_frame(app: &mut App, terminal: &mut TuiTerminal) -> io::Result<()> {
    let current_tab = app.tabs.current();
    let current_working_dir = app.working_dir().to_path_buf();
    let current_git_branch = app.git_branch().map(std::string::ToString::to_string);
    let current_git_status = app.git_status_info();
    let latest_available_version = app
        .latest_available_version()
        .map(std::string::ToString::to_string);
    let current_active_project_id = app.active_project_id();
    let plan_followups = app.plan_followup_snapshot();
    let session_progress_messages = app.session_progress_snapshot();
    let show_onboarding = app.should_show_onboarding();
    let mode = &app.mode;
    let projects = &app.projects;
    let (
        sessions,
        stats_activity,
        all_time_model_usage,
        longest_session_duration_seconds,
        codex_usage_limits,
        table_state,
    ) = app.sessions.render_parts();
    let settings = &mut app.settings;

    terminal.draw(|frame| {
        ui::render(
            frame,
            ui::RenderContext {
                active_project_id: current_active_project_id,
                current_tab,
                git_branch: current_git_branch.as_deref(),
                git_status: current_git_status,
                latest_available_version: latest_available_version.as_deref(),
                longest_session_duration_seconds,
                mode,
                plan_followups: &plan_followups,
                projects,
                all_time_model_usage,
                session_progress_messages: &session_progress_messages,
                settings,
                show_onboarding,
                stats_activity,
                codex_usage_limits,
                sessions,
                table_state,
                working_dir: &current_working_dir,
            },
        );
    })?;

    Ok(())
}
