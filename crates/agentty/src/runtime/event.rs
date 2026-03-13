use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{Event, KeyEvent};
use tokio::sync::mpsc;

use crate::app::{App, AppEvent};
use crate::runtime::{EventResult, TuiTerminal, key_handler, mode};
use crate::ui::state::app_mode::AppMode;

/// Reads terminal events from an underlying event backend.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait EventSource: Send + Sync + 'static {
    /// Polls for an available event.
    fn poll(&self, timeout: Duration) -> io::Result<bool>;

    /// Reads the next available event.
    fn read(&self) -> io::Result<Event>;
}

struct CrosstermEventSource;

impl EventSource for CrosstermEventSource {
    fn poll(&self, timeout: Duration) -> io::Result<bool> {
        crossterm::event::poll(timeout)
    }

    fn read(&self) -> io::Result<Event> {
        crossterm::event::read()
    }
}

/// Spawns the terminal event reader thread with production dependencies.
pub(crate) fn spawn_event_reader(
    event_tx: mpsc::UnboundedSender<Event>,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let event_source: Arc<dyn EventSource> = Arc::new(CrosstermEventSource);

    spawn_event_reader_with_source(event_source, event_tx, shutdown)
}

/// Spawns the terminal event reader with injected dependencies.
fn spawn_event_reader_with_source(
    event_source: Arc<dyn EventSource>,
    event_tx: mpsc::UnboundedSender<Event>,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            match event_source.poll(Duration::from_millis(50)) {
                Ok(true) => {
                    if let Ok(event) = event_source.read()
                        && event_tx.send(event).is_err()
                    {
                        break;
                    }
                }
                Ok(false) => {}
                Err(_) => break,
            }
        }
    })
}

/// Waits for the next terminal/app event or tick and dispatches one runtime
/// processing cycle.
pub(crate) async fn process_events(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<EventResult> {
    process_events_with_handler(app, terminal, event_rx, tick, |app, terminal, event| {
        Box::pin(process_event(app, terminal, event))
    })
    .await
}

/// Processes one event/tick cycle with an injected event handler so loop exit
/// branches can be tested without a real terminal.
async fn process_events_with_handler<Terminal, EventHandler>(
    app: &mut App,
    terminal: &mut Terminal,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    tick: &mut tokio::time::Interval,
    mut handle_event: EventHandler,
) -> io::Result<EventResult>
where
    EventHandler: for<'handler> FnMut(
        &'handler mut App,
        &'handler mut Terminal,
        Option<Event>,
    ) -> Pin<
        Box<dyn Future<Output = io::Result<EventResult>> + 'handler>,
    >,
{
    enum LoopSignal {
        AppEvent(Option<AppEvent>),
        Event(Option<Event>),
        Tick,
    }

    // Wait for either a terminal event or the next tick (for redraws).
    // This yields to tokio so spawned tasks (agent output, git status) can
    // make progress on this worker thread.
    let signal = tokio::select! {
        biased;
        event = event_rx.recv() => LoopSignal::Event(event),
        app_event = app.next_app_event() => LoopSignal::AppEvent(app_event),
        _ = tick.tick() => LoopSignal::Tick,
    };
    let maybe_event = match signal {
        LoopSignal::AppEvent(Some(event)) => {
            app.apply_app_events(event).await;
            None
        }
        LoopSignal::AppEvent(None) => None,
        LoopSignal::Event(event) => event,
        LoopSignal::Tick => {
            app.refresh_sessions_if_needed().await;
            None
        }
    };

    if matches!(
        handle_event(app, terminal, maybe_event).await?,
        EventResult::Quit
    ) {
        return Ok(EventResult::Quit);
    }

    // Drain remaining queued events before re-rendering so rapid key
    // presses are processed immediately instead of one-per-frame.
    while let Ok(event) = event_rx.try_recv() {
        if matches!(
            handle_event(app, terminal, Some(event)).await?,
            EventResult::Quit
        ) {
            return Ok(EventResult::Quit);
        }
    }

    Ok(EventResult::Continue)
}

