use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};

use crate::domain::permission::PermissionMode;

const CODEX_APP_SERVER_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const CODEX_APP_SERVER_TURN_TIMEOUT: Duration = Duration::from_mins(5);

/// Canonical wire-level policy mapping for one [`PermissionMode`].
///
/// The fields map directly to Codex app-server request/approval payload
/// fields so mode behavior stays consistent across thread start, turn start,
/// and pre-action approval responses.
struct PermissionModePolicy {
    approval_policy: &'static str,
    legacy_pre_action_decision: &'static str,
    pre_action_decision: &'static str,
    thread_sandbox_mode: &'static str,
    turn_sandbox_type: &'static str,
}

const AUTO_EDIT_POLICY: PermissionModePolicy = PermissionModePolicy {
    approval_policy: "on-request",
    legacy_pre_action_decision: "approved",
    pre_action_decision: "accept",
    thread_sandbox_mode: "workspace-write",
    turn_sandbox_type: "workspaceWrite",
};

const AUTONOMOUS_POLICY: PermissionModePolicy = PermissionModePolicy {
    approval_policy: "never",
    legacy_pre_action_decision: "approved_for_session",
    pre_action_decision: "acceptForSession",
    thread_sandbox_mode: "danger-full-access",
    turn_sandbox_type: "dangerFullAccess",
};

const PLAN_POLICY: PermissionModePolicy = PermissionModePolicy {
    approval_policy: "on-request",
    legacy_pre_action_decision: "denied",
    pre_action_decision: "decline",
    thread_sandbox_mode: "read-only",
    turn_sandbox_type: "readOnly",
};

/// Boxed async result used by [`CodexAppServerClient`] trait methods.
pub type CodexAppServerFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Input payload for one Codex app-server turn execution.
#[derive(Clone)]
pub struct CodexTurnRequest {
    pub folder: PathBuf,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub prompt: String,
    pub session_output: Option<String>,
    pub session_id: String,
}

/// Normalized result for one Codex app-server turn.
pub struct CodexTurnResponse {
    pub assistant_message: String,
    pub context_reset: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub pid: Option<u32>,
}

/// Persistent Codex app-server session boundary used by session workers.
#[cfg_attr(test, mockall::automock)]
pub trait CodexAppServerClient: Send + Sync {
    /// Executes one prompt turn for a session and returns normalized output.
    fn run_turn(
        &self,
        request: CodexTurnRequest,
    ) -> CodexAppServerFuture<Result<CodexTurnResponse, String>>;

    /// Stops and forgets a session runtime, if one exists.
    fn shutdown_session(&self, session_id: String) -> CodexAppServerFuture<()>;
}

/// Production [`CodexAppServerClient`] backed by `codex app-server` process
/// instances.
pub struct RealCodexAppServerClient {
    sessions: Arc<Mutex<HashMap<String, CodexSessionRuntime>>>,
}

