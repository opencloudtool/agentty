//! One-shot agent prompt execution helpers.
//!
//! These helpers run isolated utility prompts outside the long-lived session
//! turn flow while still enforcing the shared structured response protocol.

use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::sync::Mutex;

use tokio::io::AsyncWriteExt as _;
use tokio::task::JoinHandle;

use super::backend::{AgentBackend, AgentCommandMode, BuildCommandRequest};
use super::protocol::{AgentResponse, parse_agent_response_strict};
use super::{ParsedResponse, create_backend, parse_response};
use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::session::SessionStats;

/// Input payload for one isolated prompt that must still return structured
/// protocol output.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OneShotRequest<'a> {
    /// Optional PID slot used by cancel/stop flows to terminate the spawned
    /// subprocess while a one-shot prompt is running.
    pub(crate) child_pid: Option<&'a Mutex<Option<u32>>>,
    /// Working directory where the prompt command runs.
    pub(crate) folder: &'a Path,
    /// Provider-specific model used for command construction and parsing.
    pub(crate) model: AgentModel,
    /// Prompt text submitted to the agent.
    pub(crate) prompt: &'a str,
    /// Protocol-owned request family used to render shared response
    /// instructions for this utility prompt.
    pub(crate) protocol_profile: super::protocol::ProtocolRequestProfile,
    /// Reasoning effort preference for the one-shot prompt.
    pub(crate) reasoning_level: ReasoningLevel,
}

/// Parsed result returned by one isolated prompt execution.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OneShotSubmission {
    /// Structured protocol response parsed from the final successful attempt.
    pub(crate) response: AgentResponse,
    /// Aggregated token usage for the one-shot prompt execution.
    pub(crate) stats: SessionStats,
}

/// Executes one isolated prompt and returns the parsed structured response.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output does not match the required protocol JSON.
pub(crate) async fn submit_one_shot(request: OneShotRequest<'_>) -> Result<AgentResponse, String> {
    let submission = submit_one_shot_with_stats(request).await?;

    Ok(submission.response)
}

/// Executes one isolated prompt and returns the parsed structured response
/// plus aggregated usage statistics.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output does not match the required protocol JSON.
pub(crate) async fn submit_one_shot_with_stats(
    request: OneShotRequest<'_>,
) -> Result<OneShotSubmission, String> {
    let backend = create_backend(request.model.kind());

    submit_one_shot_with_backend(backend.as_ref(), request).await
}

/// Executes one isolated prompt using the provided backend.
///
/// This shared helper keeps process execution behind the existing
/// `AgentBackend` trait boundary so production callers and tests can reuse
/// the same one-shot protocol validation path.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output does not match the required protocol JSON.
pub(crate) async fn submit_one_shot_with_backend(
    backend: &dyn AgentBackend,
    request: OneShotRequest<'_>,
) -> Result<OneShotSubmission, String> {
    let parsed_response = execute_one_shot_command(backend, request.prompt, request).await?;
    let agent_response =
        parse_agent_response_strict(&parsed_response.content).map_err(|error| {
            format!(
                "One-shot agent output did not match the required JSON schema: \
                 {error}\nresponse:\n{}",
                parsed_response.content
            )
        })?;

    Ok(OneShotSubmission {
        response: agent_response,
        stats: parsed_response.stats,
    })
}