/// Routes a single terminal event to the active mode handler.
///
/// `Event::Paste` is handled in text-input modes so multiline clipboard
/// content is inserted as text instead of interpreted as navigation keys.
async fn process_event(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event: Option<Event>,
) -> io::Result<EventResult> {
    process_event_with_key_handler(app, terminal, event, |app, terminal, key| {
        Box::pin(key_handler::handle_key_event(app, terminal, key))
    })
    .await
}

/// Routes one terminal event with an injected key handler for deterministic
/// branch tests.
async fn process_event_with_key_handler<Terminal, KeyHandler>(
    app: &mut App,
    terminal: &mut Terminal,
    event: Option<Event>,
    mut handle_key_event: KeyHandler,
) -> io::Result<EventResult>
where
    KeyHandler: for<'handler> FnMut(
        &'handler mut App,
        &'handler mut Terminal,
        KeyEvent,
    ) -> Pin<
        Box<dyn Future<Output = io::Result<EventResult>> + 'handler>,
    >,
{
    if let Some(event) = event {
        match event {
            Event::Key(key) => {
                return handle_key_event(app, terminal, key).await;
            }
            Event::Paste(pasted_text) => {
                process_paste_event(app, &pasted_text);
            }
            _ => {}
        }
    }

    Ok(EventResult::Continue)
}