impl RealCodexAppServerClient {
    /// Creates an empty app-server runtime registry.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &Mutex<HashMap<String, CodexSessionRuntime>>,
        request: CodexTurnRequest,
    ) -> Result<CodexTurnResponse, String> {
        let mut context_reset = false;
        let mut session_runtime = Self::take_session(sessions, &request.session_id)?;

        if session_runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.matches_request(&request))
        {
            if let Some(runtime) = session_runtime.as_mut() {
                Self::shutdown_runtime(runtime).await;
            }

            session_runtime = None;
            context_reset = true;
        }

        let mut session_runtime = match session_runtime {
            Some(existing_runtime) => existing_runtime,
            None => Self::start_runtime(&request).await?,
        };
        let first_attempt_prompt = Self::turn_prompt_for_runtime(
            request.prompt.as_str(),
            request.session_output.as_deref(),
            context_reset,
        );

        let first_attempt =
            Self::run_turn_with_runtime(&mut session_runtime, &first_attempt_prompt).await;
        if let Ok((assistant_message, input_tokens, output_tokens)) = first_attempt {
            let pid = session_runtime.child.id();
            Self::store_session(sessions, request.session_id, session_runtime)?;

            return Ok(CodexTurnResponse {
                assistant_message,
                context_reset,
                input_tokens,
                output_tokens,
                pid,
            });
        }

        let first_error = first_attempt
            .err()
            .unwrap_or_else(|| "Codex app-server turn failed".to_string());
        Self::shutdown_runtime(&mut session_runtime).await;

        let mut restarted_runtime = Self::start_runtime(&request).await?;
        let retry_attempt_prompt = Self::turn_prompt_for_runtime(
            request.prompt.as_str(),
            request.session_output.as_deref(),
            true,
        );
        let retry_attempt =
            Self::run_turn_with_runtime(&mut restarted_runtime, &retry_attempt_prompt).await;
        match retry_attempt {
            Ok((assistant_message, input_tokens, output_tokens)) => {
                let pid = restarted_runtime.child.id();
                Self::store_session(sessions, request.session_id, restarted_runtime)?;

                Ok(CodexTurnResponse {
                    assistant_message,
                    context_reset: true,
                    input_tokens,
                    output_tokens,
                    pid,
                })
            }
            Err(retry_error) => {
                Self::shutdown_runtime(&mut restarted_runtime).await;

                Err(format!(
                    "Codex app-server failed, then retry failed after restart: first error: \
                     {first_error}; retry error: {retry_error}"
                ))
            }
        }
    }

    /// Removes and returns a stored runtime for `session_id`.
    fn take_session(
        sessions: &Mutex<HashMap<String, CodexSessionRuntime>>,
        session_id: &str,
    ) -> Result<Option<CodexSessionRuntime>, String> {
        let mut sessions = sessions
            .lock()
            .map_err(|_| "Failed to lock Codex app-server session map".to_string())?;

        Ok(sessions.remove(session_id))
    }

    /// Stores or replaces the runtime for `session_id`.
    fn store_session(
        sessions: &Mutex<HashMap<String, CodexSessionRuntime>>,
        session_id: String,
        session: CodexSessionRuntime,
    ) -> Result<(), String> {
        let mut sessions = sessions
            .lock()
            .map_err(|_| "Failed to lock Codex app-server session map".to_string())?;
        sessions.insert(session_id, session);

        Ok(())
    }

    /// Starts `codex app-server`, initializes it, and creates a thread.
    async fn start_runtime(request: &CodexTurnRequest) -> Result<CodexSessionRuntime, String> {
        let mut command = tokio::process::Command::new("codex");
        command.arg("--model").arg(&request.model);

        command
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .current_dir(&request.folder)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = command
            .spawn()
            .map_err(|error| format!("Failed to spawn `codex app-server`: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Codex app-server stdin is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Codex app-server stdout is unavailable".to_string())?;
        let mut session = CodexSessionRuntime {
            child,
            folder: request.folder.clone(),
            model: request.model.clone(),
            permission_mode: request.permission_mode,
            stdin,
            stdout_lines: BufReader::new(stdout).lines(),
            thread_id: String::new(),
        };

        Self::initialize_runtime(&mut session).await?;
        let thread_id = Self::start_thread(&mut session).await?;
        session.thread_id = thread_id;

        Ok(session)
    }

    /// Sends the initialize handshake for one app-server process.
    async fn initialize_runtime(session: &mut CodexSessionRuntime) -> Result<(), String> {
        let initialize_id = format!("init-{}", uuid::Uuid::new_v4());
        let initialize_payload = serde_json::json!({
            "method": "initialize",
            "id": initialize_id,
            "params": {
                "clientInfo": {
                    "name": "agentty",
                    "title": "agentty",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": true,
                    "optOutNotificationMethods": Value::Null
                }
            }
        });
        Self::write_json_line(&mut session.stdin, &initialize_payload).await?;
        Self::wait_for_response_line(&mut session.stdout_lines, &initialize_id).await?;

        let runtime_initialized_payload = serde_json::json!({
            "method": "initialized",
            "params": {}
        });
        Self::write_json_line(&mut session.stdin, &runtime_initialized_payload).await?;

        Ok(())
    }

    /// Starts one Codex thread and returns its identifier.
    async fn start_thread(session: &mut CodexSessionRuntime) -> Result<String, String> {
        let thread_start_id = format!("thread-start-{}", uuid::Uuid::new_v4());
        let thread_start_payload = serde_json::json!({
            "method": "thread/start",
            "id": thread_start_id,
            "params": {
                "model": Value::Null,
                "modelProvider": Value::Null,
                "cwd": Value::Null,
                "approvalPolicy": Self::approval_policy(session.permission_mode),
                "sandbox": Self::thread_sandbox_mode(session.permission_mode),
                "config": Value::Null,
                "baseInstructions": Value::Null,
                "developerInstructions": Value::Null,
                "personality": Value::Null,
                "ephemeral": Value::Null,
                "dynamicTools": Value::Null,
                "mockExperimentalField": Value::Null,
                "experimentalRawEvents": false,
                "persistExtendedHistory": false
            }
        });

        Self::write_json_line(&mut session.stdin, &thread_start_payload).await?;
        let response_line =
            Self::wait_for_response_line(&mut session.stdout_lines, &thread_start_id).await?;
        let response_value = serde_json::from_str::<Value>(&response_line)
            .map_err(|error| format!("Failed to parse thread/start response JSON: {error}"))?;

        response_value
            .get("result")
            .and_then(|result| result.get("thread"))
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| {
                "Codex app-server `thread/start` response does not include a thread id".to_string()
            })
    }

    /// Sends one turn prompt and waits for terminal completion notification.
    async fn run_turn_with_runtime(
        session: &mut CodexSessionRuntime,
        prompt: &str,
    ) -> Result<(String, u64, u64), String> {
        let turn_start_id = format!("turn-start-{}", uuid::Uuid::new_v4());
        let turn_start_payload = serde_json::json!({
            "method": "turn/start",
            "id": turn_start_id,
            "params": {
                "threadId": session.thread_id,
                "input": [{
                    "type": "text",
                    "text": prompt,
                    "text_elements": []
                }],
                "cwd": Value::Null,
                "approvalPolicy": Self::approval_policy(session.permission_mode),
                "sandboxPolicy": Self::turn_sandbox_policy(session.permission_mode),
                "model": Value::Null,
                "effort": Value::Null,
                "summary": Value::Null,
                "personality": Value::Null,
                "outputSchema": Value::Null
            }
        });
        Self::write_json_line(&mut session.stdin, &turn_start_payload).await?;

        let mut assistant_messages = Vec::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        tokio::time::timeout(CODEX_APP_SERVER_TURN_TIMEOUT, async {
            loop {
                let stdout_line = session
                    .stdout_lines
                    .next_line()
                    .await
                    .map_err(|error| format!("Failed reading Codex app-server stdout: {error}"))?
                    .ok_or_else(|| {
                        "Codex app-server terminated before `turn/completed` was received"
                            .to_string()
                    })?;

                if stdout_line.trim().is_empty() {
                    continue;
                }

                if let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) {
                    if response_id_matches(&response_value, &turn_start_id)
                        && response_value.get("error").is_some()
                    {
                        return Err(extract_json_error_message(&response_value).unwrap_or_else(
                            || "Codex app-server returned an error for `turn/start`".to_string(),
                        ));
                    }

                    if let Some(approval_response) = Self::build_pre_action_approval_response(
                        session.permission_mode,
                        &response_value,
                    ) {
                        Self::write_json_line(&mut session.stdin, &approval_response).await?;

                        continue;
                    }

                    if let Some(message) = extract_agent_message(&response_value) {
                        assistant_messages.push(message);
                    }

                    let (turn_input_tokens, turn_output_tokens) =
                        extract_turn_usage(&response_value);
                    input_tokens = input_tokens.saturating_add(turn_input_tokens);
                    output_tokens = output_tokens.saturating_add(turn_output_tokens);

                    if let Some(turn_result) = parse_turn_completed(&response_value) {
                        return turn_result.map(|()| {
                            let assistant_message = assistant_messages.join("\n\n");
                            (assistant_message, input_tokens, output_tokens)
                        });
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            format!(
                "Timed out waiting for Codex app-server `turn/completed` after {} seconds",
                CODEX_APP_SERVER_TURN_TIMEOUT.as_secs()
            )
        })?
    }

    /// Returns the turn prompt, replaying session output when context was
    /// reset and previous transcript is available.
    fn turn_prompt_for_runtime(
        prompt: &str,
        session_output: Option<&str>,
        context_reset: bool,
    ) -> String {
        if !context_reset {
            return prompt.to_string();
        }

        crate::infra::agent::build_resume_prompt(prompt, session_output)
    }

    /// Returns the app-server approval policy used for one permission mode.
    fn approval_policy(permission_mode: PermissionMode) -> &'static str {
        Self::permission_mode_policy(permission_mode).approval_policy
    }

    /// Returns the thread-level sandbox mode used for one permission mode.
    fn thread_sandbox_mode(permission_mode: PermissionMode) -> &'static str {
        Self::permission_mode_policy(permission_mode).thread_sandbox_mode
    }

    /// Returns the turn-level sandbox policy object for one permission mode.
    fn turn_sandbox_policy(permission_mode: PermissionMode) -> Value {
        serde_json::json!({
            "type": Self::permission_mode_policy(permission_mode).turn_sandbox_type
        })
    }

    /// Returns the modern pre-action approval decision for one permission
    /// mode.
    fn pre_action_approval_decision(permission_mode: PermissionMode) -> &'static str {
        Self::permission_mode_policy(permission_mode).pre_action_decision
    }

    /// Returns the legacy pre-action approval decision for one permission
    /// mode.
    fn legacy_pre_action_approval_decision(permission_mode: PermissionMode) -> &'static str {
        Self::permission_mode_policy(permission_mode).legacy_pre_action_decision
    }

    /// Returns the canonical wire-level policy for one permission mode.
    fn permission_mode_policy(permission_mode: PermissionMode) -> &'static PermissionModePolicy {
        match permission_mode {
            PermissionMode::AutoEdit => &AUTO_EDIT_POLICY,
            PermissionMode::Autonomous => &AUTONOMOUS_POLICY,
            PermissionMode::Plan => &PLAN_POLICY,
        }
    }

    /// Builds a JSON-RPC approval response for known pre-action request
    /// methods.
    ///
    /// Returns `None` when the input line is not a supported approval request
    /// or does not include a request id.
    fn build_pre_action_approval_response(
        permission_mode: PermissionMode,
        response_value: &Value,
    ) -> Option<Value> {
        let method = response_value.get("method")?.as_str()?;
        let request_id = response_value.get("id")?.clone();
        let decision = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                Self::pre_action_approval_decision(permission_mode)
            }
            "execCommandApproval" | "applyPatchApproval" => {
                Self::legacy_pre_action_approval_decision(permission_mode)
            }
            _ => return None,
        };

        Some(serde_json::json!({
            "id": request_id,
            "result": {
                "decision": decision
            }
        }))
    }

    /// Writes one JSON line payload to app-server stdin.
    async fn write_json_line(
        stdin: &mut tokio::process::ChildStdin,
        payload: &Value,
    ) -> Result<(), String> {
        let serialized_payload = payload.to_string();

        stdin
            .write_all(serialized_payload.as_bytes())
            .await
            .map_err(|error| format!("Failed writing to Codex app-server stdin: {error}"))?;
        stdin.write_all(b"\n").await.map_err(|error| {
            format!("Failed writing newline to Codex app-server stdin: {error}")
        })?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("Failed flushing Codex app-server stdin: {error}"))
    }

    /// Waits until a response line carrying `response_id` is observed.
    async fn wait_for_response_line<R>(
        stdout_lines: &mut Lines<BufReader<R>>,
        response_id: &str,
    ) -> Result<String, String>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        tokio::time::timeout(CODEX_APP_SERVER_STARTUP_TIMEOUT, async {
            loop {
                let stdout_line = stdout_lines
                    .next_line()
                    .await
                    .map_err(|error| format!("Failed reading Codex app-server stdout: {error}"))?
                    .ok_or_else(|| {
                        "Codex app-server terminated before sending expected response".to_string()
                    })?;

                let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                    continue;
                };
                if response_id_matches(&response_value, response_id) {
                    return Ok(stdout_line);
                }
            }
        })
        .await
        .map_err(|_| {
            format!(
                "Timed out waiting for Codex app-server response `{response_id}` after {} seconds",
                CODEX_APP_SERVER_STARTUP_TIMEOUT.as_secs()
            )
        })?
    }

    /// Terminates one runtime process and waits for process exit.
    async fn shutdown_runtime(session: &mut CodexSessionRuntime) {
        let _ = session.stdin.shutdown().await;

        if tokio::time::timeout(Duration::from_secs(1), session.child.wait())
            .await
            .is_err()
        {
            let _ = session.child.kill().await;
            let _ = session.child.wait().await;
        }
    }
}

