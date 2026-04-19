//! Runtime event loop and terminal rendering orchestration.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use tokio::sync::mpsc;

use crate::app::App;
use crate::runtime::{FRAME_INTERVAL, event, terminal};

/// Concrete terminal type used by the production runtime entry point.
pub(crate) type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Converts a backend-specific error into `io::Error`.
///
/// This enables generic functions to use `?` with `Terminal` methods that
/// return `Result<_, B::Error>` for any backend, including `TestBackend`
/// whose error type is `Infallible`.
pub(crate) fn backend_err<E: std::error::Error + Send + Sync + 'static>(error: E) -> io::Error {
    io::Error::other(error)
}

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
    let terminal_guard = terminal::TerminalGuard::new();
    let mut terminal = terminal::setup_terminal(&terminal_guard)?;

    // Spawn a dedicated thread for crossterm event reading so the main async
    // loop can yield to tokio between iterations.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    event::spawn_event_reader(event_tx, shutdown.clone());

    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    run_main_loop(app, &mut terminal, &mut event_rx, &mut tick).await?;

    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    terminal.show_cursor()?;

    Ok(())
}

/// Runs the TUI event/render loop with an externally provided backend and
/// event channel.
///
/// Tests use this to drive the full runtime with a `TestBackend` and injected
/// `crossterm::event::Event` values, bypassing terminal setup and the
/// background event-reader thread.
///
/// # Errors
/// Returns an error if rendering or event processing fails.
pub async fn run_with_backend<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    run_main_loop(app, terminal, event_rx, &mut tick).await
}

/// Drives the main render/event loop until quit or error.
async fn run_main_loop<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut main_loop_state = MainLoopState {
        app,
        event_rx,
        terminal,
        tick,
    };

    run_until_quit(&mut main_loop_state, |state| Box::pin(state.run_cycle())).await
}

/// Borrowed runtime state required to process one main-loop cycle.
struct MainLoopState<'a, B: Backend> {
    app: &'a mut App,
    event_rx: &'a mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    terminal: &'a mut Terminal<B>,
    tick: &'a mut tokio::time::Interval,
}

impl<B: Backend> MainLoopState<'_, B>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Runs one render/event cycle and returns the continuation result.
    ///
    /// Pending app events are reduced before draw so touched sessions refresh
    /// from their live handles without a full per-frame session sweep.
    async fn run_cycle(&mut self) -> io::Result<EventResult> {
        self.app.process_pending_app_events().await;
        render_frame(self.app, self.terminal)?;

        event::process_events(self.app, self.terminal, self.event_rx, self.tick).await
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

/// Renders one frame of the TUI application into the terminal buffer.
fn render_frame<B: Backend>(app: &mut App, terminal: &mut Terminal<B>) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    terminal
        .draw(|frame| app.draw(frame))
        .map_err(backend_err)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use tempfile::tempdir;

    use super::*;
    use crate::app::AppEvent;
    use crate::db::Database;
    use crate::domain::session::tests::SessionFixtureBuilder;
    use crate::domain::session::{SessionHandles, Status};
    use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode};

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

    /// Builds one client bundle with deterministic agent availability for
    /// test app startup.
    fn test_app_clients() -> crate::app::AppClients {
        crate::app::AppClients::new().with_agent_availability_probe(std::sync::Arc::new(
            crate::infra::agent::StaticAgentAvailabilityProbe {
                available_agent_kinds: crate::domain::agent::AgentKind::ALL.to_vec(),
            },
        ))
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

    /// Builds a test app rooted at a temporary directory.
    ///
    /// Returns both the `App` and the `TempDir` guard so the caller keeps the
    /// temporary directory alive for the full test lifetime.
    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        let app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build test app");

        (app, base_dir)
    }

    /// Flattens a test terminal buffer into one searchable string.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Verifies that `run_with_backend` drives the main loop with a
    /// `TestBackend` and exits cleanly when quit key events are injected.
    #[tokio::test]
    async fn run_with_backend_exits_on_quit_key() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        // Send `q` to open the quit confirmation, then `y` to confirm.
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send quit key");
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send confirm key");

        // Act
        let result = run_with_backend(&mut app, &mut terminal, &mut event_rx).await;

        // Assert
        assert!(
            result.is_ok(),
            "run_with_backend should exit cleanly on quit"
        );
    }

    #[tokio::test]
    /// Verifies one queued `SessionUpdated` event syncs the touched session
    /// before the next render without scanning all session handles.
    async fn run_cycle_renders_pending_session_update_before_waiting_for_events() {
        // Arrange
        let (mut app, base_dir) = new_test_app().await;
        let session_id = "session-1".to_string();
        let mut event_rx = mpsc::unbounded_channel().1;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let mut tick = tokio::time::interval(Duration::from_millis(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let session = SessionFixtureBuilder::new()
            .id(session_id.clone())
            .folder(base_dir.path().to_path_buf())
            .status(Status::InProgress)
            .build();
        app.sessions.push_session(session);
        app.sessions.handles.insert(
            session_id.clone(),
            SessionHandles::new(String::new(), Status::InProgress),
        );

        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Output,
            review_status_message: None,
            review_text: None,
            session_id: session_id.clone(),
            scroll_offset: None,
        };
        if let Some(session) = app
            .sessions
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.status = Status::InProgress;
        }
        if let Some(handles) = app.sessions.handles.get(&session_id) {
            if let Ok(mut output) = handles.output.lock() {
                output.push_str("synced output");
            }
            if let Ok(mut status) = handles.status.lock() {
                *status = Status::InProgress;
            }
        }
        app.services.emit_app_event(AppEvent::SessionUpdated {
            session_id: session_id.clone(),
        });

        let mut main_loop_state = MainLoopState {
            app: &mut app,
            event_rx: &mut event_rx,
            terminal: &mut terminal,
            tick: &mut tick,
        };

        // Act
        let cycle_result = main_loop_state.run_cycle().await;
        let rendered_text = buffer_text(terminal.backend().buffer());

        // Assert
        assert!(matches!(cycle_result, Ok(EventResult::Continue)));
        assert!(
            rendered_text.contains("synced output"),
            "expected rendered session output to contain synced handle text: {rendered_text}"
        );
    }
}
