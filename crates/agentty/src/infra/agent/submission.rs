//! One-shot agent prompt execution helpers.
//!
//! These helpers run isolated utility prompts outside the long-lived session
//! turn flow. They require the shared structured response protocol on every
//! transport so one-shot callers enforce the same schema contract as normal
//! session turns.

use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::backend::{AgentBackend, BuildCommandRequest};
use super::cli::{error, stdin};
use super::protocol::{AgentResponse, parse_agent_response_strict};
use super::{
    ParsedResponse, create_app_server_client, create_backend, parse_response, transport_mode,
};
use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::session::SessionStats;
use crate::infra::app_server::{AppServerClient, AppServerTurnRequest};
use crate::infra::channel::AgentRequestKind;

/// Input payload for one isolated prompt that prefers structured protocol
/// output.
#[derive(Clone, Debug)]
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
    /// Canonical request kind for this isolated prompt.
    pub(crate) request_kind: AgentRequestKind,
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

/// Executes one isolated prompt and returns the parsed response.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output is empty or otherwise unusable.
pub(crate) async fn submit_one_shot(request: OneShotRequest<'_>) -> Result<AgentResponse, String> {
    let submission = submit_one_shot_with_stats(request).await?;

    Ok(submission.response)
}

/// Executes one isolated prompt and returns the parsed response plus
/// aggregated usage statistics.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output is empty or otherwise unusable.
pub(crate) async fn submit_one_shot_with_stats(
    request: OneShotRequest<'_>,
) -> Result<OneShotSubmission, String> {
    submit_one_shot_with_stats_and_app_server_client(request, None).await
}

/// Executes one isolated prompt and returns the parsed response plus
/// aggregated usage statistics, optionally overriding the backend-owned
/// app-server client.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output is empty or otherwise unusable.
pub(crate) async fn submit_one_shot_with_stats_and_app_server_client(
    request: OneShotRequest<'_>,
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
) -> Result<OneShotSubmission, String> {
    let backend = create_backend(request.model.kind());

    if transport_mode(request.model.kind()).uses_app_server() {
        let app_server_client =
            create_app_server_client(request.model.kind(), app_server_client_override).ok_or_else(
                || {
                    format!(
                        "{} provider did not provide an app-server client",
                        request.model.kind()
                    )
                },
            )?;

        return submit_one_shot_with_app_server_client(app_server_client.as_ref(), request).await;
    }

    submit_one_shot_with_backend(backend.as_ref(), request).await
}

/// Executes one isolated prompt through the shared app-server transport.
///
/// The temporary app-server session is shut down after the utility prompt
/// finishes so one-shot helpers do not keep a provider runtime alive after the
/// result has been parsed.
///
/// # Errors
/// Returns an error when app-server turn execution fails or the final output
/// is empty or otherwise unusable.
pub(crate) async fn submit_one_shot_with_app_server_client(
    app_server_client: &dyn AppServerClient,
    request: OneShotRequest<'_>,
) -> Result<OneShotSubmission, String> {
    clear_child_pid_slot(request.child_pid);

    let session_id = format!("one-shot-{}", uuid::Uuid::new_v4());
    let (stream_tx, _stream_rx) = tokio::sync::mpsc::unbounded_channel();
    let turn_request = AppServerTurnRequest {
        folder: request.folder.to_path_buf(),
        live_session_output: None,
        model: request.model.as_str().to_string(),
        prompt: crate::infra::channel::TurnPrompt::from_text(request.prompt.to_string()),
        request_kind: request.request_kind,
        provider_conversation_id: None,
        reasoning_level: request.reasoning_level,
        session_id: session_id.clone(),
    };

    let turn_result = app_server_client.run_turn(turn_request, stream_tx).await;
    app_server_client.shutdown_session(session_id).await;
    clear_child_pid_slot(request.child_pid);

    let turn_result = turn_result
        .map_err(|error| format!("Failed to execute one-shot app-server turn: {error}"))?;
    let response = parse_one_shot_response(&turn_result.assistant_message)?;

    Ok(OneShotSubmission {
        response,
        stats: SessionStats {
            input_tokens: turn_result.input_tokens,
            output_tokens: turn_result.output_tokens,
        },
    })
}

