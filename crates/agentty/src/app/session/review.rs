//! Review-session replay helpers.

use std::collections::HashSet;

use super::SessionManager;
use crate::domain::session::{Session, Status};

impl SessionManager {
    /// Collects session ids that should replay persisted transcript output on
    /// the next reply after app startup.
    pub(super) fn startup_history_replay_set(sessions: &[Session]) -> HashSet<String> {
        sessions
            .iter()
            .filter(|session| session.status == Status::Review)
            .map(|session| session.id.clone())
            .collect()
    }

    /// Marks a session id for one-time transcript replay on next reply.
    pub(super) fn mark_history_replay_pending(&mut self, session_id: &str) {
        self.pending_history_replay.insert(session_id.to_string());
    }

    /// Clears one-time transcript replay tracking for a session id.
    pub(super) fn clear_history_replay_pending(&mut self, session_id: &str) {
        self.pending_history_replay.remove(session_id);
    }

    /// Returns whether a session should replay transcript output on next
    /// reply.
    pub(super) fn should_replay_history(&self, session_id: &str) -> bool {
        self.pending_history_replay.contains(session_id)
    }
}
