//! Shared app-server runtime registry helpers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::infra::app_server::AppServerError;

/// Shared runtime registry used by app-server providers.
///
/// Each session id maps to one runtime process. Workers temporarily remove a
/// runtime while executing a turn and store it back when the turn succeeds.
pub struct AppServerSessionRegistry<Runtime> {
    provider_name: &'static str,
    sessions: Arc<Mutex<HashMap<String, Runtime>>>,
}

impl<Runtime> AppServerSessionRegistry<Runtime> {
    /// Creates an empty session runtime registry for one provider.
    pub fn new(provider_name: &'static str) -> Self {
        Self {
            provider_name,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Removes and returns the runtime stored for `session_id`.
    ///
    /// # Errors
    /// Returns an error when the session map lock is poisoned.
    pub fn take_session(&self, session_id: &str) -> Result<Option<Runtime>, AppServerError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AppServerError::LockPoisoned {
                provider: self.provider_name,
            })?;

        Ok(sessions.remove(session_id))
    }

    /// Stores or replaces the runtime for `session_id`.
    ///
    /// # Errors
    /// Returns an error when the session map lock is poisoned.
    pub fn store_session(
        &self,
        session_id: String,
        session: Runtime,
    ) -> Result<(), AppServerError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AppServerError::LockPoisoned {
                provider: self.provider_name,
            })?;
        sessions.insert(session_id, session);

        Ok(())
    }

    /// Stores or replaces the runtime for `session_id`, returning ownership
    /// back to the caller when lock acquisition fails.
    ///
    /// This allows callers to shut down process-backed runtimes before
    /// returning an error, preventing orphaned child processes on early exits.
    ///
    /// # Errors
    /// Returns `(error, session)` when the session map lock is poisoned.
    pub fn store_session_or_recover(
        &self,
        session_id: String,
        session: Runtime,
    ) -> Result<(), (AppServerError, Runtime)> {
        let Ok(mut sessions) = self.sessions.lock() else {
            return Err((
                AppServerError::LockPoisoned {
                    provider: self.provider_name,
                },
                session,
            ));
        };
        sessions.insert(session_id, session);

        Ok(())
    }

    /// Returns the provider label used in user-facing retry errors.
    pub fn provider_name(&self) -> &'static str {
        self.provider_name
    }
}

/// Clones the registry handle by sharing the same underlying session map.
impl<Runtime> Clone for AppServerSessionRegistry<Runtime> {
    fn clone(&self) -> Self {
        Self {
            provider_name: self.provider_name,
            sessions: Arc::clone(&self.sessions),
        }
    }
}
