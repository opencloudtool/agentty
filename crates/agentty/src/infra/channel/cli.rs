//! CLI subprocess [`AgentChannel`] adapter.
//!
//! Spawns a provider CLI process per turn, streams stdout line-by-line as
//! [`TurnEvent`]s, and parses the final process output when the process exits.

use std::os::unix::process::ExitStatusExt as _;
use std::sync::{Arc, Mutex};

use tokio::io::AsyncBufReadExt as _;
use tokio::sync::mpsc;

use crate::domain::agent::{AgentKind, ReasoningLevel};
use crate::infra::agent::protocol::{
    build_protocol_repair_prompt, normalize_stream_assistant_chunk, parse_agent_response,
    parse_agent_response_strict,
};
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
        let backend = Arc::clone(&self.backend);
        let build_result = self.backend.build_command(BuildCommandRequest {
            reasoning_level: req.reasoning_level,
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
        let reasoning_level = req.reasoning_level;
        let folder = req.folder;
        let model = req.model;

        Box::pin(async move {
            let command = build_result
                .map_err(|error| AgentError(format!("Failed to build command: {error}")))?;

            let mut tokio_cmd = tokio::process::Command::from(command);
            tokio_cmd.stdin(std::process::Stdio::null());
            tokio_cmd.stdout(std::process::Stdio::piped());
            tokio_cmd.stderr(std::process::Stdio::piped());

            let mut child = tokio_cmd
                .spawn()
                .map_err(|error| AgentError(format!("Failed to spawn process: {error}")))?;

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
                return Err(AgentError(
                    "[Stopped] Agent interrupted by user.".to_string(),
                ));
            }

            let stdout_text = raw_stdout.lock().map(|buf| buf.clone()).unwrap_or_default();
            let stderr_text = raw_stderr.lock().map(|buf| buf.clone()).unwrap_or_default();
            let parsed = crate::infra::agent::parse_response(kind, &stdout_text, &stderr_text);
            let assistant_message = parse_or_repair_structured_response(
                backend,
                kind,
                reasoning_level,
                &folder,
                &model,
                &parsed.content,
            )
            .await?;

            Ok(TurnResult {
                assistant_message,
                context_reset: false,
                input_tokens: parsed.stats.input_tokens,
                output_tokens: parsed.stats.output_tokens,
                provider_conversation_id: None,
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
///
/// Response content is streamed live through the best-effort heuristic
/// normalizer. Partial protocol JSON fragments may leak through, but the
/// worker layer always appends the clean final parsed response at turn
/// completion so the authoritative output is present regardless.
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

        let event = if is_response_content {
            let Some(normalized_text) = normalize_stream_response_content(&text) else {
                continue;
            };
            if normalized_text.trim().is_empty() {
                continue;
            }

            TurnEvent::AssistantDelta(normalized_text)
        } else {
            if text.trim().is_empty() {
                continue;
            }

            TurnEvent::Progress(text)
        };

        let _ = events.send(event);
    }
}

/// Normalizes one response-content stream chunk before transcript emission.
///
/// For structured protocol chunks that already contain a complete JSON payload,
/// this strips the protocol wrapper and returns only `answer` display text.
///
/// Partial protocol JSON fragments are suppressed so raw JSON does not leak
/// into session output while streaming.
fn normalize_stream_response_content(text: &str) -> Option<String> {
    normalize_stream_assistant_chunk(text)
}

/// Parses one final assistant payload, optionally repairing malformed
/// structured output for strict providers.
async fn parse_or_repair_structured_response(
    backend: Arc<dyn AgentBackend>,
    kind: AgentKind,
    reasoning_level: ReasoningLevel,
    folder: &std::path::Path,
    model: &str,
    response_text: &str,
) -> Result<crate::infra::agent::AgentResponse, AgentError> {
    if !requires_strict_structured_output(kind) {
        return Ok(parse_agent_response(response_text));
    }

    if let Ok(parsed_response) = parse_agent_response_strict(response_text) {
        return Ok(parsed_response);
    }

    let repair_content = run_structured_output_repair_turn(
        backend,
        kind,
        reasoning_level,
        folder,
        model,
        response_text,
    )
    .await?;

    parse_agent_response_strict(&repair_content).map_err(|error| {
        AgentError(format!(
            "Agent output did not match the required JSON schema after one repair attempt: {error}"
        ))
    })
}

/// Runs one best-effort repair turn that asks the model to re-emit valid
/// protocol JSON only.
async fn run_structured_output_repair_turn(
    backend: Arc<dyn AgentBackend>,
    kind: AgentKind,
    reasoning_level: ReasoningLevel,
    folder: &std::path::Path,
    model: &str,
    invalid_response: &str,
) -> Result<String, AgentError> {
    let repair_prompt = build_protocol_repair_prompt(invalid_response);
    let repair_command = backend
        .build_command(BuildCommandRequest {
            reasoning_level,
            folder,
            mode: AgentCommandMode::Start {
                prompt: &repair_prompt,
            },
            model,
        })
        .map_err(|error| AgentError(format!("Failed to build repair command: {error}")))?;

    let mut tokio_command = tokio::process::Command::from(repair_command);
    tokio_command.stdin(std::process::Stdio::null());
    tokio_command.stdout(std::process::Stdio::piped());
    tokio_command.stderr(std::process::Stdio::piped());

    let output = tokio_command
        .output()
        .await
        .map_err(|error| AgentError(format!("Failed to run repair command: {error}")))?;

    if output.status.signal().is_some() {
        return Err(AgentError(
            "[Stopped] Agent interrupted during structured output repair.".to_string(),
        ));
    }

    let stdout_text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr_text = String::from_utf8_lossy(&output.stderr).into_owned();
    let parsed = crate::infra::agent::parse_response(kind, &stdout_text, &stderr_text);

    Ok(parsed.content)
}

/// Returns whether the provider should fail closed on invalid structured
/// output instead of falling back to plain text.
fn requires_strict_structured_output(kind: AgentKind) -> bool {
    matches!(kind, AgentKind::Claude | AgentKind::Gemini)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::domain::agent::{AgentKind, ReasoningLevel};
    use crate::infra::agent::tests::MockAgentBackend;
    use crate::infra::channel::TurnMode;

    fn make_turn_request(folder: PathBuf) -> TurnRequest {
        TurnRequest {
            reasoning_level: ReasoningLevel::default(),
            folder,
            live_session_output: None,
            model: "claude-sonnet-4-6".to_string(),
            mode: TurnMode::Start,
            prompt: "Write a test".to_string(),
            provider_conversation_id: None,
        }
    }

    #[tokio::test]
    /// Verifies spawn failure returns `Err` with a descriptive message and
    /// does not emit any `AssistantDelta` events (the worker appends the
    /// error to session output once via `apply_turn_result`).
    async fn test_run_turn_spawn_failure_returns_err_without_delta() {
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
        assert!(
            events_rx.try_recv().is_err(),
            "no events should be emitted when the process never spawned"
        );
    }

    #[tokio::test]
    /// Verifies kill-by-signal returns `Err` with a `[Stopped]` message and
    /// does not emit an `AssistantDelta` event (the worker appends the error
    /// to session output once via `apply_turn_result`).
    async fn test_run_turn_kill_signal_returns_err_without_stopped_delta() {
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

        // Drain `PidUpdate` events and verify no `AssistantDelta` was emitted.
        while let Ok(event) = events_rx.try_recv() {
            assert!(
                matches!(event, TurnEvent::PidUpdate(_)),
                "only PidUpdate events expected, got: {event:?}"
            );
        }
    }

    #[tokio::test]
    /// Verifies that a clean process exit returns `Ok(TurnResult)` with no
    /// context reset (CLI turns never reset context).
    async fn test_run_turn_clean_exit_returns_ok_result_without_context_reset() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg("printf '{\"messages\":[{\"type\":\"answer\",\"text\":\"ok\"}]}'");

            Ok(command)
        });
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

    #[tokio::test]
    /// Verifies Claude turns run one repair retry when final output is not
    /// valid structured JSON.
    async fn test_run_turn_repairs_invalid_structured_output_for_claude() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let call_count = Arc::new(AtomicUsize::new(0));
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().times(2).returning({
            let call_count = Arc::clone(&call_count);
            move |request| {
                let call_index = call_count.fetch_add(1, Ordering::SeqCst);
                let mut command = std::process::Command::new("sh");
                command.arg("-c");

                if call_index == 0 {
                    command.arg("printf 'plain non-json response'");
                } else {
                    assert!(
                        matches!(request.mode, AgentCommandMode::Start { .. }),
                        "repair turn should use start mode"
                    );
                    assert!(
                        request
                            .mode
                            .prompt()
                            .contains("did not match the required JSON schema")
                    );
                    command.arg(
                        "printf '{\"messages\":[{\"type\":\"answer\",\"text\":\"Recovered \
                         output\"}]}'",
                    );
                }

                Ok(command)
            }
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(
            result.assistant_message.to_display_text(),
            "Recovered output"
        );
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    /// Verifies strict protocol providers stream assistant deltas live and
    /// forward progress events alongside them. The worker layer appends
    /// the clean final parsed response at turn completion.
    async fn test_run_turn_streams_deltas_and_progress_for_strict_protocol_provider() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(concat!(
                r#"echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]}}';"#,
                r#"echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"streamed fragment"}]}}';"#,
                r#"echo '{"result":"{\"messages\":[{\"type\":\"answer\",\"text\":\"final answer\"}]}","usage":{"input_tokens":5,"output_tokens":3}}'"#,
            ));

            Ok(command)
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        let mut saw_progress = false;
        let mut saw_delta = false;
        while let Ok(event) = events_rx.try_recv() {
            match event {
                TurnEvent::AssistantDelta(_) => saw_delta = true,
                TurnEvent::Progress(label) => {
                    saw_progress = true;
                    assert_eq!(label, "Running a command");
                }
                _ => {}
            }
        }
        assert!(saw_progress, "progress events should be forwarded");
        assert!(saw_delta, "assistant deltas should be streamed live");
        assert_eq!(result.assistant_message.to_display_text(), "final answer");
    }

    #[test]
    /// Verifies plain stream chunks are returned unchanged.
    fn test_normalize_stream_response_content_keeps_plain_text() {
        // Arrange
        let text = "Plain response line";

        // Act
        let normalized_text = normalize_stream_response_content(text);

        // Assert
        assert_eq!(normalized_text, Some(text.to_string()));
    }

    #[test]
    /// Verifies structured JSON stream chunks keep only `answer` text.
    fn test_normalize_stream_response_content_keeps_answer_text_only() {
        // Arrange
        let text = r#"{"messages":[{"type":"answer","text":"Done."},{"type":"question","text":"Need clarification."}]}"#;

        // Act
        let normalized_text = normalize_stream_response_content(text);

        // Assert
        assert_eq!(normalized_text, Some("Done.".to_string()));
    }

    #[test]
    /// Verifies structured JSON chunks without `answer` text are suppressed.
    fn test_normalize_stream_response_content_suppresses_question_only_payload() {
        // Arrange
        let text = r#"{"messages":[{"type":"question","text":"Need clarification."}]}"#;

        // Act
        let normalized_text = normalize_stream_response_content(text);

        // Assert
        assert_eq!(normalized_text, None);
    }

    #[test]
    /// Verifies partial protocol JSON chunks are suppressed during streaming.
    fn test_normalize_stream_response_content_suppresses_json_fragment() {
        // Arrange
        let text = r#"{"messages":[{"type":"answer","#;

        // Act
        let normalized_text = normalize_stream_response_content(text);

        // Assert
        assert_eq!(normalized_text, None);
    }
}