impl Default for RealCodexAppServerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAppServerClient for RealCodexAppServerClient {
    fn run_turn(
        &self,
        request: CodexTurnRequest,
    ) -> CodexAppServerFuture<Result<CodexTurnResponse, String>> {
        let sessions = Arc::clone(&self.sessions);

        Box::pin(async move { Self::run_turn_internal(sessions.as_ref(), request).await })
    }

    fn shutdown_session(&self, session_id: String) -> CodexAppServerFuture<()> {
        let sessions = Arc::clone(&self.sessions);

        Box::pin(async move {
            let mut session = {
                let Ok(mut sessions) = sessions.lock() else {
                    return;
                };

                sessions.remove(&session_id)
            };

            if let Some(session) = session.as_mut() {
                RealCodexAppServerClient::shutdown_runtime(session).await;
            }
        })
    }
}

struct CodexSessionRuntime {
    child: tokio::process::Child,
    folder: PathBuf,
    model: String,
    permission_mode: PermissionMode,
    stdin: tokio::process::ChildStdin,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
    thread_id: String,
}

impl CodexSessionRuntime {
    /// Returns whether the stored runtime configuration matches one request.
    fn matches_request(&self, request: &CodexTurnRequest) -> bool {
        self.folder == request.folder
            && self.model == request.model
            && self.permission_mode == request.permission_mode
    }
}

