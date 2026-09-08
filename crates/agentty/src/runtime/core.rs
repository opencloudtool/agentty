//! Runtime event loop and terminal rendering orchestration.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ag_orchestration::{OrchestrationCoordinator, OrchestrationSchedule};
use async_trait::async_trait;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use tokio::sync::mpsc;

use crate::app::App;
use crate::infra::clock::Clock;
use crate::runtime::{FRAME_INTERVAL, PresentationState, event, terminal};

/// Fallback redraw cadence for visible spinner and timer UI when no new
/// events arrive.
const FORCED_REDRAW_INTERVAL: Duration = Duration::from_millis(200);
/// Coordinator polling cadence while the terminal runtime is active.
const ORCHESTRATION_RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

/// Tokio-backed production schedule for orchestration reconciliation.
struct RuntimeOrchestrationSchedule {
    interval: tokio::time::Interval,
}

impl RuntimeOrchestrationSchedule {
    fn new() -> Self {
        let mut interval = tokio::time::interval(ORCHESTRATION_RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        Self { interval }
    }
}

#[async_trait]
impl OrchestrationSchedule for RuntimeOrchestrationSchedule {
    async fn wait_for_reconciliation(&mut self) {
        self.interval.tick().await;
    }
}

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

/// Owns the blocking terminal-reader thread and its shutdown signal.
struct EventReaderTask {
    join_handle: std::thread::JoinHandle<()>,
    shutdown: Arc<AtomicBool>,
}

impl EventReaderTask {
    /// Starts the production terminal reader for `event_tx`.
    fn spawn(event_tx: mpsc::UnboundedSender<io::Result<crossterm::event::Event>>) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let join_handle = event::spawn_event_reader(event_tx, Arc::clone(&shutdown));

        Self {
            join_handle,
            shutdown,
        }
    }

    /// Requests reader shutdown and waits for the blocking thread to finish.
    async fn shutdown(self) -> io::Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        let join_result = tokio::task::spawn_blocking(move || self.join_handle.join()).await;

        Self::map_join_result(join_result)
    }

    /// Maps the blocking join task and reader thread outcomes into one runtime
    /// error surface.
    fn map_join_result(
        join_result: Result<std::thread::Result<()>, tokio::task::JoinError>,
    ) -> io::Result<()> {
        let reader_result = join_result.map_err(|error| {
            io::Error::other(format!("failed to join event reader task: {error}"))
        })?;

        reader_result.map_err(|_| io::Error::other("terminal event reader panicked"))
    }
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
    let event_reader_task = EventReaderTask::spawn(event_tx);

    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let run_result = run_main_loop(app, &mut terminal, &mut event_rx, &mut tick).await;
    let reader_shutdown_result = event_reader_task.shutdown().await;
    app.wait_for_background_cleanup_tasks().await;
    let cursor_result = terminal.show_cursor().map_err(backend_err);

    run_result.and(reader_shutdown_result).and(cursor_result)
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

    let run_result = run_main_loop(app, terminal, event_rx, &mut tick).await;
    app.wait_for_background_cleanup_tasks().await;

    run_result
}

/// Drives the main render/event loop until quit or error.
///
/// Reads the runtime clock from `app.services.clock()` so render-throttle
/// timing is sourced through the same `Clock` trait used by session refresh
/// logic, keeping `runtime` free of direct `Instant::now()` calls.
async fn run_main_loop<B: Backend, Message: event::TerminalEventMessage>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<Message>,
    tick: &mut tokio::time::Interval,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let _session_runtime_consumer = app.sessions.foreground_consumer();
    let orchestration_coordinator = OrchestrationCoordinator::new(
        Arc::new(app.services.clone()),
        app.services.db().orchestration_repository(),
        app.coordinator_session_service(),
    );
    let orchestration_task =
        tokio::spawn(orchestration_coordinator.run(RuntimeOrchestrationSchedule::new()));
    let clock = app.services.clock();
    let last_draw_at = clock.now_instant();
    let mut main_loop_state = MainLoopState {
        app,
        clock,
        event_rx,
        last_draw_at,
        presentation: Rc::new(PresentationState::default()),
        terminal,
        tick,
    };

    let result = run_until_quit(&mut main_loop_state, |state| Box::pin(state.run_cycle())).await;
    let orchestration_shutdown_result = stop_orchestration_task(orchestration_task).await;

    result.and(orchestration_shutdown_result)
}

