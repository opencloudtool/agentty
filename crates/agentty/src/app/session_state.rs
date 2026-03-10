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

    /// Synchronizes one session snapshot from shared runtime handles.
    ///
    /// Output synchronization prefers an append-only fast path: when runtime
    /// output extends the existing snapshot with the same prefix, only the
    /// newly appended suffix is copied. Equal-length rewrites and non-prefix
    /// changes still fall back to full replacement so edits are never missed.
    fn sync_session_with_handles(session: &mut Session, session_handles: &SessionHandles) {
        if let Ok(output) = session_handles.output.lock() {
            Self::sync_session_output(&mut session.output, output.as_str());
        }

        if let Ok(status) = session_handles.status.lock() {
            session.status = *status;
        }
    }

    /// Synchronizes one output snapshot from its shared runtime output buffer.
    ///
    /// This keeps equal-length rewrites correct while reducing copy cost for
    /// append-heavy streams by pushing only the unseen suffix.
    fn sync_session_output(session_output: &mut String, handle_output: &str) {
        let session_output_len = session_output.len();
        let handle_output_len = handle_output.len();

        if session_output_len == handle_output_len {
            if session_output != handle_output {
                session_output.clear();
                session_output.push_str(handle_output);
            }
        } else if handle_output_len > session_output_len
            && handle_output.starts_with(session_output.as_str())
            && let Some(appended_output) = handle_output.get(session_output_len..)
        {
            session_output.push_str(appended_output);
        } else {
            session_output.clear();
            session_output.push_str(handle_output);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use ratatui::widgets::TableState;

    use super::*;
    use crate::domain::agent::AgentKind;
    use crate::domain::session::{Session, SessionHandles, SessionSize, SessionStats, Status};

    struct FixedClock {
        instant: Instant,
        system_time: SystemTime,
    }

    impl FixedClock {
        fn new() -> Self {
            Self {
                instant: Instant::now(),
                system_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            }
        }
    }

    impl Clock for FixedClock {
        fn now_instant(&self) -> Instant {
            self.instant
        }

        fn now_system_time(&self) -> SystemTime {
            self.system_time
        }
    }

    #[test]
    /// Verifies handle output replaces session output even when lengths match.
    fn sync_from_handles_updates_output_when_same_length_content_changes() {
        // Arrange
        let session_id = "sess-1".to_string();
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: std::env::temp_dir(),
            id: session_id.clone(),
            model: AgentKind::Gemini.default_model(),
            output: "old".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles = HashMap::from([(
            session_id,
            SessionHandles::new("new".to_string(), Status::Review),
        )]);
        let mut state = SessionState::new(
            handles,
            vec![session],
            TableState::default(),
            Arc::new(FixedClock::new()),
            0,
            0,
        );

        // Act
        state.sync_from_handles();

        // Assert
        assert_eq!(state.sessions[0].output, "new");
        assert_eq!(state.sessions[0].status, Status::Review);
    }

    #[test]
    /// Verifies direct single-session sync updates output and status together.
    fn sync_session_with_handles_equal_length_sync() {
        // Arrange
        let mut session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: std::env::temp_dir(),
            id: "session-2".to_string(),
            model: AgentKind::Gemini.default_model(),
            output: "Old".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::New,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles = SessionHandles::new("New".to_string(), Status::InProgress);

        // Act
        SessionState::sync_session_with_handles(&mut session, &handles);

        // Assert
        assert_eq!(session.output, "New");
        assert_eq!(session.status, Status::InProgress);
    }

    #[test]
    /// Verifies append-only output sync copies only the new suffix.
    fn sync_session_with_handles_appends_suffix_for_extended_output() {
        // Arrange
        let mut session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: std::env::temp_dir(),
            id: "session-3".to_string(),
            model: AgentKind::Gemini.default_model(),
            output: "first line\n".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles =
            SessionHandles::new("first line\nsecond line\n".to_string(), Status::InProgress);

        // Act
        SessionState::sync_session_with_handles(&mut session, &handles);

        // Assert
        assert_eq!(session.output, "first line\nsecond line\n");
        assert_eq!(session.status, Status::InProgress);
    }

    #[test]
    /// Verifies non-prefix changes still use full replacement when lengths
    /// differ.
    fn sync_session_with_handles_replaces_output_when_prefix_changes() {
        // Arrange
        let mut session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: std::env::temp_dir(),
            id: "session-4".to_string(),
            model: AgentKind::Gemini.default_model(),
            output: "abc".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles = SessionHandles::new("xyzq".to_string(), Status::Review);

        // Act
        SessionState::sync_session_with_handles(&mut session, &handles);

        // Assert
        assert_eq!(session.output, "xyzq");
        assert_eq!(session.status, Status::Review);
    }
}
