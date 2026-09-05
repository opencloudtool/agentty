//! Shared agent-assistance helpers for commit/rebase recovery loops.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent::OneShotClient;
use ag_git::GitClient;
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{RunAgentAssistTaskInput, SessionError, SessionTaskService};
use crate::domain::agent::AgentSelection;
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::db::AppRepositories;

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
    /// Session PID slot for CLI cancellation or retained app-server accounting.
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    /// Repository bundle used for session persistence updates.
    pub(super) db: AppRepositories,
    /// Session worktree folder where git/agent commands run.
    pub(super) folder: PathBuf,
    /// Git boundary used for commit/rebase operations.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Session identifier receiving assist output updates.
    pub(super) id: String,
    /// Provider-neutral boundary for isolated structured assist prompts.
    pub(super) one_shot_client: Arc<dyn OneShotClient>,
    /// Agent/model selection used when invoking agent-assisted recovery.
    pub(super) session_agent: AgentSelection,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
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
    notice: TranscriptNotice,
    assist_attempt: usize,
    max_assist_attempts: usize,
    assist_action: &str,
    detail: &str,
) {
    let assist_header = notice.format(format!(
        "Attempt {assist_attempt}/{max_assist_attempts}. {assist_action}\n{detail}"
    ));
    SessionTaskService::append_workflow_notice(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.id,
        &assist_header,
    )
    .await;
}

/// Executes one assistance run using the current session context.
///
/// # Errors
/// Returns an error when the one-shot assist command fails or returns invalid
/// protocol output.
pub(super) async fn run_agent_assist(
    context: &AssistContext,
    prompt: &str,
) -> Result<(), SessionError> {
    SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
        app_event_tx: context.app_event_tx.clone(),
        child_pid: Arc::clone(&context.child_pid),
        db: context.db.clone(),
        folder: context.folder.clone(),
        id: context.id.clone(),
        one_shot_client: Arc::clone(&context.one_shot_client),
        prompt: prompt.to_string(),
        session_agent: context.session_agent,
        session_update_versions: context.session_update_versions.clone(),
        transcript: Arc::clone(&context.transcript),
    })
    .await
}

#[cfg(test)]
mod tests {
    use ag_agent::MockOneShotClient;
    use ag_git::MockGitClient;
    use tempfile::tempdir;

    use super::*;
    use crate::domain::agent::{AgentKind, AgentModel};

    #[tokio::test]
    async fn test_run_agent_assist_uses_injected_one_shot_client() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
        let mut one_shot_client = MockOneShotClient::new();
        one_shot_client
            .expect_submit()
            .times(1)
            .returning(|request| {
                assert_eq!(request.prompt, "Resolve the conflict");

                Ok(ag_agent::OneShotSubmission {
                    response: ag_protocol::AgentResponse::plain("Conflict resolved"),
                    stats: ag_agent::SessionStats {
                        added_lines: 0,
                        deleted_lines: 0,
                        diff_state: ag_agent::SessionDiffState::Unknown,
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                })
            });
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let context = AssistContext {
            app_event_tx,
            child_pid: Arc::new(Mutex::new(None)),
            db: AppRepositories::in_memory().await.expect("db should open"),
            folder: temp_directory.path().to_path_buf(),
            git_client: Arc::new(MockGitClient::new()),
            id: "session-id".to_string(),
            one_shot_client: Arc::new(one_shot_client),
            session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
            session_update_versions: Arc::default(),
            transcript: Arc::clone(&transcript),
        };

        // Act
        let result = run_agent_assist(&context, "Resolve the conflict").await;

        // Assert
        result.expect("assist should succeed");
        let replay_text = transcript
            .lock()
            .expect("transcript lock should succeed")
            .replay_text();
        assert_eq!(replay_text.as_deref(), Some("Conflict resolved\n\n"));
    }

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
    fn test_failure_tracker_observe_normalizes_case_and_whitespace() {
        // Arrange
        let mut tracker = FailureTracker::new(1);

        // Act
        let first_exceeded = tracker.observe("  Same Failure  ");
        let second_exceeded = tracker.observe("same failure");

        // Assert
        assert!(!first_exceeded);
        assert!(second_exceeded);
    }

    #[test]
    fn test_failure_tracker_observe_empty_fingerprint_resets_streak() {
        // Arrange
        let mut tracker = FailureTracker::new(1);
        let _ = tracker.observe("same");

        // Act
        let empty_exceeded = tracker.observe("  ");
        let next_exceeded = tracker.observe("same");

        // Assert
        assert!(!empty_exceeded);
        assert!(!next_exceeded);
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
    fn test_format_detail_lines_trims_lines_and_returns_empty_for_blank_detail() {
        // Arrange
        let detail = " line one \n\tline two\t";
        let blank_detail = " \n\n\t";

        // Act
        let formatted = format_detail_lines(detail);
        let blank_formatted = format_detail_lines(blank_detail);

        // Assert
        assert_eq!(formatted, "- line one\n- line two");
        assert_eq!(blank_formatted, "");
    }
}
