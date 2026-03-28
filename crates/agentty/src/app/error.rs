use crate::app::session::SessionError;

/// Typed error returned by top-level app orchestration operations.
///
/// Wraps session-layer errors, direct infrastructure failures from the app
/// layer, and startup or workflow-specific failures, replacing the previous
/// opaque `Result<T, String>` pattern in `App` methods and `main.rs`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A session workflow operation failed.
    #[error("{0}")]
    Session(#[from] SessionError),

    /// A database operation failed at the app layer.
    #[error("{0}")]
    Db(#[from] crate::infra::db::DbError),

    /// A git operation failed at the app layer.
    #[error("{0}")]
    Git(#[from] crate::infra::git::GitError),

    /// A workflow-specific or startup failure with a contextual message.
    #[error("{0}")]
    Workflow(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_error_converts_via_from() {
        // Arrange
        let session_error = SessionError::NotFound;

        // Act
        let error = AppError::from(session_error);

        // Assert
        assert!(matches!(error, AppError::Session(SessionError::NotFound)));
        assert_eq!(error.to_string(), "Session not found");
    }

    #[test]
    fn workflow_display_shows_contextual_message() {
        // Arrange
        let error = AppError::Workflow("Failed to run terminal UI: broken pipe".to_string());

        // Act / Assert
        assert_eq!(error.to_string(), "Failed to run terminal UI: broken pipe");
    }

    #[test]
    fn db_error_converts_via_from() {
        // Arrange
        let db_error = crate::infra::db::DbError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "db file missing",
        ));

        // Act
        let error = AppError::from(db_error);

        // Assert
        assert!(matches!(error, AppError::Db(_)));
        assert!(error.to_string().contains("db file missing"));
    }
}