/// Executes one isolated prompt using the provided backend.
///
/// This shared helper keeps process execution behind the existing
/// `AgentBackend` trait boundary so production callers and tests can reuse
/// the same one-shot parsing path.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output is empty or otherwise unusable.
pub(crate) async fn submit_one_shot_with_backend(
    backend: &dyn AgentBackend,
    request: OneShotRequest<'_>,
) -> Result<OneShotSubmission, String> {
    let parsed_response = execute_one_shot_command(backend, request.prompt, request).await?;
    let agent_response = parse_one_shot_response(&parsed_response.content)?;

    Ok(OneShotSubmission {
        response: agent_response,
        stats: parsed_response.stats,
    })
}

/// Parses one one-shot response strictly against the shared protocol schema.
///
/// # Errors
/// Returns an error when the response is empty or not valid protocol JSON.
fn parse_one_shot_response(content: &str) -> Result<AgentResponse, String> {
    parse_agent_response_strict(content).map_err(|error| {
        format!(
            "One-shot agent output did not match the required JSON schema: \
             {error}\nresponse:\n{content}"
        )
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
        prompt,
        request_kind: &request.request_kind,
        model: request.model.as_str(),
        reasoning_level: request.reasoning_level,
    };
    let command = backend
        .build_command(build_request)
        .map_err(|error| format!("Failed to build one-shot agent command: {error}"))?;
    let stdin_payload = super::build_command_stdin_payload(request.model.kind(), build_request)
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
    let stdin_write_task = stdin::spawn_optional_stdin_write(
        child.stdin.take(),
        stdin_payload,
        "one-shot stdin pipe unavailable after spawn",
        std::convert::identity,
    );
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("Failed to execute one-shot agent command: {error}"))?;
    stdin::await_optional_stdin_write(
        stdin_write_task,
        "One-shot stdin write task failed",
        std::convert::identity,
    )
    .await?;

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

/// Formats one non-zero one-shot command exit into a user-facing error.
fn format_one_shot_exit_error(
    agent_kind: AgentKind,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    error::format_agent_cli_exit_error(
        agent_kind,
        "One-shot agent command",
        exit_code,
        stdout,
        stderr,
    )
}

/// Clears the shared one-shot child PID slot when one exists.
fn clear_child_pid_slot(child_pid: Option<&Mutex<Option<u32>>>) {
    let Some(child_pid) = child_pid else {
        return;
    };

    if let Ok(mut guard) = child_pid.lock() {
        *guard = None;
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
    use crate::infra::app_server::{AppServerTurnResponse, MockAppServerClient};

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
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));
            assert_eq!(request.prompt, "Generate title");

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
                request_kind: AgentRequestKind::UtilityPrompt,
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
    /// Verifies one-shot execution rejects plain-text utility output that
    /// does not match the protocol schema.
    async fn test_submit_one_shot_with_backend_rejects_plain_text_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));
                assert_eq!(request.prompt, "Generate title");

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
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("plain-text utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\nplain text"));
    }

    #[tokio::test]
    /// Verifies one-shot execution rejects wrapped non-schema utility output
    /// instead of extracting plain text from provider wrappers.
    async fn test_submit_one_shot_with_backend_rejects_wrapped_plain_text_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));
                assert_eq!(request.prompt, "Generate title");

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
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("wrapped plain-text utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\n"));
    }

    #[tokio::test]
    /// Verifies one-shot execution recovers a trailing protocol payload when
    /// the provider prepends extra prose before the final JSON object.
    async fn test_submit_one_shot_with_backend_recovers_wrapped_protocol_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));
                assert_eq!(request.prompt, "Generate title");

                Ok(mock_shell_command(
                    concat!(
                        "Now I have full context.\n",
                        r#"{"answer":"Generated title","questions":[],"summary":null}"#
                    ),
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
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect("wrapped protocol output should succeed");

        // Assert
        assert_eq!(
            response.response.answers(),
            vec!["Generated title".to_string()]
        );
    }

    #[tokio::test]
    /// Verifies one-shot execution still rejects blank utility responses.
    async fn test_submit_one_shot_with_backend_rejects_blank_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|request| {
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));
            assert_eq!(request.prompt, "Generate title");

            Ok(mock_shell_command("   ", "", 0))
        });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::Gpt54,
                prompt: "Generate title",
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("blank utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\n"));
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
                    request_kind: AgentRequestKind::UtilityPrompt,
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
                request_kind: AgentRequestKind::UtilityPrompt,
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
                request_kind: AgentRequestKind::UtilityPrompt,
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
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("expired Claude auth should fail");

        // Assert
        assert!(
            error.contains("One-shot agent command failed because Claude authentication expired")
        );
        assert!(error.contains("`claude auth login`"));
        assert!(error.contains("`claude auth status`"));
    }

    #[tokio::test]
    /// Verifies app-server-backed one-shot execution returns the parsed
    /// structured answer and usage totals.
    async fn test_submit_one_shot_with_app_server_client_returns_protocol_response() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut app_server_client = MockAppServerClient::new();
        app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|request, _| {
                assert_eq!(request.model, AgentModel::Gpt54.as_str());
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));
                assert_eq!(request.prompt.text, "Generate title");

                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message:
                            r#"{"answer":"Generated title","questions":[],"summary":null}"#
                                .to_string(),
                        context_reset: false,
                        input_tokens: 11,
                        output_tokens: 7,
                        pid: Some(42),
                        provider_conversation_id: Some("thread-1".to_string()),
                    })
                })
            });
        app_server_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));

        // Act
        let response = submit_one_shot_with_app_server_client(
            &app_server_client,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::Gpt54,
                prompt: "Generate title",
                request_kind: AgentRequestKind::UtilityPrompt,
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
        assert_eq!(response.stats.input_tokens, 11);
        assert_eq!(response.stats.output_tokens, 7);
    }

    #[tokio::test]
    /// Verifies app-server-backed one-shot execution rejects plain-text
    /// utility output that does not match the protocol schema.
    async fn test_submit_one_shot_with_app_server_client_rejects_plain_text_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut app_server_client = MockAppServerClient::new();
        app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|request, _| {
                assert_eq!(request.model, AgentModel::Gpt54.as_str());

                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: "plain text".to_string(),
                        context_reset: false,
                        input_tokens: 2,
                        output_tokens: 1,
                        pid: None,
                        provider_conversation_id: None,
                    })
                })
            });
        app_server_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));

        // Act
        let error = submit_one_shot_with_app_server_client(
            &app_server_client,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::Gpt54,
                prompt: "Generate title",
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("plain-text utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\nplain text"));
    }

    #[tokio::test]
    /// Verifies app-server-backed non-utility one-shot execution still
    /// rejects plain-text output that does not match the protocol schema.
    async fn test_submit_one_shot_with_app_server_client_rejects_plain_text_non_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut app_server_client = MockAppServerClient::new();
        app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|request, _| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::SessionStart
                ));

                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: "plain text".to_string(),
                        context_reset: false,
                        input_tokens: 2,
                        output_tokens: 1,
                        pid: None,
                        provider_conversation_id: None,
                    })
                })
            });
        app_server_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));

        // Act
        let error = submit_one_shot_with_app_server_client(
            &app_server_client,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::Gpt54,
                prompt: "Generate title",
                request_kind: AgentRequestKind::SessionStart,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect_err("invalid non-utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\nplain text"));
    }
}
