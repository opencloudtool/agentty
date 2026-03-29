//! Review-session replay helpers.

use std::collections::HashSet;

use super::SessionManager;
use crate::domain::session::{Session, Status};

impl SessionManager {
    /// Collects session ids that should replay persisted transcript output on
    /// the next reply after app startup.
    pub(in crate::app::session) fn startup_history_replay_set(
        sessions: &[Session],
    ) -> HashSet<String> {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Instant, SystemTime};

    use ratatui::widgets::TableState;

    use super::*;
    use crate::app::SessionState;
    use crate::app::session::{Clock, SessionDefaults};
    use crate::domain::agent::{AgentKind, AgentModel};
    use crate::domain::session::{SessionSize, SessionStats};
    use crate::infra::git;

    /// Deterministic clock for test construction.
    struct FixedClock;

    impl Clock for FixedClock {
        fn now_instant(&self) -> Instant {
            Instant::now()
        }

        fn now_system_time(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    /// Builds a minimal test session with the given identifier and status.
    fn test_session(session_id: &str, status: Status) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::from("/tmp/test"),
            follow_up_tasks: Vec::new(),
            id: session_id.to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: None,
            updated_at: 0,
        }
    }

    /// Builds a session manager with given sessions (no handles).
    fn session_manager_with(sessions: Vec<Session>) -> SessionManager {
        SessionManager::new(
            SessionDefaults {
                model: AgentKind::Gemini.default_model(),
            },
            Arc::new(git::MockGitClient::new()),
            SessionState::new(
                HashMap::new(),
                sessions,
                TableState::default(),
                Arc::new(FixedClock),
                0,
                0,
            ),
            Vec::new(),
        )
    }

    // --- startup_history_replay_set ---

    #[test]
    fn test_startup_replay_set_collects_review_sessions() {
        // Arrange
        let sessions = vec![
            test_session("review-1", Status::Review),
            test_session("in-progress", Status::InProgress),
            test_session("review-2", Status::Review),
        ];

        // Act
        let replay_set = SessionManager::startup_history_replay_set(&sessions);

        // Assert
        assert_eq!(replay_set.len(), 2);
        assert!(replay_set.contains("review-1"));
        assert!(replay_set.contains("review-2"));
    }

    #[test]
    fn test_startup_replay_set_returns_empty_when_no_review_sessions() {
        // Arrange
        let sessions = vec![
            test_session("new-1", Status::New),
            test_session("done-1", Status::Done),
        ];

        // Act
        let replay_set = SessionManager::startup_history_replay_set(&sessions);

        // Assert
        assert!(replay_set.is_empty());
    }

    #[test]
    fn test_startup_replay_set_returns_empty_for_empty_list() {
        // Arrange / Act
        let replay_set = SessionManager::startup_history_replay_set(&[]);

        // Assert
        assert!(replay_set.is_empty());
    }

    // --- mark_history_replay_pending / should_replay_history ---

    #[test]
    fn test_mark_and_check_replay_pending() {
        // Arrange
        let mut manager = session_manager_with(Vec::new());

        // Act
        manager.mark_history_replay_pending("sess-1");

        // Assert
        assert!(manager.should_replay_history("sess-1"));
    }

    #[test]
    fn test_should_replay_returns_false_when_not_marked() {
        // Arrange
        let manager = session_manager_with(Vec::new());

        // Act / Assert
        assert!(!manager.should_replay_history("unknown"));
    }

    // --- clear_history_replay_pending ---

    #[test]
    fn test_clear_removes_pending_replay() {
        // Arrange
        let mut manager = session_manager_with(Vec::new());
        manager.mark_history_replay_pending("sess-1");

        // Act
        manager.clear_history_replay_pending("sess-1");

        // Assert
        assert!(!manager.should_replay_history("sess-1"));
    }

    #[test]
    fn test_clear_is_idempotent_for_unmarked_session() {
        // Arrange
        let mut manager = session_manager_with(Vec::new());

        // Act / Assert — does not panic
        manager.clear_history_replay_pending("nonexistent");
    }

    // --- constructor auto-marks review sessions ---

    #[test]
    fn test_constructor_marks_review_sessions_for_replay() {
        // Arrange
        let sessions = vec![
            test_session("review-sess", Status::Review),
            test_session("new-sess", Status::New),
        ];

        // Act
        let manager = session_manager_with(sessions);

        // Assert
        assert!(manager.should_replay_history("review-sess"));
        assert!(!manager.should_replay_history("new-sess"));
    }
}
