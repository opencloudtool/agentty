//! CLI subprocess [`AgentChannel`] adapter.
//!
//! Spawns a provider CLI process per turn, streams stdout line-by-line as
//! [`TurnEvent`]s, and parses the final process output when the process exits.

use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::domain::agent::AgentKind;
use crate::infra::agent::{self as agent, AgentBackend, BuildCommandRequest};
use crate::infra::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};

/// [`AgentChannel`] adapter that spawns one CLI subprocess per agent turn.
///
/// Stdout lines are classified by
/// [`agent::parse_stream_output_line`] and forwarded as
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
        let backend = Arc::from(agent::create_backend(kind));

        Self { backend, kind }
    }

    /// Creates a CLI channel backed by the given pre-built backend.
    ///
    /// Channel factories use this helper so transport selection can be done
    /// once before constructing the concrete channel. Tests also use it to
    /// inject a [`MockAgentBackend`] that controls command construction and
    /// process spawning without relying on a real provider binary.
    pub(crate) fn with_backend(backend: Arc<dyn agent::AgentBackend>, kind: AgentKind) -> Self {
        Self { backend, kind }
    }
}

/// Builds the provider backend command request for one CLI turn.
fn build_command_request(request: &TurnRequest) -> BuildCommandRequest<'_> {
    BuildCommandRequest {
        attachments: &request.prompt.attachments,
        folder: &request.folder,
        prompt: &request.prompt.text,
        request_kind: &request.request_kind,
        model: &request.model,
        reasoning_level: request.reasoning_level,
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
        let build_request = build_command_request(&req);
        let build_result = self.backend.build_command(build_request);
        let stdin_payload_result = agent::build_command_stdin_payload(self.kind, build_request);
        let kind = self.kind;

        Box::pin(async move {
            let command = build_result
                .map_err(|error| AgentError(format!("Failed to build command: {error}")))?;
            let stdin_payload = stdin_payload_result.map_err(|error| {
                AgentError(format!("Failed to build command stdin payload: {error}"))
            })?;

            let mut tokio_cmd = tokio::process::Command::from(command);
            tokio_cmd.stdin(if stdin_payload.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            });
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
            let stdin_write_task = spawn_optional_stdin_write(child.stdin.take(), stdin_payload);

            let _ = stdout_task.await;
            let _ = stderr_task.await;

            let exit_status = child.wait().await.ok();
            await_optional_stdin_write(stdin_write_task).await?;

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
            if exit_status.as_ref().is_some_and(|status| !status.success()) {
                return Err(format_cli_turn_exit_error(
                    kind,
                    exit_status.and_then(|status| status.code()),
                    &stdout_text,
                    &stderr_text,
                ));
            }

            let parsed = agent::parse_response(kind, &stdout_text, &stderr_text);
            let assistant_message = agent::parse_turn_response(
                kind,
                &parsed.content,
                req.request_kind.protocol_profile(),
            )
            .map_err(AgentError)?;

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

/// Starts one background stdin writer when the child needs prompt input.
fn spawn_optional_stdin_write(
    child_stdin: Option<tokio::process::ChildStdin>,
    stdin_payload: Option<Vec<u8>>,
) -> Option<JoinHandle<Result<(), AgentError>>> {
    stdin_payload.map(|stdin_payload| {
        tokio::spawn(async move { write_optional_stdin(child_stdin, stdin_payload).await })
    })
}

/// Waits for one optional background stdin writer to finish.
///
/// # Errors
/// Returns an error when the writer task fails or panics before the full
/// prompt payload is delivered.
async fn await_optional_stdin_write(
    stdin_write_task: Option<JoinHandle<Result<(), AgentError>>>,
) -> Result<(), AgentError> {
    let Some(stdin_write_task) = stdin_write_task else {
        return Ok(());
    };

    stdin_write_task
        .await
        .map_err(|error| AgentError(format!("stdin write task failed: {error}")))?
}

/// Writes one optional stdin payload into the spawned CLI subprocess.
///
/// # Errors
/// Returns an error when stdin was requested but unavailable or writing the
/// payload fails.
async fn write_optional_stdin(
    child_stdin: Option<tokio::process::ChildStdin>,
    stdin_payload: Vec<u8>,
) -> Result<(), AgentError> {
    let mut child_stdin =
        child_stdin.ok_or_else(|| AgentError("stdin pipe unavailable after spawn".to_string()))?;
    if let Err(error) = child_stdin.write_all(&stdin_payload).await
        && !is_broken_pipe_error(&error)
    {
        return Err(AgentError(format!(
            "Failed to write stdin payload: {error}"
        )));
    }
    if let Err(error) = child_stdin.shutdown().await
        && !is_broken_pipe_error(&error)
    {
        return Err(AgentError(format!(
            "Failed to close stdin payload: {error}"
        )));
    }

    Ok(())
}

/// Returns whether one stdin write error is the expected closed-pipe case.
fn is_broken_pipe_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::BrokenPipe
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

        let Some((text, is_response_content)) = agent::parse_stream_output_line(kind, &line) else {
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
    agent::protocol::normalize_stream_assistant_chunk(text)
}

/// Formats one failed CLI turn into a user-facing error.
fn format_cli_turn_exit_error(
    kind: AgentKind,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> AgentError {
    if let Some(guidance) = known_cli_turn_exit_guidance(kind, stdout, stderr) {
        return AgentError(guidance);
    }

    let exit_code = exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    let output_detail = cli_turn_output_detail(stdout, stderr);

    AgentError(format!(
        "Agent command failed with exit code {exit_code}: {output_detail}"
    ))
}

/// Returns provider-specific guidance for known CLI turn failures.
fn known_cli_turn_exit_guidance(kind: AgentKind, stdout: &str, stderr: &str) -> Option<String> {
    match kind {
        AgentKind::Claude if is_claude_authentication_error(stdout, stderr) => {
            Some(claude_authentication_error_message())
        }
        AgentKind::Claude | AgentKind::Codex | AgentKind::Gemini => None,
    }
}

/// Builds the actionable Claude authentication refresh guidance message.
fn claude_authentication_error_message() -> String {
    "Claude command failed because authentication expired or is missing.\nRun `claude auth login` \
     to refresh your Anthropic session, verify with `claude auth status`, then retry."
        .to_string()
}

/// Detects Claude CLI authentication failures surfaced through stdout/stderr.
fn is_claude_authentication_error(stdout: &str, stderr: &str) -> bool {
    let combined_output = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    combined_output.contains("oauth token has expired")
        || combined_output.contains("failed to authenticate")
        || combined_output.contains("authentication_error")
}

/// Formats captured stdout/stderr into one compact CLI turn error detail.
fn cli_turn_output_detail(stdout: &str, stderr: &str) -> String {
    let trimmed_stdout = stdout.trim();
    let trimmed_stderr = stderr.trim();

    match (trimmed_stdout.is_empty(), trimmed_stderr.is_empty()) {
        (false, false) => format!("stdout: {trimmed_stdout}; stderr: {trimmed_stderr}"),
        (false, true) => format!("stdout: {trimmed_stdout}"),
        (true, false) => format!("stderr: {trimmed_stderr}"),
        (true, true) => "no output".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::domain::agent::{AgentKind, ReasoningLevel};
    use crate::infra::agent::tests::MockAgentBackend;
    use crate::infra::channel::{AgentRequestKind, TurnPrompt, TurnPromptAttachment};

    fn make_turn_request(folder: PathBuf) -> TurnRequest {
        TurnRequest {
            folder,
            live_session_output: None,
            model: "claude-sonnet-4-6".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "Write a test".into(),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
        }
    }

    fn stdin_capture_command(capture_path: &std::path::Path) -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(
            "cat > \"$CLI_CAPTURE_PATH\"; printf '%s' \
             '{\"answer\":\"ok\",\"questions\":[],\"summary\":null}'",
        );
        command.env("CLI_CAPTURE_PATH", capture_path);

        command
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
                .arg("printf '{\"answer\":\"ok\",\"questions\":[],\"summary\":null}'");

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
    /// Verifies session-turn Claude responses synthesize an empty summary when
    /// the provider returns `summary: null`.
    async fn test_run_turn_fills_missing_summary_for_session_turn() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg("printf '{\"answer\":\"ok\",\"questions\":[],\"summary\":null}'");

            Ok(command)
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
            result.assistant_message.summary,
            Some(agent::protocol::AgentResponseSummary {
                turn: String::new(),
                session: String::new(),
            })
        );
    }

    #[tokio::test]
    /// Verifies Claude CLI turns avoid deadlock when the child emits stderr
    /// before it starts reading a large stdin prompt.
    async fn test_run_turn_writes_large_stdin_concurrently_for_claude() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(
                "printf 'warming up\\n' >&2; sleep 0.1; cat >/dev/null; printf '%s' \
                 '{\"answer\":\"ok\",\"questions\":[],\"summary\":null}'",
            );

            Ok(command)
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let mut req = make_turn_request(dir.path().to_path_buf());
        req.prompt = "x".repeat(512 * 1024).into();

        // Act
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            channel.run_turn("sess-1".to_string(), req, events_tx),
        )
        .await
        .expect("turn should not deadlock")
        .expect("turn should succeed");

        // Assert
        assert_eq!(result.assistant_message.to_display_text(), "ok");
    }

    #[tokio::test]
    /// Verifies Claude CLI turns stream image-aware prompt text through stdin
    /// so large multimodal session prompts do not rely on argv transport.
    async fn test_run_turn_writes_prompt_to_stdin_for_claude() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let capture_path = dir.path().join("stdin.txt");
        let image_path = dir.path().join("pasted-image.png");
        std::fs::write(&image_path, b"image-bytes").expect("image should be written");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning({
            let capture_path = capture_path.clone();

            move |_| Ok(stdin_capture_command(&capture_path))
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let mut req = make_turn_request(dir.path().to_path_buf());
        req.prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: image_path.clone(),
            }],
            text: "Review [Image #1]".to_string(),
        };

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect("turn should succeed");
        let captured_prompt =
            std::fs::read_to_string(&capture_path).expect("captured stdin payload should exist");

        // Assert
        assert_eq!(result.assistant_message.to_display_text(), "ok");
        assert!(captured_prompt.contains("Structured response protocol:"));
        assert!(captured_prompt.contains(image_path.to_string_lossy().as_ref()));
        assert!(!captured_prompt.contains("[Image #1]"));
    }

    #[tokio::test]
    /// Verifies a broken stdin pipe does not hide the backend stderr or exit
    /// status when the CLI exits before consuming the full prompt.
    async fn test_run_turn_preserves_child_error_after_broken_pipe() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg("printf 'auth failed' >&2; exit 9");

            Ok(command)
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let mut req = make_turn_request(dir.path().to_path_buf());
        req.prompt = "x".repeat(512 * 1024).into();

        // Act
        let error = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect_err("turn should surface the child exit");

        // Assert
        assert!(error.0.contains("auth failed"), "error was: {}", error.0);
        assert!(
            !error.0.contains("stdin payload"),
            "stdin write error should not mask child failure: {}",
            error.0
        );
    }

    #[tokio::test]
    /// Verifies Claude turns surface invalid structured output instead of
    /// starting a repair retry.
    async fn test_run_turn_returns_error_for_invalid_structured_output_for_claude() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::SessionStart
                ));

                let mut command = std::process::Command::new("sh");
                command.arg("-c").arg("printf 'plain non-json response'");

                Ok(command)
            });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let error = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect_err("invalid structured output should fail");

        // Assert
        assert!(error.0.contains("did not match the required JSON schema"));
        assert!(error.0.contains("response:\nplain non-json response"));
    }

    #[tokio::test]
    /// Verifies non-zero CLI turn exits surface actionable Claude
    /// re-authentication guidance instead of protocol schema errors.
    async fn test_run_turn_returns_claude_auth_guidance_for_expired_token() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().times(1).returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(
                "printf '%s' \
                 '{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"\
                 OAuth token has expired. Please obtain a new token or refresh your existing \
                 token.\"}}'; exit 1",
            );

            Ok(command)
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let error = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect_err("expired Claude auth should fail")
            .0;

        // Assert
        assert!(error.contains("Claude command failed because authentication expired"));
        assert!(error.contains("`claude auth login`"));
        assert!(error.contains("`claude auth status`"));
    }

    #[tokio::test]
    /// Verifies non-zero CLI turn exits preserve generic stderr details for
    /// non-authentication failures.
    async fn test_run_turn_returns_exit_error_for_non_zero_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().times(1).returning(|_| {
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg("printf '%s' 'assist failed' >&2; exit 7");

            Ok(command)
        });
        let channel = CliAgentChannel {
            backend: Arc::new(mock_backend),
            kind: AgentKind::Claude,
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let req = make_turn_request(dir.path().to_path_buf());

        // Act
        let error = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect_err("non-zero exit should fail")
            .0;

        // Assert
        assert!(error.contains("Agent command failed with exit code 7"));
        assert!(error.contains("assist failed"));
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
                r#"echo '{"result":"{\"answer\":\"final answer\",\"questions\":[],\"summary\":null}","usage":{"input_tokens":5,"output_tokens":3}}'"#,
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
        let text = r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}],"summary":null}"#;

        // Act
        let normalized_text = normalize_stream_response_content(text);

        // Assert
        assert_eq!(normalized_text, Some("Done.".to_string()));
    }

    #[test]
    /// Verifies structured JSON chunks without `answer` text are suppressed.
    fn test_normalize_stream_response_content_suppresses_question_only_payload() {
        // Arrange
        let text = r#"{"answer":"","questions":[{"text":"Need clarification.","options":[]}],"summary":null}"#;

        // Act
        let normalized_text = normalize_stream_response_content(text);

        // Assert
        assert_eq!(normalized_text, None);
    }

    #[test]
    /// Verifies partial protocol JSON chunks are suppressed during streaming.
    fn test_normalize_stream_response_content_suppresses_json_fragment() {
        // Arrange
        let text = r#"{"answer":"#;

        // Act
        let normalized_text = normalize_stream_response_content(text);

        // Assert
        assert_eq!(normalized_text, None);
    }
}
