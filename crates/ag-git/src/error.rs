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

    /// A git subprocess exceeded its configured runtime bound.
    #[error("{command} timed out after {timeout:?}")]
    CommandTimedOut {
        /// Git invocation that exceeded the timeout.
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
#[path = "error_test.rs"]
mod tests;