/// Runs one one-shot backend command and returns the parsed provider content.
///
/// The spawned child is configured with `kill_on_drop(true)` so timeout-driven
/// callers do not leave orphaned agent CLI processes behind when the future is
/// canceled before completion.
///
/// # Errors
/// Returns an error when the command cannot be built, run, or exits
/// unsuccessfully.
async fn execute_one_shot_command(
    backend: &dyn AgentBackend,
    prompt: &str,
    request: OneShotRequest<'_>,
) -> Result<ParsedResponse, String> {
    let prompt_payload = crate::infra::channel::TurnPrompt::from_text(prompt.to_string());
    let build_request = BuildCommandRequest {
        attachments: &prompt_payload.attachments,
        folder: request.folder,
        mode: AgentCommandMode::OneShot { prompt },
        model: request.model.as_str(),
        protocol_profile: request.protocol_profile,
        reasoning_level: request.reasoning_level,
    };
    let command = backend
        .build_command(build_request)
        .map_err(|error| format!("Failed to build one-shot agent command: {error}"))?;
    let stdin_payload =
        super::backend::build_command_stdin_payload(request.model.kind(), build_request)
            .map_err(|error| format!("Failed to build one-shot agent stdin payload: {error}"))?;
    let mut tokio_command = tokio::process::Command::from(command);
    tokio_command
        .stdin(if stdin_payload.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .kill_on_drop(true);
    let mut pid_guard = ChildPidGuard::new(request.child_pid);
    let mut child = tokio_command
        .spawn()
        .map_err(|error| format!("Failed to execute one-shot agent command: {error}"))?;
    pid_guard.update_from_child(&child);
    let stdin_write_task = spawn_optional_stdin_write(child.stdin.take(), stdin_payload);
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("Failed to execute one-shot agent command: {error}"))?;
    await_optional_stdin_write(stdin_write_task).await?;

    if output.status.signal().is_some() {
        return Err("One-shot agent command was interrupted".to_string());
    }

    let stdout_text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr_text = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(format_one_shot_exit_error(
            request.model.kind(),
            output.status.code(),
            &stdout_text,
            &stderr_text,
        ));
    }

    let parsed_response = parse_response(request.model.kind(), &stdout_text, &stderr_text);

    Ok(parsed_response)
}

/// Starts one background stdin writer when the child needs prompt input.
fn spawn_optional_stdin_write(
    child_stdin: Option<tokio::process::ChildStdin>,
    stdin_payload: Option<Vec<u8>>,
) -> Option<JoinHandle<Result<(), String>>> {
    stdin_payload.map(|stdin_payload| {
        tokio::spawn(async move { write_optional_stdin(child_stdin, stdin_payload).await })
    })
}

/// Waits for one optional background stdin writer to finish.
///
/// # Errors
/// Returns an error when the writer task fails or panics before the full
/// payload is sent.
async fn await_optional_stdin_write(
    stdin_write_task: Option<JoinHandle<Result<(), String>>>,
) -> Result<(), String> {
    let Some(stdin_write_task) = stdin_write_task else {
        return Ok(());
    };

    stdin_write_task
        .await
        .map_err(|error| format!("One-shot stdin write task failed: {error}"))?
}

/// Writes one optional stdin payload into the spawned one-shot subprocess.
///
/// # Errors
/// Returns an error when stdin was requested but not available or the write
/// fails before EOF is signaled.
async fn write_optional_stdin(
    child_stdin: Option<tokio::process::ChildStdin>,
    stdin_payload: Vec<u8>,
) -> Result<(), String> {
    let mut child_stdin =
        child_stdin.ok_or_else(|| "one-shot stdin pipe unavailable after spawn".to_string())?;
    if let Err(error) = child_stdin.write_all(&stdin_payload).await
        && !is_broken_pipe_error(&error)
    {
        return Err(format!("Failed to write one-shot stdin payload: {error}"));
    }
    if let Err(error) = child_stdin.shutdown().await
        && !is_broken_pipe_error(&error)
    {
        return Err(format!("Failed to close one-shot stdin payload: {error}"));
    }

    Ok(())
}

/// Returns whether one stdin write error is the expected closed-pipe case.
fn is_broken_pipe_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::BrokenPipe
}

/// Formats one non-zero one-shot command exit into a user-facing error.
fn format_one_shot_exit_error(
    agent_kind: AgentKind,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    if let Some(guidance) = known_one_shot_exit_guidance(agent_kind, stdout, stderr) {
        return guidance;
    }

    let exit_code = exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    let output_detail = one_shot_output_detail(stdout, stderr);

    format!("One-shot agent command failed with exit code {exit_code}: {output_detail}")
}

/// Returns provider-specific guidance for known one-shot command failures.
fn known_one_shot_exit_guidance(
    agent_kind: AgentKind,
    stdout: &str,
    stderr: &str,
) -> Option<String> {
    match agent_kind {
        AgentKind::Claude if is_claude_authentication_error(stdout, stderr) => {
            Some(claude_authentication_error_message())
        }
        AgentKind::Claude | AgentKind::Codex | AgentKind::Gemini => None,
    }
}

/// Builds the actionable Claude authentication refresh guidance message.
fn claude_authentication_error_message() -> String {
    "One-shot Claude command failed because authentication expired or is missing.\nRun `claude \
     auth login` to refresh your Anthropic session, verify with `claude auth status`, then retry."
        .to_string()
}