/// Returns whether a JSON response line has the requested id.
fn response_id_matches(response_value: &Value, response_id: &str) -> bool {
    response_value
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|line_id| line_id == response_id)
}

/// Extracts a top-level JSON-RPC style error message from one line.
fn extract_json_error_message(response_value: &Value) -> Option<String> {
    response_value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Extracts completed assistant message text from an `item/completed` line.
fn extract_agent_message(response_value: &Value) -> Option<String> {
    if response_value.get("method").and_then(Value::as_str) != Some("item/completed") {
        return None;
    }

    let item = response_value.get("params")?.get("item")?;
    let item_type = item.get("type")?.as_str()?.to_ascii_lowercase();
    if !(item_type == "agentmessage" || item_type == "agent_message") {
        return None;
    }

    if let Some(item_text) = item.get("text").and_then(Value::as_str) {
        return Some(item_text.to_string());
    }

    let content = item.get("content")?.as_array()?;
    let mut parts = Vec::new();

    for content_item in content {
        if let Some(text) = content_item.get("text").and_then(Value::as_str) {
            parts.push(text.to_string());
        }
    }

    if parts.is_empty() {
        return None;
    }

    Some(parts.join("\n\n"))
}

/// Parses `turn/completed` notifications and maps failures to user errors.
fn parse_turn_completed(response_value: &Value) -> Option<Result<(), String>> {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return None;
    }

    let status = response_value
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status == "failed" {
        let message = response_value
            .get("params")
            .and_then(|params| params.get("turn"))
            .and_then(|turn| turn.get("error"))
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Codex app-server turn failed")
            .to_string();

        return Some(Err(message));
    }

    Some(Ok(()))
}

