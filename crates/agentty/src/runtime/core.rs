//! Runtime event loop and terminal rendering orchestration.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::App;
use crate::runtime::{event, terminal};

/// Shared ratatui terminal type used by runtime helpers.
pub(crate) type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Event-loop continuation outcome after processing one input/tick cycle.
pub(crate) enum EventResult {
    /// Continue running the runtime loop.
    Continue,
    /// Exit the runtime loop and terminate the TUI session.
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
    let event_reader_pause = Arc::new(AtomicBool::new(false));
    event::spawn_event_reader(event_tx, shutdown.clone(), event_reader_pause.clone());

    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    run_main_loop(
        app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        &event_reader_pause,
    )
    .await?;

    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    terminal.show_cursor()?;

    Ok(())
}

async fn run_main_loop(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    tick: &mut tokio::time::Interval,
    event_reader_pause: &Arc<AtomicBool>,
) -> io::Result<()> {
    let mut main_loop_state = MainLoopState {
        app,
        event_reader_pause,
        event_rx,
        terminal,
        tick,
    };

    run_until_quit(&mut main_loop_state, |state| Box::pin(state.run_cycle())).await
}

/// Borrowed runtime state required to process one main-loop cycle.
struct MainLoopState<'a> {
    app: &'a mut App,
    event_reader_pause: &'a Arc<AtomicBool>,
    event_rx: &'a mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    terminal: &'a mut TuiTerminal,
    tick: &'a mut tokio::time::Interval,
}

impl MainLoopState<'_> {
    /// Runs one render/event cycle and returns the continuation result.
    async fn run_cycle(&mut self) -> io::Result<EventResult> {
        self.app.sessions.sync_from_handles();
        render_frame(self.app, self.terminal)?;

        event::process_events(
            self.app,
            self.terminal,
            self.event_rx,
            self.tick,
            self.event_reader_pause,
        )
        .await
    }
}

/// Repeats an async runtime cycle until one cycle returns `EventResult::Quit`.
async fn run_until_quit<State, CycleFn>(state: &mut State, mut cycle: CycleFn) -> io::Result<()>
where
    CycleFn: for<'state> FnMut(
        &'state mut State,
    )
        -> Pin<Box<dyn Future<Output = io::Result<EventResult>> + 'state>>,
{
    loop {
        if matches!(cycle(state).await?, EventResult::Quit) {
            break;
        }
    }

    Ok(())
}

fn render_frame(app: &mut App, terminal: &mut TuiTerminal) -> io::Result<()> {
    terminal.draw(|frame| app.draw(frame))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Test-only loop state that records call counts and scripted outcomes.
    struct TestLoopState {
        cycle_count: usize,
        results: VecDeque<io::Result<EventResult>>,
    }

    impl TestLoopState {
        /// Runs one scripted test cycle.
        fn run_cycle(&mut self) -> io::Result<EventResult> {
            self.cycle_count += 1;

            self.results
                .pop_front()
                .expect("test should provide one result per cycle")
        }
    }

    #[tokio::test]
    async fn run_until_quit_stops_after_first_quit_result() {
        // Arrange
        let mut state = TestLoopState {
            cycle_count: 0,
            results: VecDeque::from([
                Ok(EventResult::Continue),
                Ok(EventResult::Quit),
                Ok(EventResult::Continue),
            ]),
        };

        // Act
        let loop_result = run_until_quit(&mut state, |loop_state| {
            Box::pin(async move { loop_state.run_cycle() })
        })
        .await;

        // Assert
        assert!(loop_result.is_ok());
        assert_eq!(state.cycle_count, 2);
    }

    #[tokio::test]
    async fn run_until_quit_returns_cycle_error_without_extra_iterations() {
        // Arrange
        let mut state = TestLoopState {
            cycle_count: 0,
            results: VecDeque::from([Err(io::Error::other("cycle failed"))]),
        };

        // Act
        let loop_result = run_until_quit(&mut state, |loop_state| {
            Box::pin(async move { loop_state.run_cycle() })
        })
        .await;

        // Assert
        let error = loop_result.expect_err("loop should return the cycle error");
        assert_eq!(error.to_string(), "cycle failed");
        assert_eq!(state.cycle_count, 1);
    }
}