/// Applies one pasted-text event to the active prompt or question input.
fn process_paste_event(app: &mut App, pasted_text: &str) {
    if matches!(&app.mode, AppMode::Prompt { .. }) {
        mode::prompt::handle_paste(app, pasted_text);
    }

    if matches!(&app.mode, AppMode::Question { .. }) {
        mode::question::handle_paste(app, pasted_text);
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use mockall::Sequence;
    use mockall::predicate::eq;
    use tempfile::tempdir;

    use super::*;
    use crate::db::Database;
    use crate::domain::input::InputState;
    use crate::infra::agent::protocol::QuestionItem;
    use crate::infra::app_server;
    use crate::ui::state::app_mode::{AppMode, QuestionFocus};
    use crate::ui::state::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};

    /// Returns a mock app-server client wrapped in `Arc` for runtime tests.
    fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
        Arc::new(app_server::MockAppServerClient::new())
    }

    /// Builds one test app rooted at a temporary directory.
    async fn new_test_app() -> App {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        App::new(
            true,
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
        )
        .await
        .expect("failed to build app")
    }

    /// Verifies the event reader forwards one queued event before stopping on
    /// a poll error.
    #[tokio::test]
    async fn test_spawn_event_reader_with_source_forwards_event_to_channel() {
        // Arrange
        let mut mock_source = MockEventSource::new();
        let mut sequence = Sequence::new();
        mock_source
            .expect_poll()
            .with(eq(Duration::from_millis(50)))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(true));
        mock_source
            .expect_read()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|| {
                Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char('x'),
                    KeyModifiers::NONE,
                )))
            });
        mock_source
            .expect_poll()
            .with(eq(Duration::from_millis(50)))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(ErrorKind::Interrupted, "stop")));
        let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        // Act
        let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
        let received_event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("failed to receive event");
        join_handle
            .join()
            .expect("failed to join event reader thread");

        // Assert
        assert!(matches!(received_event, Event::Key(_)));
    }

    /// Verifies the reader exits cleanly when the event receiver is already
    /// gone.
    #[test]
    fn test_spawn_event_reader_with_source_stops_when_receiver_is_dropped() {
        // Arrange
        let mut mock_source = MockEventSource::new();
        mock_source
            .expect_poll()
            .with(eq(Duration::from_millis(50)))
            .times(1)
            .returning(|_| Ok(true));
        mock_source.expect_read().times(1).returning(|| {
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            )))
        });
        let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        drop(event_rx);
        let shutdown = Arc::new(AtomicBool::new(false));

        // Act
        let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
        let join_result = join_handle.join();

        // Assert
        assert!(join_result.is_ok());
    }

    /// Verifies a false poll result skips reads and leaves the channel empty.
    #[test]
    fn test_spawn_event_reader_with_source_skips_read_when_poll_returns_false() {
        // Arrange
        let mut mock_source = MockEventSource::new();
        let mut sequence = Sequence::new();
        mock_source
            .expect_poll()
            .with(eq(Duration::from_millis(50)))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(false));
        mock_source
            .expect_poll()
            .with(eq(Duration::from_millis(50)))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(ErrorKind::Interrupted, "stop")));
        mock_source.expect_read().times(0);
        let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        // Act
        let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
        let join_result = join_handle.join();
        let queued_event = event_rx.try_recv();

        // Assert
        assert!(join_result.is_ok());
        assert!(queued_event.is_err());
    }

    /// Verifies a pre-set shutdown flag exits the reader without touching the
    /// event source.
    #[test]
    fn test_spawn_event_reader_with_source_exits_when_shutdown_is_already_requested() {
        // Arrange
        let mut mock_source = MockEventSource::new();
        mock_source.expect_poll().times(0);
        mock_source.expect_read().times(0);
        let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(true));

        // Act
        let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
        let join_result = join_handle.join();

        // Assert
        assert!(join_result.is_ok());
    }

    /// Verifies pasted text is routed into prompt input without invoking the
    /// key handler.
    #[tokio::test]
    async fn test_process_event_with_key_handler_pastes_into_prompt_mode() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1".to_string();
        app.mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::default(),
            input: InputState::default(),
            scroll_offset: None,
            session_id,
            slash_state: PromptSlashState::default(),
        };
        let mut terminal = ();

        // Act
        let result = process_event_with_key_handler(
            &mut app,
            &mut terminal,
            Some(Event::Paste("line 1\r\nline 2".to_string())),
            |_, (), _| Box::pin(async { Err(io::Error::other("unexpected key-handler call")) }),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(
            matches!(&app.mode, AppMode::Prompt { input, .. } if input.text() == "line 1\nline 2")
        );
    }

    /// Verifies pasted text updates question input in free-text mode.
    #[tokio::test]
    async fn test_process_event_with_key_handler_pastes_into_question_free_text_mode() {
        // Arrange — paste only works in free-text mode (`selected_option_index`
        // is `None`).
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::default(),
            scroll_offset: None,
            questions: vec![QuestionItem {
                options: vec!["yes".to_string()],
                text: "Is this enough?".to_string(),
            }],
            responses: Vec::new(),
            selected_option_index: None,
            session_id: "session-1".to_string(),
        };
        let mut terminal = ();

        // Act
        let result = process_event_with_key_handler(
            &mut app,
            &mut terminal,
            Some(Event::Paste("custom\ranswer".to_string())),
            |_, (), _| Box::pin(async { Err(io::Error::other("unexpected key-handler call")) }),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(
            &app.mode,
            AppMode::Question {
                input,
                selected_option_index: None,
                ..
            } if input.text() == "custom\nanswer"
        ));
    }

    /// Verifies non-key terminal events are ignored by the runtime handler.
    #[tokio::test]
    async fn test_process_event_with_key_handler_ignores_resize_events() {
        // Arrange
        let mut app = new_test_app().await;
        let original_mode = AppMode::List;
        app.mode = original_mode;
        let mut terminal = ();

        // Act
        let result = process_event_with_key_handler(
            &mut app,
            &mut terminal,
            Some(Event::Resize(120, 40)),
            |_, (), _| Box::pin(async { Err(io::Error::other("unexpected key-handler call")) }),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(&app.mode, AppMode::List));
    }

    /// Verifies handler errors terminate the outer event-processing cycle.
    #[tokio::test]
    async fn test_process_events_with_handler_returns_handler_error() {
        // Arrange
        let mut app = new_test_app().await;
        let mut terminal = ();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            )))
            .expect("failed to queue event");
        let mut tick = tokio::time::interval(Duration::from_mins(1));

        // Act
        let result = process_events_with_handler(
            &mut app,
            &mut terminal,
            &mut event_rx,
            &mut tick,
            |_, (), _| Box::pin(async { Err(io::Error::other("handler failed")) }),
        )
        .await;

        // Assert
        assert!(result.is_err());
        let error = result
            .err()
            .expect("handler error should exit the event loop");
        assert_eq!(error.to_string(), "handler failed");
    }
}
