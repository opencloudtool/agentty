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
