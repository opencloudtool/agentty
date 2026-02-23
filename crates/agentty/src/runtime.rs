use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::App;

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
    terminal.draw(|frame| app.draw(frame))?;

    Ok(())
}
