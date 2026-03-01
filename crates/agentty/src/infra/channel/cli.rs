//! CLI subprocess [`AgentChannel`] adapter.
//!
//! Spawns a provider CLI process per turn, streams stdout line-by-line as
//! [`TurnEvent`]s, and parses the final process output when the process exits.

use std::os::unix::process::ExitStatusExt as _;
use std::sync::{Arc, Mutex};

use tokio::io::AsyncBufReadExt as _;
use tokio::sync::mpsc;

use crate::domain::agent::AgentKind;
use crate::infra::agent::{AgentBackend, AgentCommandMode, BuildCommandRequest};
use crate::infra::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnMode,
    TurnRequest, TurnResult,
};

/// [`AgentChannel`] adapter that spawns one CLI subprocess per agent turn.
///
/// Stdout lines are classified by
/// [`crate::infra::agent::parse_stream_output_line`] and forwarded as
/// [`TurnEvent::AssistantDelta`] or [`TurnEvent::Progress`]. A kill signal
/// transitions the turn to a failed state with a `[Stopped]` banner. A spawn
/// failure is surfaced through [`AgentError`].
pub struct CliAgentChannel {
    /// Provider-specific command builder.
    backend: Arc<dyn AgentBackend>,
    /// Provider family used for stream and response parsing.
    kind: AgentKind,
}

impl CliAgentChannel {
    /// Creates a new CLI channel for the given agent provider.
    pub fn new(kind: AgentKind) -> Self {
        let backend = Arc::from(crate::infra::agent::create_backend(kind));

        Self { backend, kind }
    }

    /// Creates a CLI channel backed by the given pre-built backend.
    ///
    /// Used in tests to inject a [`MockAgentBackend`] that controls command
    /// construction and process spawning without relying on a real provider
    /// binary.
    #[cfg(test)]
    pub(crate) fn with_backend(
        backend: Arc<dyn crate::infra::agent::AgentBackend>,
        kind: AgentKind,
    ) -> Self {
        Self { backend, kind }
    }
}

impl AgentChannel for CliAgentChannel {
    /// Returns a [`SessionRef`] immediately; CLI turns are stateless.
    fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> AgentFuture<Result<SessionRef, AgentError>> {
        let session_id = req.session_id;

        Box::pin(async move { Ok(SessionRef { session_id }) })
    }

    /// Spawns a CLI process for the turn and streams its output as events.
    ///
    /// Stdout lines are parsed with the provider-specific stream parser and
    /// forwarded as [`TurnEvent::AssistantDelta`] (response content) or
    /// [`TurnEvent::Progress`] (tool-use labels, thinking labels). After the
    /// process exits, usage statistics are extracted from the raw stdout/stderr
    /// and returned in [`TurnResult`].
    ///
    /// # Errors
    /// Returns [`AgentError`] when command construction fails, the process
    /// cannot be spawned, or the process is killed by a signal.
    fn run_turn(
        &self,
        _session_id: String,
        req: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        let build_result = self.backend.build_command(BuildCommandRequest {
            folder: &req.folder,
            mode: match &req.mode {
                TurnMode::Start => AgentCommandMode::Start {
                    prompt: &req.prompt,
                },
                TurnMode::Resume { session_output } => AgentCommandMode::Resume {
                    prompt: &req.prompt,
                    session_output: session_output.as_deref(),
                },
            },
            model: &req.model,
        });
        let kind = self.kind;

        Box::pin(async move {
            let command = build_result.map_err(|error| {
                let message = format!("Failed to build command: {error}\n");
                let _ = events.send(TurnEvent::AssistantDelta(message.clone()));

                AgentError(message.trim_end().to_string())
            })?;

            let mut tokio_cmd = tokio::process::Command::from(command);
            tokio_cmd.stdin(std::process::Stdio::null());
            tokio_cmd.stdout(std::process::Stdio::piped());
            tokio_cmd.stderr(std::process::Stdio::piped());

            let mut child = tokio_cmd.spawn().map_err(|error| {
                let message = format!("Failed to spawn process: {error}\n");
                let _ = events.send(TurnEvent::AssistantDelta(message.clone()));

                AgentError(message.trim_end().to_string())
            })?;

            // Notify the consumer of the child PID so cancellation signals can
            // be sent while the process is running.
            let _ = events.send(TurnEvent::PidUpdate(child.id()));

            let raw_stdout = Arc::new(Mutex::new(String::new()));
            let raw_stderr = Arc::new(Mutex::new(String::new()));

            let stdout_task = {
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| AgentError("stdout pipe unavailable after spawn".to_string()))?;
                let raw_stdout = Arc::clone(&raw_stdout);
                let events = events.clone();

                tokio::spawn(stream_stdout(stdout, kind, events, raw_stdout))
            };

            let stderr_task = {
                let stderr = child
                    .stderr
                    .take()
                    .ok_or_else(|| AgentError("stderr pipe unavailable after spawn".to_string()))?;
                let raw_stderr = Arc::clone(&raw_stderr);

                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if let Ok(mut buf) = raw_stderr.lock() {
                            buf.push_str(&line);
                            buf.push('\n');
                        }
                    }
                })
            };

            let _ = stdout_task.await;
            let _ = stderr_task.await;

            let exit_status = child.wait().await.ok();

            // Clear the PID slot now that the child has exited.
            let _ = events.send(TurnEvent::PidUpdate(None));

            let killed_by_signal = exit_status
                .as_ref()
                .is_some_and(|status| status.signal().is_some());

            if killed_by_signal {
                let message = "\n[Stopped] Agent interrupted by user.\n";
                let _ = events.send(TurnEvent::AssistantDelta(message.to_string()));

                return Err(AgentError(
                    "[Stopped] Agent interrupted by user.".to_string(),
                ));
            }

            let stdout_text = raw_stdout.lock().map(|buf| buf.clone()).unwrap_or_default();
            let stderr_text = raw_stderr.lock().map(|buf| buf.clone()).unwrap_or_default();
            let parsed = crate::infra::agent::parse_response(kind, &stdout_text, &stderr_text);

            Ok(TurnResult {
                assistant_message: parsed.content,
                context_reset: false,
                input_tokens: parsed.stats.input_tokens,
                output_tokens: parsed.stats.output_tokens,
            })
        })
    }

    /// No-op; CLI sessions are stateless and require no teardown.
    fn shutdown_session(&self, _session_id: String) -> AgentFuture<Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Reads stdout line-by-line, classifying each line and forwarding events.
