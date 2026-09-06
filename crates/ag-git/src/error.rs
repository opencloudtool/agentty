use std::time::Duration;

use crate::rebase;

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

    /// A git subprocess or index preparation exceeded its runtime bound.
    #[error("{command} timed out after {timeout:?}")]
    CommandTimedOut {
        /// Git invocation or preparation step that exceeded the timeout.
        command: String,
        /// Configured command timeout.
        timeout: Duration,
    },

    /// Git command output could not be parsed into the expected structure.
    #[error("{0}")]
    OutputParse(String),

    /// The requested repository or worktree is no longer available.
    #[error("{detail}")]
    RepositoryUnavailable {
        /// Original repository-discovery failure detail.
        detail: String,
    },

    /// A filesystem or process-spawn operation failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// A repository declares pre-commit validation but its Git hook is
    /// unavailable.
    #[error(
        "pre-commit validation is configured by `{config_file}`, but the Git pre-commit hook is \
         not installed or executable. Install it with one of these commands:\n\n  prek install\n  \
         pre-commit install\n\nAgentty will continue for now, but missing configured hooks will \
         become an error in a future release."
    )]
    PreCommitHookMissing {
        /// Repository-root-relative configuration file that declares
        /// validation.
        config_file: String,
    },

    /// A `tokio::task::spawn_blocking` join failed.
    #[error("Join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl GitError {
    /// Returns whether a failed command reports Git index-lock contention.
    ///
    /// This identifies the lock failure without implying that the lock is
    /// stale or safe to remove.
    #[must_use]
    pub fn is_index_locked(&self) -> bool {
        matches!(self, Self::CommandFailed { stderr, .. } if rebase::is_git_index_lock_error(stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_lock_classification_requires_a_matching_command_failure() {
        // Arrange
        let cases = [
            (
                "fatal: Unable to create '.git/index.lock': File exists.",
                true,
            ),
            (
                "fatal: Unable to create '.git/worktrees/session/index.lock': File exists.",
                true,
            ),
            (
                "fatal: Unable to create '.git/HEAD.lock': File exists.",
                false,
            ),
            (
                "fatal: Unable to create '.git/index.lock': Permission denied",
                false,
            ),
            ("index.lock: another git process is running", true),
            ("pre-commit hook rejected changes", false),
            ("index.lock mentioned by a hook", false),
        ];

        for (stderr, expected) in cases {
            let error = GitError::CommandFailed {
                command: "git add -A".to_string(),
                stderr: stderr.to_string(),
            };

            // Act / Assert
            assert_eq!(error.is_index_locked(), expected, "{stderr}");
        }

        // Arrange
        let error = GitError::OutputParse(cases[0].0.to_string());

        // Act / Assert
        assert!(!error.is_index_locked());
    }

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
    fn command_timed_out_display_includes_command_and_timeout() {
        // Arrange
        let error = GitError::CommandTimedOut {
            command: "git worktree remove --force /tmp/worktree".to_string(),
            timeout: Duration::from_secs(30),
        };

        // Act
        let display = error.to_string();

        // Assert
        assert_eq!(
            display,
            "git worktree remove --force /tmp/worktree timed out after 30s"
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
    fn repository_unavailable_display_shows_original_detail() {
        // Arrange
        let error = GitError::RepositoryUnavailable {
            detail: "git rev-parse: not a git repository".to_string(),
        };

        // Act
        let display = error.to_string();

        // Assert
        assert_eq!(display, "git rev-parse: not a git repository");
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

    #[test]
    fn pre_commit_hook_missing_display_explains_required_setup() {
        // Arrange
        let error = GitError::PreCommitHookMissing {
            config_file: ".pre-commit-config.yaml".to_string(),
        };

        // Act
        let display = error.to_string();

        // Assert
        assert!(display.contains(".pre-commit-config.yaml"));
        assert!(display.contains("not installed or executable"));
        assert!(display.contains("prek install"));
        assert!(display.contains("pre-commit install"));
        assert!(display.contains("will become an error in a future release"));
    }
}
