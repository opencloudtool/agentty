//! CLI subprocess [`AgentChannel`] adapter.
//!
//! Spawns a provider CLI process per turn, streams stdout line-by-line as
//! [`TurnEvent`]s, and parses the final process output when the process exits.

use std::sync::Arc;

use ag_protocol::{AgentResponse, TurnPrompt, build_protocol_repair_prompt};
use tokio::sync::mpsc;

use crate::agent::cli::error;
use crate::agent::cli::execution::{
    self, CliExecutionError, CliExecutionObserver, CliExitStatus, CollectingCliObserver,
};
use crate::agent::{self as agent, AgentBackend, BuildCommandRequest};
use crate::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};
use crate::model::agent::AgentKind;

/// [`AgentChannel`] adapter that spawns one CLI subprocess per agent turn.
///
/// Stdout lines are classified by
/// [`agent::parse_stream_output_line`] and transient loader updates are
/// forwarded as [`TurnEvent::ThoughtDelta`]. A kill signal transitions the
/// turn to a failed state with a `[Stopped]` banner. A spawn failure is
/// surfaced through [`AgentError`].
pub(crate) struct CliAgentChannel {
    /// Provider-specific command builder.
    backend: Arc<dyn AgentBackend>,
    /// Provider family used for stream and response parsing.
    kind: AgentKind,
}

impl CliAgentChannel {
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

/// Bridges raw CLI execution observations into session turn events.
struct CliTurnObserver {
    /// Session event sink receiving PID and thought updates.
    events: mpsc::UnboundedSender<TurnEvent>,
    /// Provider family used to classify streamed stdout lines.
    kind: AgentKind,
}

impl CliExecutionObserver for CliTurnObserver {
    fn pid_updated(&self, child_pid: Option<u32>) {
        let _ = self.events.send(TurnEvent::PidUpdate(child_pid));
    }

