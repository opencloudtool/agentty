//! App-event batch reduction helpers.

use tokio::sync::mpsc;

use super::core::AppEvent;

/// Reducer utilities for draining and coalescing queued app events.
pub(crate) struct AppEventReducer;

impl AppEventReducer {
    /// Drains the current app-event queue into one ordered vector.
    pub(crate) fn drain(
        event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
        first_event: AppEvent,
    ) -> Vec<AppEvent> {
        let mut events = vec![first_event];
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }

        events
    }
}
