use std::collections::HashMap;
use std::time::Instant;

use ratatui::widgets::TableState;

use crate::app::session::SESSION_REFRESH_INTERVAL;
use crate::domain::session::{Session, SessionHandles};

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    pub handles: HashMap<String, SessionHandles>,
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    pub(crate) refresh_deadline: Instant,
    pub(crate) row_count: i64,
    pub(crate) updated_at_max: i64,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    pub fn new(
        handles: HashMap<String, SessionHandles>,
        sessions: Vec<Session>,
        table_state: TableState,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        Self {
            handles,
            sessions,
            table_state,
            refresh_deadline: Instant::now() + SESSION_REFRESH_INTERVAL,
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

    fn sync_session_with_handles(session: &mut Session, session_handles: &SessionHandles) {
        if let Ok(output) = session_handles.output.lock()
            && session.output.len() != output.len()
        {
            session.output.clone_from(&*output);
        }

        if let Ok(status) = session_handles.status.lock() {
            session.status = *status;
        }
    }
}