    fn stdout_line(&self, line: &str) {
        let Some((text, is_response_content)) = agent::parse_stream_output_line(self.kind, line)
        else {
            return;
        };
        if is_response_content {
            return;
        }

        let trimmed_text = text.trim();
        if trimmed_text.is_empty() {
            return;
        }

        let _ = self
            .events
            .send(TurnEvent::ThoughtDelta(trimmed_text.to_string()));
    }
}

/// Builds the provider backend command request for one CLI turn.
fn build_command_request<'a>(
    request: &'a TurnRequest,
    prompt_text: &'a str,
) -> BuildCommandRequest<'a> {
    BuildCommandRequest {
        attachments: &request.prompt.attachments,
        folder: &request.folder,
        main_checkout_root: request.main_checkout_root.as_deref(),
        replay_transcript: request.continuation.replay_transcript(),
        model: &request.model,
        permission_mode: request.permission_mode,
        personality_prompt: request.personality.current(),
        prompt: prompt_text,
        reasoning_level: request.reasoning_level,
        request_kind: &request.request_kind,
        speed_mode: request.speed_mode,
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
    /// loader-oriented interim text is forwarded as
    /// [`TurnEvent::ThoughtDelta`]. After the process exits, usage
    /// statistics are extracted from the raw stdout/stderr and the final
    /// parsed response is returned in [`TurnResult`].
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
        let kind = self.kind;
        let backend = Arc::clone(&self.backend);

        Box::pin(async move {
            let mut req = req;
            req.prompt = agent::apply_response_style_prompt(
                req.prompt,
                req.request_kind.protocol_profile(),
                req.response_style,
            )
            .map_err(|error| AgentError::Backend(error.to_string()))?;
            let prompt_text = req.prompt.agent_text();
            let replay = agent::replay::ReplayContext::prepare(
                req.folder.clone(),
                req.continuation.replay_transcript().map(str::to_owned),
            )
            .await
            .map_err(|error| AgentError::Backend(error.to_string()))?;
            let mut build_request = build_command_request(&req, &prompt_text);
            build_request.replay_transcript = replay.text.as_deref();
            let observer = CliTurnObserver {
                events: events.clone(),
                kind,
            };
            let output = execution::execute_cli_command(
                backend.as_ref(),
                kind,
                build_request,
                &observer,
                None,
            )
            .await
            .map_err(map_cli_turn_execution_error)?;

            match output.exit_status {
                CliExitStatus::Signaled(_) => {
                    return Err(AgentError::Backend(
                        "[Stopped] Agent interrupted by user.".to_string(),
                    ));
                }
                CliExitStatus::NonZero(exit_code) => {
                    return Err(format_cli_turn_exit_error(
                        kind,
                        exit_code,
                        &output.stdout,
                        &output.stderr,
                    ));
                }
                CliExitStatus::Success => {}
            }

            let parsed = agent::parse_response(kind, &output.stdout, &output.stderr);
            let assistant_message =
                parse_or_repair_cli_response(kind, &parsed.content, &req, &backend, &events)
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

/// Parses one CLI turn response strictly, falling back to a single
/// protocol-repair retry when the initial parse fails.
///
/// When repair is attempted, a concise [`TurnEvent::ThoughtDelta`] is emitted
/// so the user can see that schema repair is in progress without flooding the
/// session output with parser diagnostics unless the turn ultimately fails.
async fn parse_or_repair_cli_response(
    kind: AgentKind,
    content: &str,
    req: &TurnRequest,
    backend: &Arc<dyn AgentBackend>,
    events: &mpsc::UnboundedSender<TurnEvent>,
) -> Result<AgentResponse, AgentError> {
    let protocol_profile = req.request_kind.protocol_profile();

    let parse_error = match agent::parse_turn_response(kind, content, protocol_profile) {
        Ok(response) => return Ok(response),
        Err(error) => error,
    };

    let _ = events.send(TurnEvent::ThoughtDelta(format!(
        "Protocol parse error; retrying schema repair for {kind}."
    )));

    let repair_prompt =
        build_protocol_repair_prompt(&parse_error, content).map_err(AgentError::Backend)?;

    let repair_content = execute_cli_repair_turn(backend.as_ref(), kind, req, &repair_prompt)
        .await
        .map_err(|error| {
            AgentError::Backend(format!(
                "{parse_error}\nprotocol repair transport failed: {error}"
            ))
        })?;

    agent::parse_turn_response(kind, &repair_content, protocol_profile).map_err(|repair_error| {
        AgentError::Backend(format!(
            "{parse_error}\nprotocol repair retry also failed: {repair_error}"
        ))
    })
}

/// Maximum wall-clock time for one protocol-repair CLI subprocess.
///
/// Repair turns ask the agent to re-emit a single JSON object, so they
/// should complete quickly. The timeout prevents a hung subprocess from
/// blocking the parent turn indefinitely. `kill_on_drop(true)` on the
/// child ensures the process is terminated when the future is dropped.
const REPAIR_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

/// Spawns a fresh CLI process for one protocol-repair retry and returns
/// the parsed provider content string.
///
/// This helper strips down the full turn-execution pipeline to the minimum
/// needed for repair: command build, spawn, stdout/stderr collection, and
/// provider response parsing. No streaming, PID tracking, or signal
/// handling is performed because the repair is a transparent one-shot
/// correction, not a user-visible turn. A [`REPAIR_TURN_TIMEOUT`] guard
/// ensures a stuck process does not block the parent turn indefinitely.
async fn execute_cli_repair_turn(
    backend: &dyn AgentBackend,
    kind: AgentKind,
    request: &TurnRequest,
    repair_prompt: &str,
) -> Result<String, String> {
    let prompt_payload = TurnPrompt::from_agent_data(repair_prompt.to_string());
    let build_request = BuildCommandRequest {
        attachments: &prompt_payload.attachments,
        folder: &request.folder,
        main_checkout_root: None,
        replay_transcript: None,
        model: &request.model,
        permission_mode: crate::model::permission::PermissionMode::ReadOnly,
        personality_prompt: None,
        prompt: repair_prompt,
        reasoning_level: request.reasoning_level,
        request_kind: &request.request_kind,
        speed_mode: request.speed_mode,
    };
    execute_cli_repair_command(backend, kind, build_request, REPAIR_TURN_TIMEOUT).await
}

/// Executes one prepared repair command with an explicit deadline.
async fn execute_cli_repair_command(
    backend: &dyn AgentBackend,
    kind: AgentKind,
    build_request: BuildCommandRequest<'_>,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let output = execution::execute_cli_command(
        backend,
        kind,
        build_request,
        &CollectingCliObserver,
        Some(timeout),
    )
    .await
    .map_err(|error| format!("repair {error}"))?;

    match output.exit_status {
        CliExitStatus::NonZero(exit_code) => {
            return Err(format!(
                "repair process exited with code {}",
                exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string())
            ));
        }
        CliExitStatus::Signaled(signal) => {
            return Err(format!("repair process was interrupted by signal {signal}"));
        }
        CliExitStatus::Success => {}
    }

    let parsed = agent::parse_response(kind, &output.stdout, &output.stderr);

    Ok(parsed.content)
}

/// Maps shared execution failures into the existing channel error categories.
fn map_cli_turn_execution_error(error: CliExecutionError) -> AgentError {
    match error {
        CliExecutionError::CommandBuild(error) => {
            AgentError::Backend(format!("Failed to build command: {error}"))
        }
        CliExecutionError::Spawn(error) => {
            AgentError::Io(format!("Failed to spawn process: {error}"))
        }
        CliExecutionError::StdinBuild(error) => {
            AgentError::Backend(format!("Failed to build command stdin payload: {error}"))
        }
        error => AgentError::Io(error.to_string()),
    }
}

/// Formats one failed CLI turn into a user-facing error.
fn format_cli_turn_exit_error(
    kind: AgentKind,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> AgentError {
    AgentError::Backend(error::format_agent_cli_exit_error(
        kind,
        "Agent command",
        exit_code,
        stdout,
        stderr,
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use ag_protocol::{TurnPromptAttachment, TurnPromptTextSource};
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::MockAgentBackend;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::{AgentKind, ReasoningLevel};

    fn make_turn_request(folder: PathBuf) -> TurnRequest {
        TurnRequest {
            continuation: crate::channel::TurnContinuation::fresh(),
            folder,
            main_checkout_root: None,
            model: "claude-sonnet-5".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Write a test".into(),
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            response_style: crate::ResponseStyle::default(),
            speed_mode: crate::model::session::SpeedMode::default(),
        }
    }

    fn stdin_capture_command(capture_path: &std::path::Path) -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg("cat > \"$CLI_CAPTURE_PATH\"; printf '%s' '{\"answer\":\"ok\",\"questions\":[]}'");
        command.env("CLI_CAPTURE_PATH", capture_path);

        command
    }

    /// Drains all currently buffered turn events from a test receiver.
    #[test]
    fn test_map_cli_turn_execution_error_preserves_error_categories() {
        // Arrange
        let command_error = CliExecutionError::CommandBuild(
            crate::agent::AgentBackendError::CommandBuild("command".to_string()),
        );
        let spawn_error = CliExecutionError::Spawn(std::io::Error::other("spawn unavailable"));
        let stdin_error = CliExecutionError::StdinBuild(
            crate::agent::AgentBackendError::CommandBuild("stdin".to_string()),
        );
        let io_error = CliExecutionError::StdinWrite("write unavailable".to_string());

        // Act
        let command_message = map_cli_turn_execution_error(command_error).to_string();
        let spawn_message = map_cli_turn_execution_error(spawn_error).to_string();
        let stdin_message = map_cli_turn_execution_error(stdin_error).to_string();
        let io_message = map_cli_turn_execution_error(io_error).to_string();

        // Assert
        assert_eq!(command_message, "Failed to build command: command");
        assert_eq!(spawn_message, "Failed to spawn process: spawn unavailable");
        assert_eq!(
            stdin_message,
            "Failed to build command stdin payload: stdin"
        );
        assert_eq!(io_message, "stdin delivery failed: write unavailable");
    }

    #[test]
    fn test_cli_turn_observer_ignores_blank_progress_text() {
        // Arrange
        let (events, mut event_receiver) = mpsc::unbounded_channel();
        let observer = CliTurnObserver {
            events,
            kind: AgentKind::Codex,
        };

        // Act
        observer.stdout_line(r#"{"type":"item.updated","item":{"type":"reasoning","text":"   "}}"#);

        // Assert
        assert!(event_receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_parse_or_repair_cli_response_reports_repair_transport_failure() {
        // Arrange
        let folder = tempdir().expect("failed to create temp dir");
        let request = make_turn_request(folder.path().to_path_buf());
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg("exit 7");

            Ok(command)
        });
        let backend: Arc<dyn AgentBackend> = Arc::new(backend);
        let (events, mut event_receiver) = mpsc::unbounded_channel();

        // Act
        let error = parse_or_repair_cli_response(
            AgentKind::Codex,
            "not a protocol response",
            &request,
            &backend,
            &events,
        )
        .await
        .expect_err("a failing repair transport should fail the turn");

        // Assert
        assert!(
            error
                .to_string()
                .contains("protocol repair transport failed: repair process exited with code 7"),
            "unexpected repair error: {error}"
        );
        assert!(matches!(
            event_receiver.try_recv(),
            Ok(TurnEvent::ThoughtDelta(_))
        ));
    }

    #[tokio::test]
    async fn test_execute_cli_repair_turn_reports_non_zero_exit() {
        // Arrange
        let folder = tempdir().expect("failed to create temp dir");
        let request = make_turn_request(folder.path().to_path_buf());
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg("exit 7");

            Ok(command)
        });

        // Act
        let error = execute_cli_repair_turn(&backend, AgentKind::Codex, &request, "repair")
            .await
            .expect_err("repair command should fail");

        // Assert
        assert_eq!(error, "repair process exited with code 7");
    }

    #[tokio::test]
    async fn test_execute_cli_repair_turn_reports_signal() {
        // Arrange
        let folder = tempdir().expect("failed to create temp dir");
        let request = make_turn_request(folder.path().to_path_buf());
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg("kill -9 $$");

            Ok(command)
        });

        // Act
        let error = execute_cli_repair_turn(&backend, AgentKind::Codex, &request, "repair")
            .await
            .expect_err("repair command should be interrupted");

        // Assert
        assert_eq!(error, "repair process was interrupted by signal 9");
    }

    #[tokio::test]
    async fn test_execute_cli_repair_turn_cleans_up_stdin_writer_after_timeout() {
        // Arrange
        let folder = tempdir().expect("failed to create temp dir");
        let request_kind = AgentRequestKind::SessionStart;
        let repair_prompt = "repair ".repeat(200_000);
        let prompt_payload = TurnPrompt::from_agent_data(repair_prompt.clone());
        let build_request = BuildCommandRequest {
            attachments: &prompt_payload.attachments,
            folder: folder.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "test-model",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: &repair_prompt,
            reasoning_level: ReasoningLevel::default(),
            request_kind: &request_kind,
            speed_mode: crate::model::session::SpeedMode::default(),
        };
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|request| {
            assert!(request.prompt.len() > 1_000_000);
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg("while :; do :; done");

            Ok(command)
        });

        // Act
        let error = execute_cli_repair_command(
            &backend,
            AgentKind::Claude,
            build_request,
            Duration::from_millis(20),
        )
        .await
        .expect_err("repair command should time out");

        // Assert
        assert!(error.starts_with("repair process timed out after"));
    }

    #[test]
    fn test_build_command_request_uses_agent_facing_prompt_text() {
        // Arrange
        let request = TurnRequest {
            continuation: crate::channel::TurnContinuation::fresh(),
            folder: PathBuf::from("/tmp/session"),
            main_checkout_root: Some(PathBuf::from("/tmp/main")),
            model: "claude-sonnet-5".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: TurnPrompt::from("Review @src/main.rs"),
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            response_style: crate::ResponseStyle::default(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };
        let prompt_text = request.prompt.agent_text();

        // Act
        let build_request = build_command_request(&request, &prompt_text);

        // Assert
        assert_eq!(build_request.prompt, "Review \"src/main.rs\"");
        assert_eq!(
            build_request.main_checkout_root,
            Some(std::path::Path::new("/tmp/main"))
        );
    }

    #[tokio::test]
    /// Verifies spawn failure returns `Err` with a descriptive message and
    /// does not emit any turn events when the process never starts.
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
        let error_message = result
            .expect_err("expected Err for spawn failure")
            .to_string();
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
    /// does not emit any loader updates.
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
        let error_message = result
            .expect_err("expected Err for kill-by-signal")
            .to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error was: {error_message}"
        );

        // Drain `PidUpdate` events and verify no loader update was emitted.
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
                .arg("printf '{\"answer\":\"ok\",\"questions\":[]}'");

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
    /// Verifies strict turn parsing recovers one trailing protocol payload
    /// when Claude prepends extra prose before the final JSON object.
    async fn test_run_turn_recovers_wrapped_structured_output_for_claude() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(concat!(
                "printf '%s\\n' 'Now I have the full context.';",
                "printf '%s' '{\"answer\":\"ok\",\"questions\":[]}'",
            ));

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
        assert_eq!(result.assistant_message.to_answer_display_text(), "ok");
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
                 '{\"answer\":\"ok\",\"questions\":[]}'",
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
            text_source: TurnPromptTextSource::UserPrompt,
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
        let error_message = error.to_string();
        assert!(
            error_message.contains("auth failed"),
            "error was: {error_message}"
        );
        assert!(
            !error_message.contains("stdin payload"),
            "stdin write error should not mask child failure: {error_message}"
        );
    }

    #[tokio::test]
    /// Verifies Claude turns surface invalid structured output after both the
    /// original parse and the protocol-repair retry fail.
    async fn test_run_turn_returns_error_for_invalid_structured_output_for_claude() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend
            .expect_build_command()
            .times(2)
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
        let error_message = error.to_string();
        assert!(error_message.contains("did not match the required JSON schema"));
        assert!(!error_message.contains("plain non-json response"));
    }

    #[tokio::test]
    async fn repair_receives_complete_response_and_rejects_oversize_before_execution() {
        // Arrange
        let folder = tempdir().expect("workspace");
        let request = make_turn_request(folder.path().to_owned());
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(request.prompt.contains("preserved tail"));
                assert_eq!(
                    request.permission_mode,
                    crate::model::permission::PermissionMode::ReadOnly
                );
                let mut command = std::process::Command::new("sh");
                command
                    .arg("-c")
                    .arg(r#"printf '{"answer":"repaired","questions":[]}'"#);

                Ok(command)
            });
        let backend: Arc<dyn AgentBackend> = Arc::new(backend);
        let (events, _receiver) = mpsc::unbounded_channel();
        let malformed = format!("{} preserved tail", "x".repeat(2048));

        // Act
        let repaired = parse_or_repair_cli_response(
            AgentKind::Claude,
            &malformed,
            &request,
            &backend,
            &events,
        )
        .await
        .expect("repair");
        let rejected = parse_or_repair_cli_response(
            AgentKind::Claude,
            &"x".repeat(128 * 1024 + 1),
            &request,
            &backend,
            &events,
        )
        .await;

        // Assert
        assert_eq!(repaired.answer, "repaired");
        assert!(
            rejected
                .expect_err("too large")
                .to_string()
                .contains("lossless repair limit")
        );
    }

    #[tokio::test]
    async fn cli_archive_failure_stops_before_spawning_provider() {
        // Arrange
        let folder = tempdir().expect("workspace");
        let backend = Arc::new(MockAgentBackend::new());
        let channel = CliAgentChannel::with_backend(backend, AgentKind::Claude);
        let mut request = make_turn_request(folder.path().join("missing"));
        request.continuation = crate::channel::TurnContinuation::replaying("x".repeat(40 * 1024));
        let (events, _receiver) = mpsc::unbounded_channel();

        // Act
        let result = channel.run_turn("session".into(), request, events).await;

        // Assert
        assert!(result.is_err());
    }

    #[tokio::test]
    /// Verifies Claude turns recover valid output when the initial parse fails
    /// but the protocol-repair retry returns valid protocol JSON.
    async fn test_run_turn_recovers_valid_output_via_protocol_repair_for_claude() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let call_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().times(2).returning({
            let counter = Arc::clone(&call_counter);

            move |_| {
                let call_number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut command = std::process::Command::new("sh");

                if call_number == 0 {
                    command.arg("-c").arg("printf 'plain non-json response'");
                } else {
                    command
                        .arg("-c")
                        .arg(r#"printf '{"answer":"Repaired response","questions":[]}'"#);
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
            .expect("repair retry should succeed");

        // Assert
        assert_eq!(
            result.assistant_message.to_display_text(),
            "Repaired response"
        );
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
        let error_message = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect_err("expired Claude auth should fail")
            .to_string();

        // Assert
        assert!(
            error_message.contains("Agent command failed because Claude authentication expired")
        );
        assert!(error_message.contains("`claude auth login`"));
        assert!(error_message.contains("`claude auth status`"));
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
        let error_message = channel
            .run_turn("sess-1".to_string(), req, events_tx)
            .await
            .expect_err("non-zero exit should fail")
            .to_string();

        // Assert
        assert!(error_message.contains("Agent command failed with exit code 7"));
        assert!(error_message.contains("assist failed"));
    }

    #[tokio::test]
    /// Verifies CLI channels surface only transient loader text while the
    /// final assistant response is returned at turn completion.
    async fn test_run_turn_surfaces_only_loader_updates_for_strict_protocol_provider() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock_backend = MockAgentBackend::new();
        mock_backend.expect_build_command().returning(|_| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(concat!(
                r#"echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]}}';"#,
                r#"echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"streamed fragment"}]}}';"#,
                r#"echo '{"result":"{\"answer\":\"final answer\",\"questions\":[]}","usage":{"input_tokens":5,"output_tokens":3}}'"#,
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
        let mut saw_loader_update = false;
        while let Ok(event) = events_rx.try_recv() {
            if matches!(event, TurnEvent::ThoughtDelta(_)) {
                saw_loader_update = true;
            }
        }
        assert!(saw_loader_update, "loader updates should be streamed live");
        assert_eq!(result.assistant_message.to_display_text(), "final answer");
    }
}
