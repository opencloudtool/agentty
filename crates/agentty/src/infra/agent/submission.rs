//! One-shot agent prompt execution helpers.
//!
//! These helpers run isolated utility prompts outside the long-lived session
//! turn flow while still enforcing the shared structured response protocol.

use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::sync::Mutex;

use super::backend::{AgentBackend, AgentCommandMode, BuildCommandRequest};
use super::protocol::{AgentResponse, build_protocol_repair_prompt, parse_agent_response_strict};
use super::{ParsedResponse, create_backend, parse_response};
use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::session::SessionStats;

/// Maximum number of repair attempts for one-shot protocol responses.
const MAX_ONE_SHOT_PROTOCOL_REPAIR_ATTEMPTS: usize = 3;

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
    /// Reasoning effort preference for the one-shot prompt.
    pub(crate) reasoning_level: ReasoningLevel,
}

/// Parsed result returned by one isolated prompt execution.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OneShotSubmission {
    /// Structured protocol response parsed from the final successful attempt.
    pub(crate) response: AgentResponse,
    /// Aggregated token usage across the initial attempt and any repair turns.
    pub(crate) stats: SessionStats,
}

/// Executes one isolated prompt and returns the parsed structured response.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output cannot be repaired into valid protocol JSON.
pub(crate) async fn submit_one_shot(request: OneShotRequest<'_>) -> Result<AgentResponse, String> {
    let submission = submit_one_shot_with_stats(request).await?;

    Ok(submission.response)
}

/// Executes one isolated prompt and returns the parsed structured response
/// plus aggregated usage statistics.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output cannot be repaired into valid protocol JSON.
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
/// or the final output cannot be repaired into valid protocol JSON.
pub(crate) async fn submit_one_shot_with_backend(
    backend: &dyn AgentBackend,
    request: OneShotRequest<'_>,
) -> Result<OneShotSubmission, String> {
    let mut total_stats = SessionStats::default();
    let mut last_response = execute_one_shot_command(backend, request.prompt, request).await?;
    accumulate_stats(&mut total_stats, &last_response.stats);
    let mut last_error = None;

    for attempt in 0..=MAX_ONE_SHOT_PROTOCOL_REPAIR_ATTEMPTS {
        match parse_agent_response_strict(&last_response.content) {
            Ok(agent_response) => {
                return Ok(OneShotSubmission {
                    response: agent_response,
                    stats: total_stats,
                });
            }
            Err(error) => last_error = Some(error),
        }

        if attempt == MAX_ONE_SHOT_PROTOCOL_REPAIR_ATTEMPTS {
            break;
        }

        let repair_prompt = build_protocol_repair_prompt(&last_response.content);
        last_response = execute_one_shot_command(backend, &repair_prompt, request).await?;
        accumulate_stats(&mut total_stats, &last_response.stats);
    }

    let parse_error =
        last_error.unwrap_or(crate::infra::agent::protocol::AgentResponseParseError::InvalidFormat);

    Err(format!(
        "One-shot agent output did not match the required JSON schema after \
         {MAX_ONE_SHOT_PROTOCOL_REPAIR_ATTEMPTS} repair attempts: {parse_error}"
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
    request: OneShotRequest<'_>,
) -> Result<ParsedResponse, String> {
    let command = backend
        .build_command(BuildCommandRequest {
            reasoning_level: request.reasoning_level,
            folder: request.folder,
            mode: AgentCommandMode::OneShot { prompt },
            model: request.model.as_str(),
        })
        .map_err(|error| format!("Failed to build one-shot agent command: {error}"))?;
    let mut tokio_command = tokio::process::Command::from(command);
    tokio_command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut pid_guard = ChildPidGuard::new(request.child_pid);
    let child = tokio_command
        .spawn()
        .map_err(|error| format!("Failed to execute one-shot agent command: {error}"))?;
    pid_guard.update_from_child(&child);
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("Failed to execute one-shot agent command: {error}"))?;

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

/// Adds one parsed token-usage payload into the aggregated one-shot totals.
fn accumulate_stats(total_stats: &mut SessionStats, next_stats: &SessionStats) {
    total_stats.input_tokens += next_stats.input_tokens;
    total_stats.output_tokens += next_stats.output_tokens;
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::tempdir;

    use super::*;
    use crate::infra::agent::tests::MockAgentBackend;

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

            Ok(mock_shell_command(
                r#"{"messages":[{"type":"answer","text":"Generated title"}]}"#,
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
    /// Verifies one-shot execution runs a repair prompt when the first
    /// response is not valid protocol JSON.
    async fn test_submit_one_shot_with_backend_repairs_invalid_protocol_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let call_count = Arc::new(AtomicUsize::new(0));
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().times(2).returning({
            let call_count = Arc::clone(&call_count);

            move |request| {
                let attempt = call_count.fetch_add(1, Ordering::SeqCst);

                match attempt {
                    0 => {
                        assert!(matches!(
                            request.mode,
                            AgentCommandMode::OneShot {
                                prompt: "Generate title",
                            }
                        ));

                        Ok(mock_shell_command("plain text", "", 0))
                    }
                    1 => {
                        assert!(matches!(request.mode, AgentCommandMode::OneShot { .. }));
                        let prompt = match request.mode {
                            AgentCommandMode::OneShot { prompt } => prompt,
                            _ => "",
                        };
                        assert!(
                            prompt.contains(
                                "Your previous response did not match the required JSON schema."
                            ),
                            "repair prompt should explain the protocol failure"
                        );

                        Ok(mock_shell_command(
                            r#"{"messages":[{"type":"answer","text":"Recovered title"}]}"#,
                            "",
                            0,
                        ))
                    }
                    _ => Ok(mock_shell_command("", "unexpected extra repair attempt", 1)),
                }
            }
        });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::Gpt54,
                prompt: "Generate title",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect("repair should succeed");

        // Assert
        assert_eq!(
            response.response.answers(),
            vec!["Recovered title".to_string()]
        );
    }

    #[tokio::test]
    /// Verifies one-shot usage totals accumulate across protocol repair turns.
    async fn test_submit_one_shot_with_backend_accumulates_stats_across_repairs() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let call_count = Arc::new(AtomicUsize::new(0));
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().times(2).returning({
            let call_count = Arc::clone(&call_count);

            move |request| {
                let attempt = call_count.fetch_add(1, Ordering::SeqCst);

                match attempt {
                    0 => {
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
                    }
                    1 => {
                        let prompt = match request.mode {
                            AgentCommandMode::OneShot { prompt } => prompt,
                            _ => "",
                        };
                        assert!(
                            prompt.contains(
                                "Your previous response did not match the required JSON schema."
                            ),
                            "repair prompt should explain the protocol failure"
                        );

                        Ok(mock_shell_command(
                            r#"{"result":"{\"messages\":[{\"type\":\"answer\",\"text\":\"Recovered title\"}]}","usage":{"input_tokens":3,"output_tokens":2}}"#,
                            "",
                            0,
                        ))
                    }
                    _ => unreachable!("unexpected extra repair attempt"),
                }
            }
        });

        // Act
        let response = submit_one_shot_with_backend(
            &backend,
            OneShotRequest {
                child_pid: None,
                folder: temp_directory.path(),
                model: AgentModel::ClaudeSonnet46,
                prompt: "Generate title",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .await
        .expect("repair should succeed");

        // Assert
        assert_eq!(response.stats.input_tokens, 5);
        assert_eq!(response.stats.output_tokens, 3);
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