/// Extracts input/output token usage from `turn.usage` payloads.
fn extract_turn_usage(response_value: &Value) -> (u64, u64) {
    let Some(turn) = response_value
        .get("params")
        .and_then(|params| params.get("turn"))
    else {
        return (0, 0);
    };

    let Some(usage) = turn.get("usage") else {
        return (0, 0);
    };

    let input_tokens = usage
        .get("inputTokens")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = usage
        .get("outputTokens")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);

    (input_tokens, output_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_agent_message_returns_content_text_for_agent_message_item() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "agentMessage",
                    "content": [
                        {"type": "text", "text": "Line 1"},
                        {"type": "text", "text": "Line 2"}
                    ]
                }
            }
        });

        // Act
        let message = extract_agent_message(&response_value);

        // Assert
        assert_eq!(message, Some("Line 1\n\nLine 2".to_string()));
    }

    #[test]
    fn parse_turn_completed_returns_error_for_failed_turn() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "status": "failed",
                    "error": {"message": "boom"}
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value);

        // Assert
        assert_eq!(turn_result, Some(Err("boom".to_string())));
    }

    #[test]
    fn parse_turn_completed_returns_success_for_non_failed_turn() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "status": "completed"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value);

        // Assert
        assert_eq!(turn_result, Some(Ok(())));
    }

    #[test]
    fn extract_turn_usage_reads_camel_case_fields() {
        // Arrange
        let response_value = serde_json::json!({
            "params": {
                "turn": {
                    "usage": {
                        "inputTokens": 7,
                        "outputTokens": 3
                    }
                }
            }
        });

        // Act
        let usage = extract_turn_usage(&response_value);

        // Assert
        assert_eq!(usage, (7, 3));
    }

    #[test]
    fn approval_policy_maps_permission_modes() {
        // Arrange
        let auto_edit_mode = PermissionMode::AutoEdit;
        let autonomous_mode = PermissionMode::Autonomous;
        let plan_mode = PermissionMode::Plan;

        // Act
        let auto_edit_policy = RealCodexAppServerClient::approval_policy(auto_edit_mode);
        let autonomous_policy = RealCodexAppServerClient::approval_policy(autonomous_mode);
        let plan_policy = RealCodexAppServerClient::approval_policy(plan_mode);

        // Assert
        assert_eq!(auto_edit_policy, "on-request");
        assert_eq!(autonomous_policy, "never");
        assert_eq!(plan_policy, "on-request");
    }

    #[test]
    fn thread_sandbox_mode_maps_permission_modes() {
        // Arrange
        let auto_edit_mode = PermissionMode::AutoEdit;
        let autonomous_mode = PermissionMode::Autonomous;
        let plan_mode = PermissionMode::Plan;

        // Act
        let auto_edit_sandbox = RealCodexAppServerClient::thread_sandbox_mode(auto_edit_mode);
        let autonomous_sandbox = RealCodexAppServerClient::thread_sandbox_mode(autonomous_mode);
        let plan_sandbox = RealCodexAppServerClient::thread_sandbox_mode(plan_mode);

        // Assert
        assert_eq!(auto_edit_sandbox, "workspace-write");
        assert_eq!(autonomous_sandbox, "danger-full-access");
        assert_eq!(plan_sandbox, "read-only");
    }

    #[test]
    fn build_pre_action_approval_response_for_command_request_uses_mode_decision() {
        // Arrange
        let request_value = serde_json::json!({
            "id": "approval-1",
            "method": "item/commandExecution/requestApproval",
            "params": {
                "itemId": "item-1",
                "threadId": "thread-1",
                "turnId": "turn-1"
            }
        });

        // Act
        let response_value = RealCodexAppServerClient::build_pre_action_approval_response(
            PermissionMode::Plan,
            &request_value,
        );

        // Assert
        assert_eq!(
            response_value,
            Some(serde_json::json!({
                "id": "approval-1",
                "result": {
                    "decision": "decline"
                }
            }))
        );
    }

    #[test]
    fn build_pre_action_approval_response_for_legacy_request_uses_legacy_decision() {
        // Arrange
        let request_value = serde_json::json!({
            "id": "approval-2",
            "method": "execCommandApproval",
            "params": {
                "callId": "call-1",
                "command": "git status",
                "conversationId": "thread-1"
            }
        });

        // Act
        let response_value = RealCodexAppServerClient::build_pre_action_approval_response(
            PermissionMode::Autonomous,
            &request_value,
        );

        // Assert
        assert_eq!(
            response_value,
            Some(serde_json::json!({
                "id": "approval-2",
                "result": {
                    "decision": "approved_for_session"
                }
            }))
        );
    }

    #[test]
    fn build_pre_action_approval_response_returns_none_for_non_approval_method() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "notification-1",
            "method": "item/completed",
            "params": {}
        });

        // Act
        let decision = RealCodexAppServerClient::build_pre_action_approval_response(
            PermissionMode::AutoEdit,
            &response_value,
        );

        // Assert
        assert_eq!(decision, None);
    }

    #[test]
    fn turn_prompt_for_runtime_returns_original_prompt_without_context_reset() {
        // Arrange
        let prompt = "Implement feature";
        let session_output = Some("prior context");

        // Act
        let turn_prompt =
            RealCodexAppServerClient::turn_prompt_for_runtime(prompt, session_output, false);

        // Assert
        assert_eq!(turn_prompt, prompt);
    }

    #[test]
    fn turn_prompt_for_runtime_replays_session_output_after_context_reset() {
        // Arrange
        let prompt = "Implement feature";
        let session_output = Some("assistant: proposed plan");

        // Act
        let turn_prompt =
            RealCodexAppServerClient::turn_prompt_for_runtime(prompt, session_output, true);

        // Assert
        assert!(turn_prompt.contains("Continue this session using the full transcript below."));
        assert!(turn_prompt.contains("assistant: proposed plan"));
        assert!(turn_prompt.contains(prompt));
    }
}