/// Cancels the coordinator and observes its final task result.
async fn stop_orchestration_task(
    orchestration_task: tokio::task::JoinHandle<()>,
) -> io::Result<()> {
    orchestration_task.abort();

    match orchestration_task.await {
        Ok(()) => Ok(()),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(io::Error::other(format!(
            "orchestration coordinator task failed: {error}"
        ))),
    }
}

/// Borrowed runtime state required to process one main-loop cycle.
struct MainLoopState<'a, B: Backend, Message> {
    app: &'a mut App,
    clock: Arc<dyn Clock>,
    event_rx: &'a mut mpsc::UnboundedReceiver<Message>,
    last_draw_at: Instant,
    presentation: Rc<PresentationState>,
    terminal: &'a mut Terminal<B>,
    tick: &'a mut tokio::time::Interval,
}

impl<B: Backend, Message: event::TerminalEventMessage> MainLoopState<'_, B, Message>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Runs one render/event cycle and returns the continuation result.
    ///
    /// Pending app events are reduced before draw so touched sessions refresh
    /// from their live handles without a full per-frame session sweep. The open
    /// session view then reconciles into the clarification panel when its
    /// session has reached `Status::Question`, covering cases where the live
    /// `AgentResponseReceived` projection did not flip the view.
    async fn run_cycle(&mut self) -> io::Result<EventResult> {
        self.app.process_pending_app_events().await;
        self.app.reconcile_open_session_question_mode().await;
        self.app
            .expire_project_sync_status(self.clock.now_instant());
        render_frame(
            self.app,
            self.terminal,
            self.clock.as_ref(),
            &mut self.last_draw_at,
            self.presentation.as_ref(),
        )?;

        event::process_events(
            self.app,
            Rc::clone(&self.presentation),
            self.terminal,
            self.event_rx,
            self.tick,
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

/// Renders one frame of the TUI application into the terminal buffer.
///
/// Idle redraws are skipped unless the app explicitly requested a fresh frame
/// or one visible spinner/timer has reached the forced redraw cadence. Both
/// the elapsed-time comparison and the `last_draw_at` stamp read through the
/// injected `Clock` so test runs can virtualize the render-throttle clock
/// without mutating production timing behavior.
fn render_frame<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    clock: &dyn Clock,
    last_draw_at: &mut Instant,
    presentation: &PresentationState,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let forced_redraw_due =
        app.has_visible_tick_driven_ui() && forced_redraw_elapsed(clock, *last_draw_at);
    if !app.needs_redraw() && !forced_redraw_due {
        return Ok(());
    }

    let snapshot = app.view_snapshot();
    if presentation.terminal_clear_needed(&snapshot) {
        clear_terminal_for_surface_change(terminal)?;
    }
    terminal
        .draw(|frame| {
            presentation.render(&snapshot, frame);
        })
        .map_err(backend_err)?;
    presentation.record_rendered_surface(&snapshot);
    app.clear_redraw();
    *last_draw_at = clock.now_instant();

    Ok(())
}

/// Clears the fullscreen backend and invalidates Ratatui's previous frame.
///
/// `Terminal::clear()` snapshots the cursor through an interactive terminal
/// query. Agentty renders in the alternate fullscreen buffer, so clearing the
/// full backend directly preserves the cursor while avoiding that query. The
/// extra buffer swap resets Ratatui's previous frame and forces the following
/// draw to repaint every rendered cell.
fn clear_terminal_for_surface_change<B: Backend>(terminal: &mut Terminal<B>) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    terminal
        .backend_mut()
        .clear_region(ClearType::All)
        .map_err(backend_err)?;
    terminal.swap_buffers();

    Ok(())
}

