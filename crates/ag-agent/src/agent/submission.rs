//! One-shot agent prompt execution helpers.
//!
//! These helpers run isolated utility prompts outside the long-lived session
//! turn flow. They require the shared structured response protocol on every
//! transport so one-shot callers enforce the same schema contract as normal
//! session turns.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_protocol::{
    AgentResponse, ProtocolRequestProfile, build_protocol_repair_prompt,
    format_protocol_parse_debug_details, parse_protocol_response_strict,
};
use async_trait::async_trait;

use super::backend::{AgentBackend, BuildCommandRequest};
use super::cli::error;
use super::cli::execution::{self, CliExecutionError, CliExecutionObserver, CliExitStatus};
use super::{
    ParsedResponse, create_app_server_client, create_backend, parse_response, transport_mode,
};
use crate::app_server::{AppServerClient, AppServerTurnRequest};
use crate::channel::AgentRequestKind;
use crate::model::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::model::permission::PermissionMode;
use crate::model::session::{SessionDiffState, SessionStats, SpeedMode};

/// Input payload for one isolated prompt that prefers structured protocol
/// output.
#[derive(Clone, Debug)]
pub struct OneShotRequest {
    /// Provider backend used for command construction, stdin shaping, and
    /// response parsing.
    pub agent_kind: AgentKind,
    /// Optional PID slot used by cancel/stop flows to terminate the spawned
    /// subprocess while a one-shot prompt is running.
    pub child_pid: Option<Arc<Mutex<Option<u32>>>>,
    /// Working directory where the prompt command runs.
    pub folder: PathBuf,
    /// Provider-specific model used for command construction and parsing.
    pub model: AgentModel,
    /// Filesystem and command permission policy for this isolated prompt.
    pub permission_mode: PermissionMode,
    /// Prompt text submitted to the agent.
    pub prompt: String,
    /// Canonical request kind for this isolated prompt.
    pub request_kind: AgentRequestKind,
    /// Reasoning effort preference for the one-shot prompt.
    pub reasoning_level: ReasoningLevel,
    /// Response-speed preference for the one-shot prompt.
    pub speed_mode: SpeedMode,
}

/// Parsed result returned by one isolated prompt execution.
#[derive(Clone, Debug, PartialEq)]
pub struct OneShotSubmission {
    /// Structured protocol response parsed from the final successful attempt.
    pub response: AgentResponse,
    /// Aggregated token usage for the one-shot prompt execution.
    pub stats: SessionStats,
}

/// Typed failure returned by [`OneShotClient`] submissions.
///
/// The concrete transport, protocol-repair, and provider diagnostics remain
/// available through [`std::fmt::Display`] without exposing transport-specific
/// variants to callers.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct OneShotError {
    message: String,
}

impl OneShotError {
    /// Creates an error from one already formatted submission diagnostic.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Provider-neutral boundary for isolated structured agent prompts.
///
/// Implementations own transport selection, protocol repair, temporary
/// app-server lifecycle, and usage aggregation so callers submit one request
/// without selecting a CLI or app-server execution helper.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait OneShotClient: Send + Sync {
    /// Executes one isolated prompt and returns its parsed response and usage.
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError>;
}

/// Production [`OneShotClient`] that routes through the selected provider.
pub struct RealOneShotClient {
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
}

impl RealOneShotClient {
    /// Creates a client with an optional app-server override.
    ///
    /// Production passes `None` so each provider supplies its native client;
    /// deterministic environments may inject a shared app-server boundary.
    pub fn new(app_server_client_override: Option<Arc<dyn AppServerClient>>) -> Self {
        Self {
            app_server_client_override,
        }
    }
}

#[async_trait]
impl OneShotClient for RealOneShotClient {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let app_server_client_override = self.app_server_client_override.as_ref().map(Arc::clone);

        submit_one_shot_with_stats_and_app_server_client(request, app_server_client_override)
            .await
            .map_err(OneShotError::new)
    }
}