/// Detects Claude CLI authentication failures surfaced through stdout/stderr.
fn is_claude_authentication_error(stdout: &str, stderr: &str) -> bool {
    let combined_output = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    combined_output.contains("oauth token has expired")
        || combined_output.contains("failed to authenticate")
        || combined_output.contains("authentication_error")
}

/// Formats captured stdout/stderr into one compact error detail string.
fn one_shot_output_detail(stdout: &str, stderr: &str) -> String {
    let trimmed_stdout = stdout.trim();
    let trimmed_stderr = stderr.trim();

    match (trimmed_stdout.is_empty(), trimmed_stderr.is_empty()) {
        (false, false) => format!("stdout: {trimmed_stdout}; stderr: {trimmed_stderr}"),
        (false, true) => format!("stdout: {trimmed_stdout}"),
        (true, false) => format!("stderr: {trimmed_stderr}"),
        (true, true) => "no output".to_string(),
    }
}

/// Tracks the active one-shot subprocess identifier for cancel/stop flows.
struct ChildPidGuard<'a> {
    child_pid: Option<&'a Mutex<Option<u32>>>,
}

impl<'a> ChildPidGuard<'a> {
    /// Creates one PID guard for the optional shared child slot.
    fn new(child_pid: Option<&'a Mutex<Option<u32>>>) -> Self {
        Self { child_pid }
    }

    /// Copies the spawned child PID into the shared slot when available.
    fn update_from_child(&mut self, child: &tokio::process::Child) {
        let Some(pid) = child.id() else {
            return;
        };

        let Some(child_pid) = self.child_pid else {
            return;
        };

        if let Ok(mut guard) = child_pid.lock() {
            *guard = Some(pid);
        }
    }
}

