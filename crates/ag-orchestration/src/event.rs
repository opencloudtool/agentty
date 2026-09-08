//! Host boundaries for campaign notifications and reconciliation scheduling.

use ag_session::SessionId;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Notification emitted by campaign reconciliation for a host to project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrchestrationEvent {
    /// Persisted campaign or child state changed and should be reloaded.
    RefreshSessions,
    /// Replace or clear the live progress of one controller session.
    ProgressUpdated {
        /// Current progress, or `None` to clear an earlier notification.
        progress: Option<String>,
        /// Controller whose live progress changed.
        session_id: SessionId,
    },
}

/// Host-owned destination for campaign notifications.
pub trait OrchestrationEventSink: Send + Sync {
    /// Delivers a notification without waiting for the frontend to process it.
    fn emit(&self, event: OrchestrationEvent);
}

impl OrchestrationEventSink for mpsc::UnboundedSender<OrchestrationEvent> {
    fn emit(&self, event: OrchestrationEvent) {
        let _ = self.send(event);
    }
}

/// Runtime-owned schedule that wakes orchestration reconciliation.
#[async_trait]
pub trait OrchestrationSchedule: Send {
    /// Waits until the coordinator should reconcile its next persisted
    /// snapshot.
    async fn wait_for_reconciliation(&mut self);
}
