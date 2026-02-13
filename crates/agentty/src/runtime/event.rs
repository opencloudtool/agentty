use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::Event;
#[cfg(test)]
use mockall::{Sequence, automock, predicate::eq};
use tokio::sync::mpsc;

use crate::app::{AgentEvent, App};
use crate::runtime::{EventResult, TuiTerminal, key_handler};

/// Reads terminal events from an underlying event backend.
#[cfg_attr(test, automock)]
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

pub(crate) fn spawn_event_reader(event_tx: mpsc::UnboundedSender<Event>) {
    let event_source: Arc<dyn EventSource> = Arc::new(CrosstermEventSource);

    let _event_reader = spawn_event_reader_with_source(event_source, event_tx);
}

fn spawn_event_reader_with_source(
    event_source: Arc<dyn EventSource>,
    event_tx: mpsc::UnboundedSender<Event>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            match event_source.poll(Duration::from_millis(250)) {
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
    agent_rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
    tick: &mut tokio::time::Interval,
) -> io::Result<EventResult> {
    enum LoopSignal {
        Event(Option<Event>),
        Agent(Option<AgentEvent>),
        Tick,
    }

    // Wait for either a terminal event or the next tick (for redraws).
    // This yields to tokio so spawned tasks (agent output, git status) can
    // make progress on this worker thread.
    let signal = tokio::select! {
        biased;
        event = event_rx.recv() => LoopSignal::Event(event),
        agent = agent_rx.recv() => LoopSignal::Agent(agent),
        _ = tick.tick() => LoopSignal::Tick,
    };
    match signal {
        LoopSignal::Event(event) => {
            if matches!(
                process_event(app, terminal, event).await?,
                EventResult::Quit
            ) {
                return Ok(EventResult::Quit);
            }
        }
        LoopSignal::Agent(event) => {
            if let Some(event) = event {
                handle_agent_event(app, event).await;
            }
        }
        LoopSignal::Tick => {
            app.refresh_sessions_if_needed().await;
        }
    }

    // Drain remaining queued agent events
    while let Ok(event) = agent_rx.try_recv() {
        handle_agent_event(app, event).await;
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

async fn handle_agent_event(app: &App, event: AgentEvent) {
    match event {
        AgentEvent::Output { session_id, text } => {
            app.append_output_for_session(&session_id, &text).await;
        }
        AgentEvent::Finished {
            session_id,
            input_tokens,
            output_tokens,
        } => {
            app.finish_session_turn(&session_id, input_tokens, output_tokens)
                .await;
        }
        AgentEvent::Error { session_id, error } => {
            app.fail_session_turn(&session_id, &error).await;
        }
    }
}

async fn process_event(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event: Option<Event>,
) -> io::Result<EventResult> {
    if let Some(Event::Key(key)) = event {
        return key_handler::handle_key_event(app, terminal, key).await;
    }

    Ok(EventResult::Continue)
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    #[tokio::test]
    async fn test_spawn_event_reader_with_source_forwards_event_to_channel() {
        // Arrange
        let mut mock_source = MockEventSource::new();
        let mut sequence = Sequence::new();
        mock_source
            .expect_poll()
            .with(eq(Duration::from_millis(250)))
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
            .with(eq(Duration::from_millis(250)))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(ErrorKind::Interrupted, "stop")));
        let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        // Act
        let join_handle = spawn_event_reader_with_source(event_source, event_tx);
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
            .with(eq(Duration::from_millis(250)))
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

        // Act
        let join_handle = spawn_event_reader_with_source(event_source, event_tx);
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
            .with(eq(Duration::from_millis(250)))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(false));
        mock_source
            .expect_poll()
            .with(eq(Duration::from_millis(250)))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(ErrorKind::Interrupted, "stop")));
        mock_source.expect_read().times(0);
        let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        // Act
        let join_handle = spawn_event_reader_with_source(event_source, event_tx);
        let join_result = join_handle.join();
        let queued_event = event_rx.try_recv();

        // Assert
        assert!(join_result.is_ok());
        assert!(queued_event.is_err());
    }
}
