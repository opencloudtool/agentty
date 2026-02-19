//! Shared agent-assistance helpers for commit/rebase recovery loops.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::AgentModel;
use crate::app::AppEvent;
use crate::app::task::{RunAgentAssistTaskInput, TaskService};
use crate::db::Database;
use crate::model::PermissionMode;

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
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) db: Database,
    pub(super) folder: PathBuf,
    pub(super) id: String,
    pub(super) output: Arc<Mutex<String>>,
    pub(super) permission_mode: PermissionMode,
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

/// Converts Plan-mode permission into edit-capable mode for assist runs.
pub(super) fn effective_permission_mode(permission_mode: PermissionMode) -> PermissionMode {
    if permission_mode == PermissionMode::Plan {
        return PermissionMode::AutoEdit;
    }

    permission_mode
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
    TaskService::append_session_output(
        &context.output,
        &context.db,
        &context.app_event_tx,
        &context.id,
        &assist_header,
    )
    .await;
}

/// Executes one fresh assistance run using the provided prompt.
///
/// # Errors
/// Returns an error when the agent process cannot be spawned, fails, or is
/// interrupted.
pub(super) async fn run_agent_assist(context: &AssistContext, prompt: &str) -> Result<(), String> {
    let effective_permission_mode = effective_permission_mode(context.permission_mode);
    let backend = context.session_model.kind().create_backend();
    let command = backend.build_start_command(
        &context.folder,
        prompt,
        context.session_model.as_str(),
        effective_permission_mode,
    );

    TaskService::run_agent_assist_task(RunAgentAssistTaskInput {
        agent: context.session_model.kind(),
        app_event_tx: context.app_event_tx.clone(),
        cmd: command,
        db: context.db.clone(),
        id: context.id.clone(),
        output: Arc::clone(&context.output),
        permission_mode: effective_permission_mode,
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

    #[test]
    fn test_effective_permission_mode_plan_uses_auto_edit() {
        // Arrange
        let permission_mode = PermissionMode::Plan;

        // Act
        let effective_mode = effective_permission_mode(permission_mode);

        // Assert
        assert_eq!(effective_mode, PermissionMode::AutoEdit);
    }
}
