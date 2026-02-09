use std::io;
use std::time::Duration;

use crossterm::event::Event;
use tokio::sync::mpsc;

use crate::app::App;
use crate::runtime::{EventResult, TuiTerminal, key_handler};

pub(crate) fn spawn_event_reader(event_tx: mpsc::UnboundedSender<Event>) {
    std::thread::spawn(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(250)) {
                Ok(true) => {
                    if let Ok(event) = crossterm::event::read()
                        && event_tx.send(event).is_err()
                    {
                        break;
                    }
                }
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
}

pub(crate) async fn process_events(
    app: &mut App,
    terminal: &mut TuiTerminal,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<EventResult> {
    enum LoopSignal {
        Event(Option<Event>),
        Tick,
    }

    // Wait for either a terminal event or the next tick (for redraws).
    // This yields to tokio so spawned tasks (agent output, git status) can
    // make progress on this worker thread.
    let signal = tokio::select! {
        biased;
        event = event_rx.recv() => LoopSignal::Event(event),
        _ = tick.tick() => LoopSignal::Tick,
    };
    let maybe_event = match signal {
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
