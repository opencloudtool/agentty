use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crossterm::event::Event;
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

pub(crate) fn spawn_event_reader(
    event_tx: mpsc::UnboundedSender<Event>,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let event_source: Arc<dyn EventSource> = Arc::new(CrosstermEventSource);

    spawn_event_reader_with_source(event_source, event_tx, shutdown)
}

fn spawn_event_reader_with_source(
    event_source: Arc<dyn EventSource>,
    event_tx: mpsc::UnboundedSender<Event>,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
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

pub(crate) async fn process_events(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<EventResult> {
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
        process_event(app, terminal, maybe_event).await?,
        EventResult::Quit
    ) {
        return Ok(EventResult::Quit);
    }

    // Drain remaining queued events before re-rendering so rapid key
    // presses are processed immediately instead of one-per-frame.
    while let Ok(event) = event_rx.try_recv() {
        if matches!(
            process_event(app, terminal, Some(event)).await?,
            EventResult::Quit
        ) {
            return Ok(EventResult::Quit);
        }
    }

    Ok(EventResult::Continue)
}

/// Routes a single terminal event to the active mode handler.
///
/// `Event::Paste` is handled only in prompt mode so multiline clipboard
/// content is inserted as text instead of interpreted as navigation keys.
async fn process_event(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event: Option<Event>,
) -> io::Result<EventResult> {
    if let Some(event) = event {
        match event {
            Event::Key(key) => {
                return key_handler::handle_key_event(app, terminal, key).await;
            }
            Event::Paste(pasted_text) => {
                if matches!(&app.mode, AppMode::Prompt { .. }) {
                    mode::prompt::handle_paste(app, &pasted_text);
                }
            }
            _ => {}
        }
    }

    Ok(EventResult::Continue)
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use mockall::Sequence;
    use mockall::predicate::eq;

    use super::*;

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
}