/// Returns whether the injected clock has advanced past `last_draw_at` by at
/// least `FORCED_REDRAW_INTERVAL`.
fn forced_redraw_elapsed(clock: &dyn Clock, last_draw_at: Instant) -> bool {
    clock.now_instant().saturating_duration_since(last_draw_at) >= FORCED_REDRAW_INTERVAL
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::{Backend, TestBackend, WindowSize};
    use ratatui::buffer::Cell;
    use ratatui::layout::{Position, Size};
    use testty::session::PtySessionBuilder;

    use super::*;
    use crate::app::AppEvent;
    use crate::domain::session::{SessionHandles, Status};
    use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
    use crate::presentation::app_mode::AppMode;
    use crate::test_support::SessionFixtureBuilder;

    /// Environment marker used to distinguish the nested PTY test process.
    const PRODUCTION_RUN_CHILD_ENV: &str = "AGENTTY_TEST_PRODUCTION_RUN_CHILD";
    /// Fully-qualified libtest name invoked inside the nested PTY.
    const PRODUCTION_RUN_CHILD_TEST: &str =
        "runtime::core::tests::production_run_exits_cleanly_in_pty_child";

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

    /// Drives the concrete terminal entrypoint in a PTY so its setup, event
    /// reader, and ordered cleanup path remain covered by source tests.
    #[test]
    fn production_run_exits_cleanly_in_pty() {
        // Arrange
        let test_binary = std::env::current_exe().expect("test binary path should be available");
        let mut session = PtySessionBuilder::new(test_binary)
            .args(["--exact", PRODUCTION_RUN_CHILD_TEST, "--nocapture"])
            .env(PRODUCTION_RUN_CHILD_ENV, "1")
            .spawn()
            .expect("failed to spawn production runtime test in a PTY");
        session
            .wait_for_text("Sessions", Duration::from_secs(10))
            .expect("production runtime should render its initial list view");

        // Act
        session
            .press_key("q")
            .expect("failed to open quit confirmation");
        session
            .wait_for_text("Confirm Quit", Duration::from_secs(5))
            .expect("quit confirmation should be rendered");
        session
            .press_key("y")
            .expect("failed to confirm runtime exit");
        let exited_successfully = session
            .wait_for_exit(Duration::from_secs(5))
            .expect("production runtime should exit before the timeout");

        // Assert
        assert!(exited_successfully);
    }

    /// Runs only when re-invoked by `production_run_exits_cleanly_in_pty`.
    #[tokio::test]
    async fn production_run_exits_cleanly_in_pty_child() {
        // Arrange
        if std::env::var_os(PRODUCTION_RUN_CHILD_ENV).is_none() {
            return;
        }
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

        // Act
        let result = tokio::time::timeout(Duration::from_secs(10), run(&mut app))
            .await
            .expect("production runtime should not hang");

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn event_reader_task_spawn_starts_a_shutdown_capable_reader() {
        // Arrange
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        // Act
        let result = EventReaderTask::spawn(event_tx).shutdown().await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn event_reader_task_shutdown_signals_and_joins_reader() {
        // Arrange
        let shutdown = Arc::new(AtomicBool::new(false));
        let reader_shutdown = Arc::clone(&shutdown);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let join_handle = std::thread::spawn(move || {
            started_tx
                .send(())
                .expect("reader should signal that it started");
            while !reader_shutdown.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
        });
        started_rx
            .recv()
            .expect("reader should start before shutdown is requested");
        let task = EventReaderTask {
            join_handle,
            shutdown,
        };

        // Act
        let result = task.shutdown().await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn event_reader_task_shutdown_reports_reader_panic() {
        // Arrange
        let shutdown = Arc::new(AtomicBool::new(false));
        let join_handle = std::thread::spawn(|| {
            std::panic::resume_unwind(Box::new("reader panic"));
        });
        let task = EventReaderTask {
            join_handle,
            shutdown,
        };

        // Act
        let result = task.shutdown().await;

        // Assert
        let error = result.expect_err("reader panic should be reported");
        assert_eq!(error.to_string(), "terminal event reader panicked");
    }

    #[tokio::test]
    async fn event_reader_task_maps_blocking_join_failure() {
        // Arrange
        let join_error = tokio::spawn(async {
            std::panic::resume_unwind(Box::new("blocking join panic"));
        })
        .await
        .expect_err("panicked task should return a join error");

        // Act
        let result = EventReaderTask::map_join_result(Err(join_error));

        // Assert
        let error = result.expect_err("blocking join failure should be reported");
        assert!(
            error
                .to_string()
                .starts_with("failed to join event reader task:")
        );
    }

    #[tokio::test]
    async fn stop_orchestration_task_accepts_completed_task() {
        // Arrange
        let task = tokio::spawn(async {});
        tokio::task::yield_now().await;

        // Act
        let result = stop_orchestration_task(task).await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stop_orchestration_task_accepts_cancelled_task() {
        // Arrange
        let task = tokio::spawn(std::future::pending::<()>());

        // Act
        let result = stop_orchestration_task(task).await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stop_orchestration_task_reports_panicked_task() {
        // Arrange
        let task = tokio::spawn(async {
            std::panic::resume_unwind(Box::new("coordinator panic"));
        });
        tokio::task::yield_now().await;

        // Act
        let result = stop_orchestration_task(task).await;

        // Assert
        let error = result.expect_err("coordinator panic should be reported");
        assert!(
            error
                .to_string()
                .starts_with("orchestration coordinator task failed:")
        );
    }

    /// Flattens a test terminal buffer into one searchable string.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Test backend wrapper that counts terminal clears and draw flushes.
    struct CountingBackend {
        clear_count: Arc<AtomicUsize>,
        draw_count: Arc<AtomicUsize>,
        inner: TestBackend,
    }

    impl CountingBackend {
        /// Creates a counting wrapper around one `TestBackend`.
        fn new(width: u16, height: u16) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let clear_count = Arc::new(AtomicUsize::new(0));
            let draw_count = Arc::new(AtomicUsize::new(0));

            (
                Self {
                    clear_count: Arc::clone(&clear_count),
                    draw_count: Arc::clone(&draw_count),
                    inner: TestBackend::new(width, height),
                },
                clear_count,
                draw_count,
            )
        }
    }

    impl Backend for CountingBackend {
        type Error = Infallible;

        fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.draw_count.fetch_add(1, Ordering::Relaxed);
            self.inner.draw(content)
        }

        fn hide_cursor(&mut self) -> Result<(), Self::Error> {
            self.inner.hide_cursor()
        }

        fn show_cursor(&mut self) -> Result<(), Self::Error> {
            self.inner.show_cursor()
        }

        fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
            self.inner.get_cursor_position()
        }

        fn set_cursor_position<P: Into<Position>>(
            &mut self,
            position: P,
        ) -> Result<(), Self::Error> {
            self.inner.set_cursor_position(position)
        }

        fn clear(&mut self) -> Result<(), Self::Error> {
            self.inner.clear()
        }

        fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
            self.clear_count.fetch_add(1, Ordering::Relaxed);
            self.inner.clear_region(clear_type)
        }

        fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
            self.inner.append_lines(n)
        }

        fn size(&self) -> Result<Size, Self::Error> {
            self.inner.size()
        }

        fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
            self.inner.window_size()
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            self.inner.flush()
        }
    }

    /// Verifies base-page transitions clear stale terminal cells while
    /// redraws within one page keep Ratatui's normal diff rendering.
    #[tokio::test]
    async fn render_frame_clears_terminal_only_when_base_page_changes() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let (backend, clear_count, _draw_count) = CountingBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let clock = crate::infra::clock::RealClock;
        let mut last_draw_at = clock.now_instant();
        let presentation = PresentationState::default();

        // Act
        render_frame(
            &mut app,
            &mut terminal,
            &clock,
            &mut last_draw_at,
            &presentation,
        )
        .expect("initial list frame should render");
        app.mode = AppMode::View {
            session_id: "missing-session".into(),
            scroll_offset: None,
        };
        app.mark_dirty();
        let repeated_clear_decisions = {
            let snapshot = app.view_snapshot();

            [
                presentation.terminal_clear_needed(&snapshot),
                presentation.terminal_clear_needed(&snapshot),
            ]
        };
        render_frame(
            &mut app,
            &mut terminal,
            &clock,
            &mut last_draw_at,
            &presentation,
        )
        .expect("session frame should render");
        app.mark_dirty();
        render_frame(
            &mut app,
            &mut terminal,
            &clock,
            &mut last_draw_at,
            &presentation,
        )
        .expect("same session page should redraw");

        // Assert
        assert_eq!(repeated_clear_decisions, [true, true]);
        assert_eq!(clear_count.load(Ordering::Relaxed), 1);
    }

    /// Verifies that `run_with_backend` drives the main loop with a
    /// `TestBackend` and exits cleanly when quit key events are injected.
    #[tokio::test]
    async fn run_with_backend_exits_on_quit_key() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
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

    /// Verifies a dropped injected event sender exits the full runtime loop
    /// instead of repeatedly producing empty input cycles.
    #[tokio::test]
    async fn run_with_backend_returns_error_when_event_channel_closes() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        drop(event_tx);

        // Act
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_with_backend(&mut app, &mut terminal, &mut event_rx),
        )
        .await
        .expect("runtime should exit when its event channel closes");

        // Assert
        let error = result.expect_err("closed event channel should fail the runtime");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(error.to_string(), "terminal event reader stopped");
    }

    #[tokio::test]
    async fn run_with_backend_waits_for_cleanup_tasks_after_quit() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (release_tx, mut release_rx) = mpsc::unbounded_channel::<()>();
        let (done_tx, mut done_rx) = mpsc::unbounded_channel::<()>();
        app.services.track_cleanup_task(tokio::spawn(async move {
            let _ = release_rx.recv().await;
            let _ = done_tx.send(());
        }));

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
        let run_future = run_with_backend(&mut app, &mut terminal, &mut event_rx);
        tokio::pin!(run_future);

        // Act / Assert — the runtime should not complete while the tracked
        // cleanup task is still pending.
        tokio::select! {
            result = &mut run_future => {
                unreachable!("runtime exited before cleanup task finished: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        // Act
        release_tx.send(()).expect("failed to release cleanup task");
        let result = run_future.await;

        // Assert
        assert!(
            result.is_ok(),
            "run_with_backend should exit cleanly after cleanup"
        );
        assert!(done_rx.try_recv().is_ok());
    }

    #[tokio::test]
    /// Verifies idle `run_with_backend` redraws stay throttled when no visible
    /// spinner or timer is active.
    async fn run_with_backend_skips_idle_redraws_without_tick_driven_ui() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let (backend, _clear_count, draw_count) = CountingBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let run_future = run_with_backend(&mut app, &mut terminal, &mut event_rx);
        tokio::pin!(run_future);

        // Act
        tokio::select! {
            result = &mut run_future => {
                unreachable!("runtime exited before idle window elapsed: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(1100)) => {}
        }
        let idle_draw_count = draw_count.load(Ordering::Relaxed);
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
        let result = run_future.await;

        // Assert
        assert!(
            result.is_ok(),
            "run_with_backend should exit cleanly on quit"
        );
        assert!(
            idle_draw_count <= 2,
            "expected at most two idle draws per second, observed {idle_draw_count}"
        );
    }

    #[tokio::test]
    /// Verifies one queued `SessionUpdated` event syncs the touched session
    /// before the next render without scanning all session handles.
    async fn run_cycle_renders_pending_session_update_before_waiting_for_events() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_test_app().await;
        let session_id = "session-1".to_string();
        let (_event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
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
        app.sessions.session_handles_mut().insert(
            session_id.clone().into(),
            SessionHandles::new(Status::InProgress),
        );

        app.mode = AppMode::View {
            session_id: session_id.clone().into(),
            scroll_offset: None,
        };
        if let Some(session) = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.status = Status::InProgress;
        }
        if let Some(handles) = app.sessions.session_handles().get(session_id.as_str()) {
            let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
                0,
                SessionMessageKind::AssistantAnswer,
                "synced output",
            )]);
            if let Ok(mut handle_transcript) = handles.transcript.lock() {
                *handle_transcript = transcript;
            }
            if let Ok(mut status) = handles.status.lock() {
                *status = Status::InProgress;
            }
        }
        app.services.emit_app_event(AppEvent::SessionUpdated {
            session_id: session_id.clone().into(),
            version: 1,
        });

        let clock: Arc<dyn Clock> = Arc::new(crate::infra::clock::RealClock);
        let last_draw_at = clock.now_instant();
        let mut main_loop_state = MainLoopState {
            app: &mut app,
            clock,
            event_rx: &mut event_rx,
            last_draw_at,
            presentation: Rc::new(PresentationState::default()),
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

    /// Stationary clock whose `now_instant()` value never advances.
    ///
    /// Used to verify that `forced_redraw_elapsed` reads elapsed time through
    /// the injected `Clock` rather than the host wall clock.
    struct FrozenInstantClock {
        instant: std::time::Instant,
    }

    impl Clock for FrozenInstantClock {
        fn now_instant(&self) -> std::time::Instant {
            self.instant
        }

        fn now_system_time(&self) -> std::time::SystemTime {
            std::time::SystemTime::now()
        }
    }

    /// Verifies the forced-redraw cadence reads elapsed time through the
    /// injected `Clock`. The frozen clock and `last_draw_at` are anchored to
    /// the same host instant, while host wall time has already drifted past
    /// `FORCED_REDRAW_INTERVAL` since the anchor instant. A correct
    /// implementation reports zero virtual elapsed time and skips the draw.
    #[test]
    fn forced_redraw_elapsed_uses_injected_clock_not_host_time() {
        // Arrange
        let anchor = std::time::Instant::now()
            .checked_sub(FORCED_REDRAW_INTERVAL * 10)
            .expect("anchor instant should fit before host now");
        let frozen = FrozenInstantClock { instant: anchor };

        // Act
        let elapsed = forced_redraw_elapsed(&frozen, anchor);

        // Assert
        assert!(
            !elapsed,
            "forced-redraw cadence must read elapsed time through the injected clock, not the \
             host wall clock"
        );
    }

    /// Verifies the forced-redraw cadence fires once the injected clock has
    /// advanced past `FORCED_REDRAW_INTERVAL`.
    #[test]
    fn forced_redraw_elapsed_fires_after_injected_clock_advances() {
        // Arrange
        let now = std::time::Instant::now();
        let last_draw_at = now
            .checked_sub(FORCED_REDRAW_INTERVAL)
            .expect("last_draw_at should fit before now");
        let frozen = FrozenInstantClock { instant: now };

        // Act
        let elapsed = forced_redraw_elapsed(&frozen, last_draw_at);

        // Assert
        assert!(
            elapsed,
            "forced-redraw cadence should trigger once the injected clock has advanced past \
             FORCED_REDRAW_INTERVAL"
        );
    }
}
