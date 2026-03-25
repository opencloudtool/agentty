/// Typed error returned by git infrastructure operations.
///
/// Wraps command execution failures, output parsing issues, and I/O errors so
/// callers can distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// A git subprocess exited with a non-zero status.
    #[error("{command}: {stderr}")]
    CommandFailed {
        /// The git command that was executed (e.g. `"git rebase main"`).
        command: String,
        /// Human-readable detail extracted from stderr/stdout.
        stderr: String,
    },

    /// Git command output could not be parsed into the expected structure.
    #[error("{0}")]
    OutputParse(String),

    /// A filesystem or process-spawn operation failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// A `tokio::task::spawn_blocking` join failed.
    #[error("Join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_failed_display_includes_command_and_stderr() {
        // Arrange
        let error = GitError::CommandFailed {
            command: "git push origin main".to_string(),
            stderr: "fatal: could not read Username".to_string(),
        };

        // Act
        let display = error.to_string();

        // Assert
        assert!(matches!(
            error,
            GitError::CommandFailed {
                ref command,
                ref stderr,
            } if command == "git push origin main" && stderr == "fatal: could not read Username"
        ));
        assert_eq!(
            display,
            "git push origin main: fatal: could not read Username"
        );
    }

    #[test]
    fn output_parse_display_shows_message() {
        // Arrange
        let error = GitError::OutputParse("unexpected rev-parse output".to_string());

        // Act / Assert
        assert!(
            matches!(error, GitError::OutputParse(ref message) if message == "unexpected rev-parse output")
        );
        assert_eq!(error.to_string(), "unexpected rev-parse output");
    }

    #[test]
    fn io_error_converts_via_from() {
        // Arrange
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");

        // Act
        let error = GitError::from(io_error);

        // Assert
        assert!(matches!(error, GitError::Io(_)));
        assert!(error.to_string().contains("file missing"));
    }
}
