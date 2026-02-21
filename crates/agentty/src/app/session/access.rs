//! Shared session lookup helpers and canonical lookup error strings.

use crate::app::SessionManager;
use crate::domain::session::{Session, SessionHandles};

/// Canonical session-handle-not-found error string used by app workflows.
pub(crate) const SESSION_HANDLES_NOT_FOUND_ERROR: &str = "Session handles not found";
/// Canonical session-not-found error string used by app workflows.
pub(crate) const SESSION_NOT_FOUND_ERROR: &str = "Session not found";

impl SessionManager {
    /// Resolves a session identifier into its current list index.
    pub(crate) fn session_index_or_err(&self, session_id: &str) -> Result<usize, String> {
        self.session_index_for_id(session_id)
            .ok_or_else(|| SESSION_NOT_FOUND_ERROR.to_string())
    }

    /// Resolves an immutable session reference by identifier.
    pub(crate) fn session_or_err(&self, session_id: &str) -> Result<&Session, String> {
        let session_index = self.session_index_or_err(session_id)?;

        self.sessions
            .get(session_index)
            .ok_or_else(|| SESSION_NOT_FOUND_ERROR.to_string())
    }

    /// Resolves runtime handles for a session identifier.
    pub(crate) fn session_handles_or_err(
        &self,
        session_id: &str,
    ) -> Result<&SessionHandles, String> {
        self.handles
            .get(session_id)
            .ok_or_else(|| SESSION_HANDLES_NOT_FOUND_ERROR.to_string())
    }

    /// Resolves both immutable session data and runtime handles together.
    pub(crate) fn session_and_handles_or_err(
        &self,
        session_id: &str,
    ) -> Result<(&Session, &SessionHandles), String> {
        let session = self.session_or_err(session_id)?;
        let handles = self.session_handles_or_err(session_id)?;

        Ok((session, handles))
    }
}
