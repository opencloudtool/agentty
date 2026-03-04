use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ratatui::widgets::TableState;

use crate::app::session::{Clock, SESSION_REFRESH_INTERVAL};
use crate::domain::session::{Session, SessionHandles};

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    pub handles: HashMap<String, SessionHandles>,
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) refresh_deadline: Instant,
    pub(crate) row_count: i64,
    pub(crate) updated_at_max: i64,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    ///
    /// Time values are provided by an injected clock so refresh scheduling can
    /// be deterministic in tests.
    pub(crate) fn new(
        handles: HashMap<String, SessionHandles>,
        sessions: Vec<Session>,
        table_state: TableState,
        clock: Arc<dyn Clock>,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        let _state_created_at = clock.now_system_time();
        let refresh_deadline = clock.now_instant() + SESSION_REFRESH_INTERVAL;

        Self {
            handles,
            sessions,
            table_state,
            clock,
            refresh_deadline,
            row_count,
            updated_at_max,
        }
    }

    /// Copies current values from one runtime handle into its `Session`
    /// snapshot.
    pub fn sync_session_from_handle(&mut self, session_id: &str) {
        let Some(session_handles) = self.handles.get(session_id) else {
            return;
        };
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        Self::sync_session_with_handles(session, session_handles);
    }

    /// Copies current values from runtime handles into plain `Session` fields.
    pub fn sync_from_handles(&mut self) {
        let handles = &self.handles;

        for session in &mut self.sessions {
            let Some(session_handles) = handles.get(&session.id) else {
                continue;
            };

            Self::sync_session_with_handles(session, session_handles);
        }
    }

    /// Syncs one session snapshot with the latest runtime handle values.
    ///
    /// Output replacement uses full string inequality so equal-length text
    /// changes are still propagated to the UI snapshot.
    fn sync_session_with_handles(session: &mut Session, session_handles: &SessionHandles) {
        if let Ok(output) = session_handles.output.lock()
            && session.output != *output
        {
            session.output.clone_from(&*output);
        }

        if let Ok(status) = session_handles.status.lock() {
            session.status = *status;
        }
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
    use crate::domain::agent::AgentModel;
    use crate::domain::session::{Session, SessionSize, SessionStats, Status};

    struct TestClock;

    impl Clock for TestClock {
        fn now_instant(&self) -> Instant {
            Instant::now()
        }

        fn now_system_time(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    /// Ensures output updates propagate even when old/new output lengths match.
    #[test]
    fn test_sync_from_handles_replaces_equal_length_output() {
        // Arrange
        let session_id = "session01";
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::from("/tmp/session01"),
            id: session_id.to_string(),
            model: AgentModel::ClaudeOpus46,
            output: "stale".to_string(),
            project_name: "test".to_string(),
            prompt: "prompt".to_string(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: Some("test".to_string()),
            updated_at: 0,
        };
        let mut handles: HashMap<String, SessionHandles> = HashMap::new();
        handles.insert(
            session_id.to_string(),
            SessionHandles::new("fresh".to_string(), Status::Review),
        );
        let mut state = SessionState::new(
            handles,
            vec![session],
            TableState::default(),
            Arc::new(TestClock),
            0,
            0,
        );

        // Act
        state.sync_from_handles();

        // Assert
        assert_eq!(state.sessions[0].output, "fresh");
        assert_eq!(state.sessions[0].status, Status::Review);
    }

    /// Ensures direct single-session sync updates output and status together.
    #[test]
    fn test_sync_session_with_handles_equal_length_sync() {
        // Arrange
        let session_id = "test";
        let mut session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::from("/tmp/test"),
            id: session_id.to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: "Old".to_string(), // length 3
            project_name: "test".to_string(),
            prompt: "prompt".to_string(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::New,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles = SessionHandles::new("New".to_string(), Status::InProgress); // length 3

        // Act
        SessionState::sync_session_with_handles(&mut session, &handles);

        // Assert
        assert_eq!(session.output, "New");
        assert_eq!(session.status, Status::InProgress);
    }
}