impl Drop for ChildPidGuard<'_> {
    fn drop(&mut self) {
        let Some(child_pid) = self.child_pid else {
            return;
        };

        if let Ok(mut guard) = child_pid.lock() {
            *guard = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;
    use crate::infra::agent::tests::MockAgentBackend;

    /// Builds one shell command that emits controlled stdout/stderr and exits.
    fn mock_shell_command(stdout: &str, stderr: &str, exit_code: i32) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "printf '%s' \"$ONE_SHOT_STDOUT\"; printf '%s' \"$ONE_SHOT_STDERR\" >&2; exit \
             \"$ONE_SHOT_EXIT\"",
        );
        command.env("ONE_SHOT_STDOUT", stdout);
        command.env("ONE_SHOT_STDERR", stderr);
        command.env("ONE_SHOT_EXIT", exit_code.to_string());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        command
    }

    /// Builds one shell command that captures stdin before returning JSON.
    fn stdin_capture_shell_command(capture_path: &Path) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "cat > \"$ONE_SHOT_CAPTURE_PATH\"; printf '%s' \
             '{\"answer\":\"captured\",\"questions\":[],\"summary\":null}'",
        );
        command.env("ONE_SHOT_CAPTURE_PATH", capture_path);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        command
    }

    #[tokio::test]
    /// Verifies one-shot execution returns the parsed structured answer.
    async fn test_submit_one_shot_with_backend_returns_protocol_response() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|request| {
            assert!(matches!(
                request.mode,
                AgentCommandMode::OneShot {
                    prompt: "Generate title",
                }
            ));
            assert_eq!(
                request.protocol_profile,
                crate::infra::agent::ProtocolRequestProfile::UtilityPrompt
            );

            Ok(mock_shell_command(
                r#"{"answer":"Generated title","questions":[],"summary":null}"#,
                "",
                0,
            ))
        });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::ClaudeSonnet46,
                prompt: "Generate title",
                protocol_profile: crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect("one-shot prompt should succeed");

        // Assert
        assert_eq!(
            response.response.answers(),
            vec!["Generated title".to_string()]
        );
    }

    #[tokio::test]
    /// Verifies one-shot execution surfaces a schema error immediately when
    /// the response is not valid protocol JSON.
    async fn test_submit_one_shot_with_backend_returns_error_for_invalid_protocol_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.mode,
                    AgentCommandMode::OneShot {
                        prompt: "Generate title",
                    }
                ));

                Ok(mock_shell_command("plain text", "", 0))
            });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::Gpt54,
                prompt: "Generate title",
                protocol_profile: crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("invalid protocol output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\nplain text"));
    }

    #[tokio::test]
    /// Verifies wrapped provider output that still fails protocol parsing is
    /// surfaced as an error without extra retries.
    async fn test_submit_one_shot_with_backend_returns_error_for_wrapped_invalid_protocol_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.mode,
                    AgentCommandMode::OneShot {
                        prompt: "Generate title",
                    }
                ));

                Ok(mock_shell_command(
                    r#"{"result":"plain text","usage":{"input_tokens":2,"output_tokens":1}}"#,
                    "",
                    0,
                ))
            });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::ClaudeSonnet46,
                prompt: "Generate title",
                protocol_profile: crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("wrapped invalid protocol output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\nplain text"));
    }

    #[tokio::test]
    /// Verifies one-shot execution does not deadlock when the child delays
    /// reading stdin until after it emits early stderr output.
    async fn test_submit_one_shot_with_backend_writes_large_stdin_concurrently() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let large_prompt = "x".repeat(512 * 1024);
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            let mut command = Command::new("sh");
            command.arg("-c").arg(
                "printf 'warming up\\n' >&2; sleep 0.1; cat >/dev/null; printf '%s' \
                 '{\"answer\":\"done\",\"questions\":[],\"summary\":null}'",
            );
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::piped());

            Ok(command)
        });

        // Act
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            submit_one_shot_with_backend(
                &backend,
                OneShotRequest {
                    child_pid: None,
                    folder: temp_directory.path(),
                    model: AgentModel::ClaudeSonnet46,
                    prompt: &large_prompt,
                    protocol_profile: crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
                    reasoning_level: ReasoningLevel::default(),
                },
            ),
        )
        .await
        .expect("one-shot prompt should not deadlock")
        .expect("one-shot prompt should succeed");

        // Assert
        assert_eq!(response.response.answers(), vec!["done".to_string()]);
    }

    #[tokio::test]
    /// Verifies one-shot execution streams Claude prompts through stdin so
    /// large review requests avoid argv length limits.
    async fn test_submit_one_shot_with_backend_writes_prompt_to_stdin() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let capture_path = temp_directory.path().join("stdin.txt");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning({
            let capture_path = capture_path.clone();

            move |_| Ok(stdin_capture_shell_command(&capture_path))
        });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::ClaudeSonnet46,
                prompt: "Generate title",
                protocol_profile: crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect("one-shot prompt should succeed");
        let captured_prompt =
            std::fs::read_to_string(&capture_path).expect("captured stdin payload should exist");

        // Assert
        assert_eq!(response.response.answers(), vec!["captured".to_string()]);
        assert!(captured_prompt.contains("Structured response protocol:"));
        assert!(captured_prompt.contains("Generate title"));
    }

    #[tokio::test]
    /// Verifies a broken stdin pipe does not hide the child exit status or
    /// stderr when the backend exits before reading the full prompt.
    async fn test_submit_one_shot_with_backend_preserves_exit_error_after_broken_pipe() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let large_prompt = "x".repeat(512 * 1024);
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            let mut command = Command::new("sh");
            command.arg("-c").arg("printf 'auth failed' >&2; exit 7");
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::piped());

            Ok(command)
        });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::ClaudeSonnet46,
                prompt: &large_prompt,
                protocol_profile: crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("one-shot prompt should surface the child exit");

        // Assert
        assert!(error.contains("exit code 7"), "error was: {error}");
        assert!(error.contains("auth failed"), "error was: {error}");
        assert!(
            !error.contains("stdin payload"),
            "stdin write error should not mask child failure: {error}"
        );
    }

    #[tokio::test]
    /// Verifies Claude authentication failures return actionable re-login
    /// guidance instead of raw transport output.
    async fn test_submit_one_shot_with_backend_surfaces_claude_auth_guidance() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            Ok(mock_shell_command(
                r#"{"type":"error","error":{"type":"authentication_error","message":"OAuth token has expired. Please obtain a new token or refresh your existing token."}}"#,
                "",
                1,
            ))
        });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::ClaudeSonnet46,
                prompt: "Generate title",
                protocol_profile: crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("expired Claude auth should fail");

        // Assert
        assert!(error.contains("One-shot Claude command failed because authentication expired"));
        assert!(error.contains("`claude auth login`"));
        assert!(error.contains("`claude auth status`"));
    }
}
