//! Shared agent-assistance helpers for commit/rebase recovery loops.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::app::session::{RunAgentAssistTaskInput, SessionTaskService};
use crate::domain::agent::AgentModel;
use crate::infra::db::Database;
use crate::infra::git::GitClient;

/// Policy knobs controlling one assisted recovery loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AssistPolicy {
    /// Maximum number of assist attempts before hard failure.
    pub(super) max_attempts: usize,
    /// Maximum identical-failure streak tolerated before fail-fast.
    pub(super) max_identical_failure_streak: usize,
}

/// Shared context required to execute one assistance attempt.
pub(super) struct AssistContext {
    /// App event sender used to update UI progress/output state.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Database handle used for session persistence updates.
    pub(super) db: Database,
    /// Session worktree folder where git/agent commands run.
    pub(super) folder: PathBuf,
    /// Git boundary used for commit/rebase operations.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Session identifier receiving assist output updates.
    pub(super) id: String,
    /// Shared output buffer mirrored to persistence and UI.
    pub(super) output: Arc<Mutex<String>>,
    /// Model used when invoking agent-assisted recovery.
    pub(super) session_model: AgentModel,
}

/// Tracks repeated identical failures to stop non-progressing assist loops.
pub(super) struct FailureTracker {
    max_identical_failure_streak: usize,
    previous_fingerprint: String,
    streak: usize,
}

impl FailureTracker {
    /// Creates a tracker with a maximum allowed identical-failure streak.
    pub(super) fn new(max_identical_failure_streak: usize) -> Self {
        Self {
            max_identical_failure_streak,
            previous_fingerprint: String::new(),
            streak: 0,
        }
    }

    /// Records one failure fingerprint and returns `true` when the
    /// identical-failure streak exceeded the configured limit.
    pub(super) fn observe(&mut self, fingerprint: &str) -> bool {
        let normalized_fingerprint = fingerprint.trim().to_ascii_lowercase();
        if normalized_fingerprint.is_empty() {
            self.previous_fingerprint.clear();
            self.streak = 0;

            return false;
        }

        if self.previous_fingerprint == normalized_fingerprint {
            self.streak += 1;
        } else {
            self.previous_fingerprint = normalized_fingerprint;
            self.streak = 1;
        }

        self.streak > self.max_identical_failure_streak
    }
}

/// Renders newline-separated details as `- item` lines for output display.
pub(super) fn format_detail_lines(detail: &str) -> String {
    detail
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Appends a normalized assist-attempt header to the session output buffer.
pub(super) async fn append_assist_header(
    context: &AssistContext,
    assist_label: &str,
    assist_attempt: usize,
    max_assist_attempts: usize,
    assist_action: &str,
    detail: &str,
) {
    let assist_header = format!(
        "\n[{assist_label} Assist] Attempt {assist_attempt}/{max_assist_attempts}. \
         {assist_action}\n{detail}\n"
    );
    SessionTaskService::append_session_output(
        &context.output,
        &context.db,
        &context.app_event_tx,
        &context.id,
        &assist_header,
    )
    .await;
}

/// Executes one assistance run using the current session context.
///
/// # Errors
/// Returns an error when the agent process cannot be spawned, fails, or is
/// interrupted.
pub(super) async fn run_agent_assist(context: &AssistContext, prompt: &str) -> Result<(), String> {
    let backend = crate::infra::agent::create_backend(context.session_model.kind());
    let command = backend.build_resume_command(
        &context.folder,
        prompt,
        context.session_model.as_str(),
        None,
    )?;

    SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
        agent: context.session_model.kind(),
        app_event_tx: context.app_event_tx.clone(),
        cmd: command,
        db: context.db.clone(),
        id: context.id.clone(),
        output: Arc::clone(&context.output),
        session_model: context.session_model,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failure_tracker_observe_exceeds_after_identical_streak_limit() {
        // Arrange
        let mut tracker = FailureTracker::new(2);

        // Act
        let first_exceeded = tracker.observe("same");
        let second_exceeded = tracker.observe("same");
        let third_exceeded = tracker.observe("same");

        // Assert
        assert!(!first_exceeded);
        assert!(!second_exceeded);
        assert!(third_exceeded);
    }

    #[test]
    fn test_failure_tracker_observe_resets_streak_for_new_fingerprint() {
        // Arrange
        let mut tracker = FailureTracker::new(2);
        let _ = tracker.observe("same");
        let _ = tracker.observe("same");

        // Act
        let exceeded = tracker.observe("other");

        // Assert
        assert!(!exceeded);
    }

    #[test]
    fn test_format_detail_lines_returns_bulleted_non_empty_lines() {
        // Arrange
        let detail = "line one\n\nline two";

        // Act
        let formatted = format_detail_lines(detail);

        // Assert
        assert_eq!(formatted, "- line one\n- line two");
    }
}
