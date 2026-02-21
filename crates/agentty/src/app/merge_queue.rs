//! App-level merge queue state and transition rules.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;

/// Queue progression outcome after applying a status update batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MergeQueueProgress {
    /// No additional merge should be started.
    NoAction,
    /// Queue state allows starting the next queued merge.
    StartNext,
}

/// Stores FIFO merge queue state and active merge tracking.
#[derive(Default)]
pub(crate) struct MergeQueue {
    active_session_id: Option<String>,
    queued_session_ids: VecDeque<String>,
}

impl MergeQueue {
    /// Returns whether a session is already active or pending in the queue.
    pub(crate) fn is_queued_or_active(&self, session_id: &str) -> bool {
        if self.active_session_id.as_deref() == Some(session_id) {
            return true;
        }

        self.queued_session_ids
            .iter()
            .any(|queued_session_id| queued_session_id == session_id)
    }

    /// Adds one session to the end of the FIFO queue.
    pub(crate) fn enqueue(&mut self, session_id: String) {
        self.queued_session_ids.push_back(session_id);
    }

    /// Returns whether a merge is currently active.
    pub(crate) fn has_active(&self) -> bool {
        self.active_session_id.is_some()
    }

    /// Pops the next queued session id from the queue head.
    pub(crate) fn pop_next(&mut self) -> Option<String> {
        self.queued_session_ids.pop_front()
    }

    /// Marks a session id as the active merge.
    pub(crate) fn set_active(&mut self, session_id: String) {
        self.active_session_id = Some(session_id);
    }

    /// Resolves queue progression for one reduced app-event batch.
    ///
    /// This clears an active merge once it transitions away from `Merging`.
    /// It returns `StartNext` when the active merge either:
    /// - Transitions from `Merging` to any other state, or
    /// - Disappears from the session list after processing.
    pub(crate) fn progress_from_status_updates(
        &mut self,
        current_active_status: Option<Status>,
        session_ids: &HashSet<String>,
        previous_session_states: &HashMap<String, (PermissionMode, Status)>,
    ) -> MergeQueueProgress {
        let Some(active_session_id) = self.active_session_id.clone() else {
            return MergeQueueProgress::NoAction;
        };
        if !session_ids.contains(&active_session_id) {
            if current_active_status.is_none() {
                self.active_session_id = None;

                return MergeQueueProgress::StartNext;
            }

            return MergeQueueProgress::NoAction;
        }

        let previous_status = previous_session_states
            .get(&active_session_id)
            .map(|(_, previous_status)| *previous_status);
        if previous_status != Some(Status::Merging) {
            return MergeQueueProgress::NoAction;
        }

        if current_active_status == Some(Status::Merging) {
            return MergeQueueProgress::NoAction;
        }

        self.active_session_id = None;

        MergeQueueProgress::StartNext
    }

    /// Returns the currently active merge session id, if any.
    pub(crate) fn active_session_id(&self) -> Option<&str> {
        self.active_session_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn previous_states_for(
        session_id: &str,
        status: Status,
    ) -> HashMap<String, (PermissionMode, Status)> {
        HashMap::from([(session_id.to_string(), (PermissionMode::AutoEdit, status))])
    }

    fn touched_session_ids(session_id: &str) -> HashSet<String> {
        HashSet::from([session_id.to_string()])
    }

    #[test]
    fn test_is_queued_or_active() {
        // Arrange
        let mut queue = MergeQueue::default();
        queue.enqueue("queued".to_string());
        queue.set_active("active".to_string());

        // Act & Assert
        assert!(queue.is_queued_or_active("queued"));
        assert!(queue.is_queued_or_active("active"));
        assert!(!queue.is_queued_or_active("missing"));
    }

    #[test]
    fn test_pop_next_follows_fifo_order() {
        // Arrange
        let mut queue = MergeQueue::default();
        queue.enqueue("session-a".to_string());
        queue.enqueue("session-b".to_string());

        // Act
        let first = queue.pop_next();
        let second = queue.pop_next();
        let third = queue.pop_next();

        // Assert
        assert_eq!(first.as_deref(), Some("session-a"));
        assert_eq!(second.as_deref(), Some("session-b"));
        assert_eq!(third, None);
    }

    #[test]
    fn test_progress_from_status_updates_done_starts_next_and_clears_active() {
        // Arrange
        let session_id = "session-1";
        let mut queue = MergeQueue::default();
        queue.set_active(session_id.to_string());
        let touched_ids = touched_session_ids(session_id);
        let previous_states = previous_states_for(session_id, Status::Merging);

        // Act
        let progress =
            queue.progress_from_status_updates(Some(Status::Done), &touched_ids, &previous_states);

        // Assert
        assert_eq!(progress, MergeQueueProgress::StartNext);
        assert!(!queue.has_active());
    }

    #[test]
    fn test_progress_from_status_updates_failure_starts_next_and_clears_active() {
        // Arrange
        let session_id = "session-1";
        let mut queue = MergeQueue::default();
        queue.set_active(session_id.to_string());
        let touched_ids = touched_session_ids(session_id);
        let previous_states = previous_states_for(session_id, Status::Merging);

        // Act
        let progress = queue.progress_from_status_updates(
            Some(Status::Review),
            &touched_ids,
            &previous_states,
        );

        // Assert
        assert_eq!(progress, MergeQueueProgress::StartNext);
        assert!(!queue.has_active());
    }

    #[test]
    fn test_progress_from_status_updates_missing_session_starts_next() {
        // Arrange
        let session_id = "session-1";
        let mut queue = MergeQueue::default();
        queue.set_active(session_id.to_string());
        let touched_ids = HashSet::new();
        let previous_states = HashMap::new();

        // Act
        let progress = queue.progress_from_status_updates(None, &touched_ids, &previous_states);

        // Assert
        assert_eq!(progress, MergeQueueProgress::StartNext);
        assert!(!queue.has_active());
    }

    #[test]
    fn test_progress_from_status_updates_ignores_unrelated_batches() {
        // Arrange
        let session_id = "session-1";
        let mut queue = MergeQueue::default();
        queue.set_active(session_id.to_string());
        let touched_ids = HashSet::new();
        let previous_states = HashMap::new();

        // Act
        let progress = queue.progress_from_status_updates(
            Some(Status::Merging),
            &touched_ids,
            &previous_states,
        );

        // Assert
        assert_eq!(progress, MergeQueueProgress::NoAction);
        assert!(queue.has_active());
    }
}