///
/// Recognized response-content lines produce [`TurnEvent::AssistantDelta`];
/// progress lines produce [`TurnEvent::Progress`]. Unrecognized lines and
/// empty-after-trim results are silently skipped. The raw bytes are also
/// accumulated in `raw_buffer` for final response parsing after the process
/// exits.
async fn stream_stdout(
    stdout: tokio::process::ChildStdout,
    kind: AgentKind,
    events: mpsc::UnboundedSender<TurnEvent>,
    raw_buffer: Arc<Mutex<String>>,
) {
    let mut reader = tokio::io::BufReader::new(stdout).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        if let Ok(mut buf) = raw_buffer.lock() {
            buf.push_str(&line);
            buf.push('\n');
        }

        let Some((text, is_response_content)) =
            crate::infra::agent::parse_stream_output_line(kind, &line)
        else {
            continue;
        };

        if text.trim().is_empty() {
            continue;
        }

        let event = if is_response_content {
            TurnEvent::AssistantDelta(text)
        } else {
            TurnEvent::Progress(text)
        };

        let _ = events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::domain::agent::AgentKind;
    use crate::infra::agent::tests::MockAgentBackend;
    use crate::infra::channel::TurnMode;

    fn make_turn_request(folder: PathBuf) -> TurnRequest {
        TurnRequest {
            folder,
            live_session_output: None,
            model: "claude-sonnet-4-6".to_string(),
            mode: TurnMode::Start,
            prompt: "Write a test".to_string(),
        }
    }

    #[tokio::test]
    /// Verifies spawn failure emits an `AssistantDelta` with the error text
    /// and returns `Err`.
    async fn test_run_turn_spawn_failure_emits_error_delta_and_returns_err() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend
            .expect_build_command()
            .returning(|_| Ok(std::process::Command::new("/no-such-binary-agentty-test")));
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let result = channel.run_turn("sess-1".to_string(), req, events_tx).await;

        // Assert
        let error_message = result.expect_err("expected Err for spawn failure").0;
        assert!(
            error_message.contains("Failed to spawn process"),
            "error was: {error_message}"
        );
        let event = events_rx.try_recv().expect("should have received an event");
        assert!(
            matches!(&event, TurnEvent::AssistantDelta(msg) if msg.contains("Failed to spawn")),
            "unexpected event: {event:?}"
        );
    }

    #[tokio::test]
    /// Verifies kill-by-signal emits a `[Stopped]` `AssistantDelta` event and
    /// returns `Err`.
    async fn test_run_turn_kill_signal_emits_stopped_delta_and_returns_err() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("kill -9 $$");

            Ok(cmd)
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let result = channel.run_turn("sess-1".to_string(), req, events_tx).await;

        // Assert
        let error_message = result.expect_err("expected Err for kill-by-signal").0;
        assert!(
            error_message.contains("[Stopped]"),
            "error was: {error_message}"
        );

        // Drain `PidUpdate` events emitted when the process spawns and exits
        // before asserting on the `[Stopped]` delta.
        let delta_event = loop {
            let event = events_rx
                .try_recv()
                .expect("expected at least one more event");
            if !matches!(event, TurnEvent::PidUpdate(_)) {
                break event;
            }
        };
        assert!(
            matches!(&delta_event, TurnEvent::AssistantDelta(msg) if msg.contains("[Stopped]")),
            "unexpected event: {delta_event:?}"
        );
    }

    #[tokio::test]
    /// Verifies that a clean process exit returns `Ok(TurnResult)` with no
    /// context reset (CLI turns never reset context).
    async fn test_run_turn_clean_exit_returns_ok_result_without_context_reset() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend
            .expect_build_command()
            .returning(|_| Ok(std::process::Command::new("true")));
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let result = channel.run_turn("sess-1".to_string(), req, events_tx).await;

        // Assert
        let turn_result = result.expect("expected Ok for clean exit");
        assert!(!turn_result.context_reset);
    }
}