/// Executes one isolated prompt and returns the parsed response plus
/// aggregated usage statistics, optionally overriding the backend-owned
/// app-server client.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output is empty or otherwise unusable.
async fn submit_one_shot_with_stats_and_app_server_client(
    request: OneShotRequest,
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
) -> Result<OneShotSubmission, String> {
    let backend = create_backend(request.agent_kind);

    if transport_mode(request.agent_kind).uses_app_server() {
        let app_server_client =
            create_app_server_client(request.agent_kind, app_server_client_override).ok_or_else(
                || {
                    format!(
                        "{} provider did not provide an app-server client",
                        request.agent_kind
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
async fn submit_one_shot_with_app_server_client(
    app_server_client: &dyn AppServerClient,
    request: OneShotRequest,
) -> Result<OneShotSubmission, String> {
    clear_child_pid_slot(request.child_pid.as_deref());

    let session_id = format!("one-shot-{}", uuid::Uuid::new_v4());
    let protocol_profile = request.request_kind.protocol_profile();
    let (stream_tx, _stream_rx) = tokio::sync::mpsc::unbounded_channel();
    let turn_request = AppServerTurnRequest {
        folder: request.folder.clone(),
        live_transcript: None,
        main_checkout_root: None,
        model: request.model.provider_model_str().to_string(),
        permission_mode: request.permission_mode,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: ag_protocol::TurnPrompt::from_agent_data(request.prompt.clone()),
        request_kind: request.request_kind.clone(),
        replay_transcript: None,
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: request.reasoning_level,
        session_id: session_id.clone(),
        speed_mode: request.speed_mode,
    };

    let turn_result = app_server_client.run_turn(turn_request, stream_tx).await;

    let child_pid = request.child_pid.as_ref().map(Arc::clone);

    let turn_result = match turn_result {
        Ok(result) => result,
        Err(error) => {
            app_server_client.shutdown_session(session_id).await;
            clear_child_pid_slot(child_pid.as_deref());

            return Err(format!(
                "Failed to execute one-shot app-server turn: {error}"
            ));
        }
    };

    let parse_result =
        match parse_one_shot_response(&turn_result.assistant_message, protocol_profile) {
            Ok(response) => Ok((response, 0, 0)),
            Err(parse_error) => {
                attempt_one_shot_app_server_repair(
                    app_server_client,
                    &parse_error,
                    &turn_result.assistant_message,
                    request,
                    &session_id,
                    turn_result.provider_conversation_id.as_deref(),
                )
                .await
            }
        };

    app_server_client.shutdown_session(session_id).await;
    clear_child_pid_slot(child_pid.as_deref());

    let (response, repair_input_tokens, repair_output_tokens) = parse_result?;

    Ok(OneShotSubmission {
        response,
        stats: SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: turn_result.input_tokens + repair_input_tokens,
            output_tokens: turn_result.output_tokens + repair_output_tokens,
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
async fn submit_one_shot_with_backend(
    backend: &dyn AgentBackend,
    request: OneShotRequest,
) -> Result<OneShotSubmission, String> {
    let protocol_profile = request.request_kind.protocol_profile();
    let parsed_response =
        execute_one_shot_command(backend, &request.prompt, request.clone()).await?;
    let (agent_response, repair_stats) =
        match parse_one_shot_response(&parsed_response.content, protocol_profile) {
            Ok(response) => (response, None),
            Err(parse_error) => {
                let repair_prompt =
                    build_protocol_repair_prompt(&parse_error, &parsed_response.content)?;
                let request = OneShotRequest {
                    permission_mode: PermissionMode::ReadOnly,
                    ..request
                };
                let repair_response = execute_one_shot_command(backend, &repair_prompt, request)
                    .await
                    .map_err(|error| format!("{parse_error}\nrepair transport failed: {error}"))?;

                let response = parse_one_shot_response(&repair_response.content, protocol_profile)
                    .map_err(|error| {
                        format!(
                            "{parse_error}\nrepair retry also failed: \
                             {error}\nrepair_response:\n{}",
                            repair_response.content
                        )
                    })?;

                (response, Some(repair_response.stats))
            }
        };

    let mut stats = parsed_response.stats;
    if let Some(repair) = repair_stats {
        stats.input_tokens += repair.input_tokens;
        stats.output_tokens += repair.output_tokens;
    }

    Ok(OneShotSubmission {
        response: agent_response,
        stats,
    })
}

/// Parses one one-shot response strictly against the shared protocol schema.
///
/// # Errors
/// Returns an error when the response is empty or not valid protocol JSON. The
/// error carries the parse reason and derived diagnostics only, never the
/// provider payload itself.
fn parse_one_shot_response(
    content: &str,
    protocol_profile: ProtocolRequestProfile,
) -> Result<AgentResponse, String> {
    parse_protocol_response_strict(content, protocol_profile).map_err(|error| {
        format!(
            "One-shot agent output did not match the required JSON schema: \
             {error}\ndebug_details:\n{}",
            format_protocol_parse_debug_details(content)
        )
    })
}

/// Attempts one protocol-repair retry through the app-server transport for
/// a one-shot prompt whose initial response failed schema validation.
///
/// The repair prompt is sent as a follow-up turn on the same session so the
/// agent retains the original conversation context. The initial turn's
/// `provider_conversation_id` is threaded through so providers that depend
/// on conversation state can continue the same thread.
///
/// Returns the parsed response together with the repair turn's token usage
/// so the caller can aggregate stats across both attempts.
///
/// # Errors
/// Returns the combined original and repair error when the retry fails.
async fn attempt_one_shot_app_server_repair(
    app_server_client: &dyn AppServerClient,
    parse_error: &str,
    malformed_response: &str,
    request: OneShotRequest,
    session_id: &str,
    provider_conversation_id: Option<&str>,
) -> Result<(AgentResponse, u64, u64), String> {
    let protocol_profile = request.request_kind.protocol_profile();
    let repair_prompt = build_protocol_repair_prompt(parse_error, malformed_response)?;

    let (repair_stream_tx, _repair_stream_rx) = tokio::sync::mpsc::unbounded_channel();
    let repair_turn_request = AppServerTurnRequest {
        folder: request.folder,
        live_transcript: None,
        main_checkout_root: None,
        model: request.model.provider_model_str().to_string(),
        permission_mode: request.permission_mode,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: ag_protocol::TurnPrompt::from_agent_data(repair_prompt),
        request_kind: request.request_kind,
        replay_transcript: None,
        provider_conversation_id: provider_conversation_id.map(String::from),
        persisted_instruction_conversation_id: None,
        reasoning_level: request.reasoning_level,
        session_id: session_id.to_string(),
        speed_mode: request.speed_mode,
    };
    let repair_result = app_server_client
        .run_turn(repair_turn_request, repair_stream_tx)
        .await
        .map_err(|error| format!("{parse_error}\nrepair transport failed: {error}"))?;

    let response = parse_one_shot_response(&repair_result.assistant_message, protocol_profile)
        .map_err(|error| {
            format!(
                "{parse_error}\nrepair retry also failed: {error}\nrepair_response:\n{}",
                repair_result.assistant_message
            )
        })?;

    Ok((
        response,
        repair_result.input_tokens,
        repair_result.output_tokens,
    ))
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
    request: OneShotRequest,
) -> Result<ParsedResponse, String> {
    let prompt_payload = ag_protocol::TurnPrompt::from_agent_data(prompt.to_string());
    let build_request = BuildCommandRequest {
        attachments: &prompt_payload.attachments,
        folder: &request.folder,
        main_checkout_root: None,
        replay_transcript: None,
        model: request.model.provider_model_str(),
        permission_mode: request.permission_mode,
        personality_prompt: None,
        prompt,
        reasoning_level: request.reasoning_level,
        request_kind: &request.request_kind,
        speed_mode: request.speed_mode,
    };
    let observer = OneShotCliObserver {
        child_pid: request.child_pid,
    };
    let output =
        execution::execute_cli_command(backend, request.agent_kind, build_request, &observer, None)
            .await
            .map_err(format_one_shot_execution_error)?;

    match output.exit_status {
        CliExitStatus::Signaled(_) => {
            return Err("One-shot agent command was interrupted".to_string());
        }
        CliExitStatus::NonZero(exit_code) => {
            return Err(format_one_shot_exit_error(
                request.agent_kind,
                exit_code,
                &output.stdout,
                &output.stderr,
            ));
        }
        CliExitStatus::Success => {}
    }

    let parsed_response = parse_response(request.agent_kind, &output.stdout, &output.stderr);

    Ok(parsed_response)
}

/// Preserves the established one-shot context around shared execution errors.
fn format_one_shot_execution_error(error: CliExecutionError) -> String {
    match error {
        CliExecutionError::CommandBuild(error) => {
            format!("Failed to build one-shot agent command: {error}")
        }
        CliExecutionError::StdinBuild(error) => {
            format!("Failed to build one-shot agent stdin payload: {error}")
        }
        error => format!("Failed to execute one-shot agent command: {error}"),
    }
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

/// Bridges shared CLI PID observations into the one-shot cancellation slot.
struct OneShotCliObserver {
    child_pid: Option<Arc<Mutex<Option<u32>>>>,
}

impl CliExecutionObserver for OneShotCliObserver {
    fn pid_updated(&self, active_child_pid: Option<u32>) {
        let Some(child_pid_slot) = self.child_pid.as_deref() else {
            return;
        };

        if let Ok(mut guard) = child_pid_slot.lock() {
            *guard = active_child_pid;
        }
    }

    fn stdout_line(&self, _line: &str) {}
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;
    use crate::MockAgentBackend;
    use crate::app_server::{AppServerError, AppServerTurnResponse, MockAppServerClient};

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
             '{\"answer\":\"captured\",\"questions\":[]}'",
        );
        command.env("ONE_SHOT_CAPTURE_PATH", capture_path);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        command
    }

    #[test]
    fn test_format_one_shot_execution_error_preserves_build_context() {
        // Arrange
        let command_error = CliExecutionError::CommandBuild(
            crate::agent::AgentBackendError::CommandBuild("command".to_string()),
        );
        let stdin_error = CliExecutionError::StdinBuild(
            crate::agent::AgentBackendError::CommandBuild("stdin".to_string()),
        );
        let execution_error = CliExecutionError::StdinWrite("write".to_string());

        // Act
        let command_message = format_one_shot_execution_error(command_error);
        let stdin_message = format_one_shot_execution_error(stdin_error);
        let execution_message = format_one_shot_execution_error(execution_error);

        // Assert
        assert_eq!(
            command_message,
            "Failed to build one-shot agent command: command"
        );
        assert_eq!(
            stdin_message,
            "Failed to build one-shot agent stdin payload: stdin"
        );
        assert_eq!(
            execution_message,
            "Failed to execute one-shot agent command: stdin delivery failed: write"
        );
    }

    #[test]
    fn test_one_shot_cli_observer_updates_child_pid_slot() {
        // Arrange
        let child_pid = Arc::new(Mutex::new(None));
        let observer = OneShotCliObserver {
            child_pid: Some(Arc::clone(&child_pid)),
        };

        // Act
        observer.pid_updated(Some(42));
        let active_pid = *child_pid.lock().expect("PID lock should be available");
        observer.stdout_line("collected output");
        observer.pid_updated(None);
        let cleared_pid = *child_pid.lock().expect("PID lock should be available");

        // Assert
        assert_eq!(active_pid, Some(42));
        assert_eq!(cleared_pid, None);
    }

    #[tokio::test]
    async fn oversized_one_shot_responses_do_not_launch_repair() {
        // Arrange
        let folder = tempdir().expect("workspace");
        let oversized = "x".repeat(128 * 1024 + 1);
        let response_path = folder.path().join("response.txt");
        std::fs::write(&response_path, &oversized).expect("response fixture");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().times(1).returning(move |_| {
            let mut command = Command::new("cat");
            command.arg(&response_path);

            Ok(command)
        });
        let client = MockAppServerClient::new();
        let request = OneShotRequest {
            agent_kind: AgentKind::Claude,
            child_pid: None,
            folder: folder.path().to_owned(),
            model: AgentModel::ClaudeSonnet5,
            permission_mode: PermissionMode::AutoEdit,
            prompt: "Generate title".into(),
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
            speed_mode: SpeedMode::Normal,
        };

        // Act
        let cli_error = submit_one_shot_with_backend(&backend, request.clone())
            .await
            .expect_err("oversized CLI response");
        let native_error = attempt_one_shot_app_server_repair(
            &client,
            "bad JSON",
            &oversized,
            request,
            "repair-limit",
            None,
        )
        .await
        .expect_err("oversized native response");

        // Assert
        assert!(cli_error.contains("lossless repair limit"));
        assert!(native_error.contains("lossless repair limit"));
    }

    #[tokio::test]
    async fn test_submit_one_shot_with_backend_reports_signal_interruption() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            let mut command = Command::new("sh");
            command.arg("-c").arg("kill -9 $$");

            Ok(command)
        });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Codex,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        )
        .await
        .expect_err("signal termination should interrupt the one-shot command");

        // Assert
        assert_eq!(error, "One-shot agent command was interrupted");
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
            assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
            assert_eq!(request.prompt, "Generate title");

            Ok(mock_shell_command(
                r#"{"answer":"Generated title","questions":[]}"#,
                "",
                0,
            ))
        });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::ReadOnly,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
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
    /// Verifies one-shot execution rejects plain-text utility output after
    /// both the original parse and the protocol-repair retry fail.
    async fn test_submit_one_shot_with_backend_rejects_plain_text_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(2)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));

                Ok(mock_shell_command("plain text", "", 0))
            });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Codex,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        )
        .await
        .expect_err("plain-text utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("debug_details:"));
        assert!(error.contains("direct_json_error_location: line 1, column 1"));
        assert!(error.contains("response:\nplain text"));
    }

    #[tokio::test]
    /// Verifies one-shot execution rejects wrapped non-schema utility output
    /// after both the original parse and the protocol-repair retry fail.
    async fn test_submit_one_shot_with_backend_rejects_wrapped_plain_text_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(2)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
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
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        )
        .await
        .expect_err("wrapped plain-text utility output should fail");

        // Assert — the provider parser extracts "plain text" from the
        // `result` wrapper, so the protocol parser sees raw text, not JSON
        // keys.
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("direct_json_error:"));
        assert!(error.contains("response:\nplain text"));
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
                        r#"{"answer":"Generated title","questions":[]}"#
                    ),
                    "",
                    0,
                ))
            });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
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
    /// Verifies one-shot execution recovers valid output when the initial
    /// parse fails but the protocol-repair retry returns valid protocol JSON.
    async fn test_submit_one_shot_with_backend_recovers_via_protocol_repair() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let call_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().times(2).returning({
            let counter = std::sync::Arc::clone(&call_counter);

            move |request| {
                assert_eq!(request.speed_mode, SpeedMode::Fast);
                let call_number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if call_number == 0 {
                    Ok(mock_shell_command("plain text", "", 0))
                } else {
                    Ok(mock_shell_command(
                        r#"{"answer":"Repaired title","questions":[]}"#,
                        "",
                        0,
                    ))
                }
            }
        });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Codex,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Fast,
            },
        )
        .await
        .expect("repair retry should succeed");

        // Assert
        assert_eq!(
            response.response.answers(),
            vec!["Repaired title".to_string()]
        );
    }

    #[tokio::test]
    async fn focused_review_repairs_trailing_text_with_direct_review_schema() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let call_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().times(2).returning({
            let counter = Arc::clone(&call_counter);

            move |request| {
                assert_eq!(request.request_kind, &AgentRequestKind::FocusedReview);
                let call_number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if call_number == 0 {
                    return Ok(mock_shell_command(
                        r#"{"project_impact":[],"suggestions":[]} trailing text"#,
                        "",
                        0,
                    ));
                }

                assert!(request.prompt.contains("Complete malformed response:"));
                let prompt = crate::agent::prompt::build_cli_prompt_text(
                    request,
                    ag_protocol::ProtocolSchemaInstructionMode::PromptSchema,
                    "Gemini",
                )
                .expect("repair envelope");
                assert!(prompt.contains("\"title\": \"FocusedReview\""));
                assert_eq!(prompt.matches("Authoritative JSON Schema:").count(), 1);
                assert!(!prompt.contains("\"answer\""));

                Ok(mock_shell_command(
                    r#"{"project_impact":["Review repaired."],"suggestions":[]}"#,
                    "",
                    0,
                ))
            }
        });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::ReadOnly,
                prompt: "Review the diff".to_string(),
                request_kind: AgentRequestKind::FocusedReview,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        )
        .await
        .expect("focused review repair should succeed");

        // Assert
        assert_eq!(
            response.response.answer,
            r#"{"project_impact":["Review repaired."],"suggestions":[]}"#
        );
    }

    #[tokio::test]
    /// Verifies one-shot execution still rejects blank utility responses
    /// after both the original parse and the protocol-repair retry fail.
    async fn test_submit_one_shot_with_backend_rejects_blank_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|request| {
            assert!(matches!(
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));

            Ok(mock_shell_command("   ", "", 0))
        });

        // Act
        let error = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                agent_kind: AgentKind::Codex,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        )
        .await
        .expect_err("blank utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("trimmed_len: 0 chars"));
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
                 '{\"answer\":\"done\",\"questions\":[]}'",
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
                    agent_kind: AgentKind::Claude,
                    child_pid: None,
                    folder: temp_directory.path().to_path_buf(),
                    model: AgentModel::ClaudeSonnet5,
                    permission_mode: PermissionMode::AutoEdit,
                    prompt: large_prompt.clone(),
                    request_kind: AgentRequestKind::UtilityPrompt,
                    reasoning_level: ReasoningLevel::default(),
                    speed_mode: SpeedMode::Normal,
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
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
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
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::AutoEdit,
                prompt: large_prompt,
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
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
                agent_kind: AgentKind::Claude,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::ClaudeSonnet5,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
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
                assert_eq!(request.model, AgentModel::Gpt56Sol.as_str());
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));
                assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
                assert_eq!(request.prompt.text, "Generate title");
                assert_eq!(request.speed_mode, SpeedMode::Fast);

                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"Generated title","questions":[]}"#
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
                agent_kind: AgentKind::Codex,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::ReadOnly,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Fast,
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
    /// Verifies app-server turn failures shut down the temporary session and
    /// clear the caller's shared child-process slot.
    async fn test_submit_one_shot_with_app_server_client_clears_pid_after_turn_failure() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let child_pid = Arc::new(Mutex::new(Some(42)));
        let mut app_server_client = MockAppServerClient::new();
        app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|_, _| {
                Box::pin(async {
                    Err(AppServerError::Provider(
                        "app-server turn failed".to_string(),
                    ))
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
                agent_kind: AgentKind::Codex,
                child_pid: Some(Arc::clone(&child_pid)),
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        )
        .await
        .expect_err("app-server turn failure should surface");

        // Assert
        assert!(error.contains("app-server turn failed"));
        assert_eq!(
            *child_pid.lock().expect("child pid lock should succeed"),
            None
        );
    }

    #[tokio::test]
    async fn one_shot_app_server_repair_preserves_permissions_and_conversation() {
        for permission_mode in PermissionMode::ALL {
            // Arrange
            let folder = tempdir().expect("workspace");
            let mut client = MockAppServerClient::new();
            client
                .expect_run_turn()
                .times(1)
                .returning(move |request, _| {
                    assert_eq!(request.permission_mode, permission_mode);
                    assert_eq!(request.session_id, "one-shot-session");
                    assert_eq!(
                        request.provider_conversation_id.as_deref(),
                        Some("native-session")
                    );

                    Box::pin(async {
                        Ok(AppServerTurnResponse {
                            assistant_message: r#"{"answer":"Repaired","questions":[]}"#.into(),
                            context_reset: false,
                            input_tokens: 2,
                            output_tokens: 1,
                            pid: None,
                            provider_conversation_id: Some("native-session".into()),
                        })
                    })
                });
            let request = OneShotRequest {
                agent_kind: AgentKind::Gemini,
                child_pid: None,
                folder: folder.path().to_owned(),
                model: AgentModel::Gemini31Pro,
                permission_mode,
                prompt: "Generate title".into(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            };

            // Act
            let (response, input_tokens, output_tokens) = attempt_one_shot_app_server_repair(
                &client,
                "invalid JSON",
                "malformed",
                request,
                "one-shot-session",
                Some("native-session"),
            )
            .await
            .expect("repair succeeds");

            // Assert
            assert_eq!(response.to_display_text(), "Repaired");
            assert_eq!((input_tokens, output_tokens), (2, 1));
        }
    }

    #[tokio::test]
    /// Verifies app-server-backed one-shot execution rejects plain-text
    /// utility output after both the original parse and the protocol-repair
    /// retry fail.
    async fn test_submit_one_shot_with_app_server_client_rejects_plain_text_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut app_server_client = MockAppServerClient::new();
        app_server_client
            .expect_run_turn()
            .times(2)
            .returning(|request, _| {
                assert_eq!(request.model, AgentModel::Gpt56Sol.as_str());
                assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
                assert_eq!(request.speed_mode, SpeedMode::Fast);

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
                agent_kind: AgentKind::Codex,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::ReadOnly,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Fast,
            },
        )
        .await
        .expect_err("plain-text utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("debug_details:"));
        assert!(error.contains("response:\nplain text"));
    }

    #[tokio::test]
    /// Verifies app-server-backed non-utility one-shot execution still
    /// rejects plain-text output after both the original parse and the
    /// protocol-repair retry fail.
    async fn test_submit_one_shot_with_app_server_client_rejects_plain_text_non_utility_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let mut app_server_client = MockAppServerClient::new();
        app_server_client
            .expect_run_turn()
            .times(2)
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
                agent_kind: AgentKind::Codex,
                child_pid: None,
                folder: temp_directory.path().to_path_buf(),
                model: AgentModel::Gpt56Sol,
                permission_mode: PermissionMode::AutoEdit,
                prompt: "Generate title".to_string(),
                request_kind: AgentRequestKind::SessionStart,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            },
        )
        .await
        .expect_err("invalid non-utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("debug_details:"));
        assert!(error.contains("response:\nplain text"));
    }
}
