/// Typed error returned by session-layer workflow operations.
///
/// Wraps infrastructure errors from git, database, app-server, and forge
/// boundaries alongside workflow-specific validation failures, replacing the
/// previous opaque `Result<T, String>` pattern used throughout session
/// orchestration code.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Session not found in the current manager state.
    #[error("Session not found")]
    NotFound,

    /// Session runtime handles not found.
    #[error("Session handles not found")]
    HandlesNotFound,

    /// A git infrastructure operation failed.
    #[error("{0}")]
    Git(#[from] crate::infra::git::GitError),

    /// A database operation failed.
    #[error("{0}")]
    Db(#[from] crate::infra::db::DbError),

    /// A filesystem boundary operation failed.
    #[error("{0}")]
    Fs(#[from] crate::infra::fs::FsError),

    /// An app-server operation failed.
    #[error("{0}")]
    AppServer(#[from] crate::infra::app_server::AppServerError),

    /// A workflow-specific failure with a contextual message.
    ///
    /// Covers validation, forge operations, template rendering, and other
    /// transient or domain-specific failures that do not warrant dedicated
    /// variants.
    #[error("{0}")]
    Workflow(String),
}

impl SessionError {
    /// Prefixes the display message of `Workflow` variants with the given
    /// context string so callers can distinguish which assist operation
    /// produced the failure.
    ///
    /// Typed infrastructure variants (`Git`, `Db`, `AppServer`) pass through
    /// unchanged because their type already identifies the failure origin
    /// and callers can still discriminate them by pattern matching.  In
    /// practice every current call site (`run_rebase_assist_agent`,
    /// `run_sync_rebase_assist_agent`, commit-assist in `task.rs`) only
    /// receives `Workflow` variants because the upstream assist functions
    /// convert all errors via `SessionError::Workflow(String)`.  The
    /// pass-through arm is a safety net so future callers that propagate
    /// typed infra errors do not silently lose their structured variant.
    #[must_use]
    pub fn with_context(self, context: &str) -> Self {
        match self {
            Self::Workflow(message) => Self::Workflow(format!("{context}: {message}")),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_display_shows_canonical_message() {
        // Arrange / Act
        let error = SessionError::NotFound;

        // Assert
        assert_eq!(error.to_string(), "Session not found");
    }

    #[test]
    fn handles_not_found_display_shows_canonical_message() {
        // Arrange / Act
        let error = SessionError::HandlesNotFound;

        // Assert
        assert_eq!(error.to_string(), "Session handles not found");
    }

    #[test]
    fn workflow_display_shows_contextual_message() {
        // Arrange
        let error = SessionError::Workflow("Session must be in review status".to_string());

        // Act / Assert
        assert_eq!(error.to_string(), "Session must be in review status");
    }

    #[test]
    fn git_error_converts_via_from() {
        // Arrange
        let git_error = crate::infra::git::GitError::OutputParse("bad output".to_string());

        // Act
        let error = SessionError::from(git_error);

        // Assert
        assert!(matches!(error, SessionError::Git(_)));
        assert_eq!(error.to_string(), "bad output");
    }

    #[test]
    fn db_error_converts_via_from() {
        // Arrange
        let db_error = crate::infra::db::DbError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "db file missing",
        ));

        // Act
        let error = SessionError::from(db_error);

        // Assert
        assert!(matches!(error, SessionError::Db(_)));
        assert!(error.to_string().contains("db file missing"));
    }

    #[test]
    /// Ensures `with_context` prefixes the message of `Workflow` variants so
    /// callers can distinguish which assist operation produced the failure.
    fn with_context_prefixes_workflow_message() {
        // Arrange
        let error = SessionError::Workflow("agent backend unavailable".to_string());

        // Act
        let contextual = error.with_context("Commit assistance failed");

        // Assert
        assert_eq!(
            contextual.to_string(),
            "Commit assistance failed: agent backend unavailable"
        );
    }

    #[test]
    /// Ensures `with_context` passes typed infrastructure variants through
    /// unchanged because their type already identifies the failure origin.
    fn with_context_preserves_typed_infrastructure_variants() {
        // Arrange
        let error = SessionError::Git(crate::infra::git::GitError::OutputParse("bad".to_string()));

        // Act
        let contextual = error.with_context("Rebase assistance failed");

        // Assert
        assert!(
            matches!(contextual, SessionError::Git(_)),
            "expected Git variant, got: {contextual:?}"
        );
        assert_eq!(contextual.to_string(), "bad");
    }
}
