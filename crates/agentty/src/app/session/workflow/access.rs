//! Shared session lookup helpers using typed [`SessionError`] results.

use crate::app::SessionManager;
use crate::app::session::SessionError;
use crate::domain::session::{Session, SessionHandles};

impl SessionManager {
    /// Resolves a session identifier into its current list index.
    pub(crate) fn session_index_or_err(&self, session_id: &str) -> Result<usize, SessionError> {
        self.session_index_for_id(session_id)
            .ok_or(SessionError::NotFound)
    }

    /// Resolves an immutable session reference by identifier.
    pub(crate) fn session_or_err(&self, session_id: &str) -> Result<&Session, SessionError> {
        let session_index = self.session_index_or_err(session_id)?;

        self.state()
            .sessions
            .get(session_index)
            .ok_or(SessionError::NotFound)
    }

    /// Resolves runtime handles for a session identifier.
    pub(crate) fn session_handles_or_err(
        &self,
        session_id: &str,
    ) -> Result<&SessionHandles, SessionError> {
        self.state()
            .handles
            .get(session_id)
            .ok_or(SessionError::HandlesNotFound)
    }

    /// Resolves both immutable session data and runtime handles together.
    pub(crate) fn session_and_handles_or_err(
        &self,
        session_id: &str,
    ) -> Result<(&Session, &SessionHandles), SessionError> {
        let session = self.session_or_err(session_id)?;
        let handles = self.session_handles_or_err(session_id)?;

        Ok((session, handles))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Instant, SystemTime};

    use ratatui::widgets::TableState;

    use crate::app::session::{Clock, SessionDefaults, SessionError};
    use crate::app::{SessionManager, SessionState};
    use crate::domain::agent::{AgentKind, AgentModel};
    use crate::domain::session::{Session, SessionHandles, SessionSize, SessionStats, Status};
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

    /// Builds a session manager with given sessions and optional handles.
    fn session_manager_with(
        sessions: Vec<Session>,
        handles: HashMap<String, SessionHandles>,
    ) -> SessionManager {
        SessionManager::new(
            SessionDefaults {
                model: AgentKind::Gemini.default_model(),
            },
            Arc::new(git::MockGitClient::new()),
            SessionState::new(
                handles,
                sessions,
                TableState::default(),
                Arc::new(FixedClock),
                0,
                0,
            ),
            Vec::new(),
        )
    }

    // --- session_index_or_err ---

    #[test]
    fn test_session_index_or_err_returns_index_for_existing_session() {
        // Arrange
        let session = test_session("sess-1", Status::Review);
        let manager = session_manager_with(vec![session], HashMap::new());

        // Act
        let index = manager
            .session_index_or_err("sess-1")
            .expect("session should be found");

        // Assert
        assert_eq!(index, 0);
    }

    #[test]
    fn test_session_index_or_err_returns_correct_index_for_second_session() {
        // Arrange
        let session_a = test_session("sess-a", Status::Review);
        let session_b = test_session("sess-b", Status::New);
        let manager = session_manager_with(vec![session_a, session_b], HashMap::new());

        // Act
        let index = manager
            .session_index_or_err("sess-b")
            .expect("session should be found");

        // Assert
        assert_eq!(index, 1);
    }

    #[test]
    fn test_session_index_or_err_returns_not_found_for_missing_session() {
        // Arrange
        let manager = session_manager_with(Vec::new(), HashMap::new());

        // Act
        let result = manager.session_index_or_err("nonexistent");

        // Assert
        assert!(matches!(result, Err(SessionError::NotFound)));
    }

    // --- session_or_err ---

    #[test]
    fn test_session_or_err_returns_session_reference() {
        // Arrange
        let session = test_session("sess-1", Status::InProgress);
        let manager = session_manager_with(vec![session], HashMap::new());

        // Act
        let found = manager
            .session_or_err("sess-1")
            .expect("session should be found");

        // Assert
        assert_eq!(found.id, "sess-1");
        assert_eq!(found.status, Status::InProgress);
    }

    #[test]
    fn test_session_or_err_returns_not_found_for_missing_session() {
        // Arrange
        let manager = session_manager_with(Vec::new(), HashMap::new());

        // Act
        let result = manager.session_or_err("missing");

        // Assert
        assert!(matches!(result, Err(SessionError::NotFound)));
    }

    // --- session_handles_or_err ---

    #[test]
    fn test_session_handles_or_err_returns_handles() {
        // Arrange
        let mut handles = HashMap::new();
        handles.insert(
            "sess-1".to_string(),
            SessionHandles::new(String::new(), Status::Review),
        );
        let manager = session_manager_with(Vec::new(), handles);

        // Act
        let result = manager.session_handles_or_err("sess-1");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn test_session_handles_or_err_returns_handles_not_found() {
        // Arrange
        let manager = session_manager_with(Vec::new(), HashMap::new());

        // Act
        let result = manager.session_handles_or_err("missing");

        // Assert
        assert!(matches!(result, Err(SessionError::HandlesNotFound)));
    }

    // --- session_and_handles_or_err ---

    #[test]
    fn test_session_and_handles_returns_both() {
        // Arrange
        let session = test_session("sess-1", Status::Review);
        let mut handles = HashMap::new();
        handles.insert(
            "sess-1".to_string(),
            SessionHandles::new("output".to_string(), Status::Review),
        );
        let manager = session_manager_with(vec![session], handles);

        // Act
        let result = manager.session_and_handles_or_err("sess-1");

        // Assert
        assert!(result.is_ok());
        if let Ok((found_session, found_handles)) = result {
            assert_eq!(found_session.id, "sess-1");
            let output = found_handles
                .output
                .lock()
                .expect("failed to lock output")
                .clone();
            assert_eq!(output, "output");
        }
    }

    #[test]
    fn test_session_and_handles_fails_when_session_missing() {
        // Arrange
        let mut handles = HashMap::new();
        handles.insert(
            "sess-1".to_string(),
            SessionHandles::new(String::new(), Status::Review),
        );
        let manager = session_manager_with(Vec::new(), handles);

        // Act
        let result = manager.session_and_handles_or_err("sess-1");

        // Assert
        assert!(matches!(result, Err(SessionError::NotFound)));
    }

    #[test]
    fn test_session_and_handles_fails_when_handles_missing() {
        // Arrange
        let session = test_session("sess-1", Status::Review);
        let manager = session_manager_with(vec![session], HashMap::new());

        // Act
        let result = manager.session_and_handles_or_err("sess-1");

        // Assert
        assert!(matches!(result, Err(SessionError::HandlesNotFound)));
    }
}
