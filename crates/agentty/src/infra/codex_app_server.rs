use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::sync::mpsc;

use crate::domain::permission::PermissionMode;
use crate::infra::app_server_transport::{
    self, extract_json_error_message, response_id_matches, write_json_line,
};

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

/// Boxed async result used by [`CodexAppServerClient`] trait methods.
pub type CodexAppServerFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Incremental event emitted during a Codex app-server turn.
///
/// The caller receives these events through an [`mpsc::UnboundedSender`]
/// channel while the turn is in progress, enabling real-time streaming of
/// agent output and progress updates to the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum CodexStreamEvent {
    /// An `item/completed` agent message was received.
    AssistantMessage(String),
    /// An `item/started` event produced a progress description.
    ProgressUpdate(String),
}

/// Input payload for one Codex app-server turn execution.
#[derive(Clone)]
pub struct CodexTurnRequest {
    pub folder: PathBuf,
    pub model: String,
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
    ///
    /// Intermediate events (agent messages, progress updates) are sent through
    /// `stream_tx` as they arrive, enabling the caller to display streaming
    /// output before the turn completes.
    fn run_turn(
        &self,
        request: CodexTurnRequest,
        stream_tx: mpsc::UnboundedSender<CodexStreamEvent>,
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
        stream_tx: &mpsc::UnboundedSender<CodexStreamEvent>,
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
            Self::run_turn_with_runtime(&mut session_runtime, &first_attempt_prompt, stream_tx)
                .await;
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
            Self::run_turn_with_runtime(&mut restarted_runtime, &retry_attempt_prompt, stream_tx)
                .await;
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
        write_json_line(&mut session.stdin, &initialize_payload).await?;
        app_server_transport::wait_for_response_line(&mut session.stdout_lines, &initialize_id)
            .await?;

        let runtime_initialized_payload = serde_json::json!({
            "method": "initialized",
            "params": {}
        });
        write_json_line(&mut session.stdin, &runtime_initialized_payload).await?;

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
                "approvalPolicy": Self::approval_policy(),
                "sandbox": Self::thread_sandbox_mode(),
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

        write_json_line(&mut session.stdin, &thread_start_payload).await?;
        let response_line = app_server_transport::wait_for_response_line(
            &mut session.stdout_lines,
            &thread_start_id,
        )
        .await?;
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
    ///
    /// Intermediate agent messages and progress updates are emitted through
    /// `stream_tx` as they arrive from the app-server event stream.
    async fn run_turn_with_runtime(
        session: &mut CodexSessionRuntime,
        prompt: &str,
        stream_tx: &mpsc::UnboundedSender<CodexStreamEvent>,
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
                "approvalPolicy": Self::approval_policy(),
                "sandboxPolicy": Self::turn_sandbox_policy(),
                "model": Value::Null,
                "effort": Value::Null,
                "summary": Value::Null,
                "personality": Value::Null,
                "outputSchema": Value::Null
            }
        });
        write_json_line(&mut session.stdin, &turn_start_payload).await?;

        let mut assistant_messages = Vec::new();
        let mut active_turn_id: Option<String> = None;
        let mut latest_stream_usage: Option<(u64, u64)> = None;
        let mut completed_turn_usage: Option<(u64, u64)> = None;

        tokio::time::timeout(app_server_transport::TURN_TIMEOUT, async {
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
                    if response_id_matches(&response_value, &turn_start_id) {
                        if response_value.get("error").is_some() {
                            return Err(extract_json_error_message(&response_value)
                                .unwrap_or_else(|| {
                                    "Codex app-server returned an error for `turn/start`"
                                        .to_string()
                                }));
                        }
                        if active_turn_id.is_none() {
                            active_turn_id =
                                extract_turn_id_from_turn_start_response(&response_value);
                        }

                        continue;
                    }

                    if let Some(approval_response) =
                        Self::build_pre_action_approval_response(&response_value)
                    {
                        write_json_line(&mut session.stdin, &approval_response).await?;

                        continue;
                    }

                    if active_turn_id.is_none() {
                        active_turn_id =
                            extract_turn_id_from_turn_started_notification(&response_value);
                    }

                    if let Some(progress) = extract_item_started_progress(&response_value) {
                        let _ = stream_tx.send(CodexStreamEvent::ProgressUpdate(progress));
                    }

                    if let Some(message) = extract_agent_message(&response_value) {
                        let _ = stream_tx.send(CodexStreamEvent::AssistantMessage(message.clone()));
                        assistant_messages.push(message);
                    }

                    Self::update_turn_usage_from_response(
                        &response_value,
                        active_turn_id.as_deref(),
                        &mut completed_turn_usage,
                        &mut latest_stream_usage,
                    );

                    if let Some(turn_result) =
                        parse_turn_completed(&response_value, active_turn_id.as_deref())
                    {
                        let (input_tokens, output_tokens) =
                            Self::resolve_turn_usage(completed_turn_usage, latest_stream_usage);

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
                app_server_transport::TURN_TIMEOUT.as_secs()
            )
        })?
    }

    /// Resolves final turn usage by preferring `turn/completed` payload usage
    /// and falling back to the last seen usage update when completion omits it.
    fn resolve_turn_usage(
        completed_turn_usage: Option<(u64, u64)>,
        latest_stream_usage: Option<(u64, u64)>,
    ) -> (u64, u64) {
        completed_turn_usage
            .or(latest_stream_usage)
            .unwrap_or((0, 0))
    }

    /// Updates usage trackers for one app-server response line.
    ///
    /// Completion usage is tracked separately so final usage selection can
    /// prefer the `turn/completed` payload and only fall back to the latest
    /// non-completion usage update when needed.
    fn update_turn_usage_from_response(
        response_value: &Value,
        expected_turn_id: Option<&str>,
        completed_turn_usage: &mut Option<(u64, u64)>,
        latest_stream_usage: &mut Option<(u64, u64)>,
    ) {
        if let Some(turn_usage) = extract_turn_usage_for_turn(response_value, expected_turn_id) {
            if response_value.get("method").and_then(Value::as_str) == Some("turn/completed") {
                *completed_turn_usage = Some(turn_usage);
            } else {
                *latest_stream_usage = Some(turn_usage);
            }
        }
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
    fn approval_policy() -> &'static str {
        Self::permission_mode_policy(PermissionMode::default()).approval_policy
    }

    /// Returns the thread-level sandbox mode used for one permission mode.
    fn thread_sandbox_mode() -> &'static str {
        Self::permission_mode_policy(PermissionMode::default()).thread_sandbox_mode
    }

    /// Returns the turn-level sandbox policy object for one permission mode.
    fn turn_sandbox_policy() -> Value {
        serde_json::json!({
            "type": Self::permission_mode_policy(PermissionMode::default()).turn_sandbox_type
        })
    }

    /// Returns the modern pre-action approval decision for one permission
    /// mode.
    fn pre_action_approval_decision() -> &'static str {
        Self::permission_mode_policy(PermissionMode::default()).pre_action_decision
    }

    /// Returns the legacy pre-action approval decision for one permission
    /// mode.
    fn legacy_pre_action_approval_decision() -> &'static str {
        Self::permission_mode_policy(PermissionMode::default()).legacy_pre_action_decision
    }

    /// Returns the canonical wire-level policy for one permission mode.
    fn permission_mode_policy(permission_mode: PermissionMode) -> &'static PermissionModePolicy {
        match permission_mode {
            PermissionMode::AutoEdit => &AUTO_EDIT_POLICY,
        }
    }

    /// Builds a JSON-RPC approval response for known pre-action request
    /// methods.
    ///
    /// Returns `None` when the input line is not a supported approval request
    /// or does not include a request id.
    fn build_pre_action_approval_response(response_value: &Value) -> Option<Value> {
        let method = response_value.get("method")?.as_str()?;
        let request_id = response_value.get("id")?.clone();
        let decision = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                Self::pre_action_approval_decision()
            }
            "execCommandApproval" | "applyPatchApproval" => {
                Self::legacy_pre_action_approval_decision()
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

    /// Terminates one runtime process and waits for process exit.
    async fn shutdown_runtime(session: &mut CodexSessionRuntime) {
        app_server_transport::shutdown_child(&mut session.child).await;
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
        stream_tx: mpsc::UnboundedSender<CodexStreamEvent>,
    ) -> CodexAppServerFuture<Result<CodexTurnResponse, String>> {
        let sessions = Arc::clone(&self.sessions);

        Box::pin(
            async move { Self::run_turn_internal(sessions.as_ref(), request, &stream_tx).await },
        )
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
    stdin: tokio::process::ChildStdin,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
    thread_id: String,
}

impl CodexSessionRuntime {
    /// Returns whether the stored runtime configuration matches one request.
    fn matches_request(&self, request: &CodexTurnRequest) -> bool {
        self.folder == request.folder && self.model == request.model
    }
}

/// Extracts the turn id from a successful `turn/start` response payload.
fn extract_turn_id_from_turn_start_response(response_value: &Value) -> Option<String> {
    let result = response_value.get("result")?;

    result
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            result
                .get("turnId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            result
                .get("turn_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

/// Extracts the active turn id from one `turn/started` notification payload.
fn extract_turn_id_from_turn_started_notification(response_value: &Value) -> Option<String> {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/started") {
        return None;
    }

    response_value
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Extracts completed assistant message text from an `item/completed` line.
///
/// Synthetic completion status lines (for example `Command completed`) are
/// ignored so only real assistant messages are streamed to chat output.
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
        if crate::infra::agent::is_codex_completion_status_message(item_text) {
            return None;
        }

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

    let message = parts.join("\n\n");
    if crate::infra::agent::is_codex_completion_status_message(&message) {
        return None;
    }

    Some(message)
}

/// Parses `turn/completed` notifications and maps failures to user errors.
///
/// Completion notifications are only considered when `expected_turn_id` is
/// `Some`, ensuring that the worker ignores early or delegated completions
/// that arrive before the main thread id has been observed.
///
/// Completion payloads without a turn id are ignored even when the expected
/// turn id is known so that nested turns are never mistaken for the active
/// user turn.
fn parse_turn_completed(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<Result<(), String>> {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return None;
    }
    let expected_turn_id = expected_turn_id?;

    let turn_id = extract_turn_id_from_turn_completed_notification(response_value)?;
    if turn_id != expected_turn_id {
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

/// Extracts one turn id from a `turn/completed` notification payload.
///
/// Supports nested `params.turn.id` and legacy flat `params.turnId` /
/// `params.turn_id` shapes.
fn extract_turn_id_from_turn_completed_notification(response_value: &Value) -> Option<&str> {
    let params = response_value.get("params")?;

    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| params.get("turnId").and_then(Value::as_str))
        .or_else(|| params.get("turn_id").and_then(Value::as_str))
}

/// Extracts a progress description from an `item/started` notification.
///
/// The app-server sends `item/started` with a `params.item.type` field that
/// indicates what kind of work the agent is beginning (e.g., command execution,
/// reasoning). Item types may arrive in camelCase (`commandExecution`) or
/// `snake_case` (`command_execution`); both are normalized before mapping.
///
/// Returns `None` when the event is not `item/started` or when the item type
/// does not produce a user-visible progress message (e.g., `agentMessage`).
fn extract_item_started_progress(response_value: &Value) -> Option<String> {
    if response_value.get("method").and_then(Value::as_str) != Some("item/started") {
        return None;
    }

    let raw_item_type = response_value
        .get("params")?
        .get("item")?
        .get("type")?
        .as_str()?;

    let normalized_item_type = camel_to_snake(raw_item_type);

    crate::infra::agent::compact_codex_progress_message(&normalized_item_type)
}

/// Converts a camelCase string to `snake_case`.
fn camel_to_snake(input: &str) -> String {
    let mut result = String::with_capacity(input.len() + 4);

    for (index, character) in input.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                result.push('_');
            }
            result.push(character.to_ascii_lowercase());
        } else {
            result.push(character);
        }
    }

    result
}
/// Extracts input/output token usage from `turn.usage` payloads.
///
/// Returns `None` when no `turn.usage` object exists in the payload.
fn extract_turn_usage(response_value: &Value) -> Option<(u64, u64)> {
    let turn = response_value
        .get("params")
        .and_then(|params| params.get("turn"))?;

    let usage = turn.get("usage")?;

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

    Some((input_tokens, output_tokens))
}

/// Extracts usage for the active turn, ignoring known delegated-turn payloads.
///
/// When `expected_turn_id` is set and the payload declares a different
/// `turn.id`, this returns `None` so delegated sub-turn events do not affect
/// the active turn usage totals.
fn extract_turn_usage_for_turn(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<(u64, u64)> {
    if let Some(expected_turn_id) = expected_turn_id {
        let turn_id = response_value
            .get("params")
            .and_then(|params| params.get("turn"))
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str);
        if turn_id.is_some_and(|payload_turn_id| payload_turn_id != expected_turn_id) {
            return None;
        }
    }

    extract_turn_usage(response_value)
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
    fn extract_agent_message_ignores_completion_status_lines() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "agentMessage",
                    "text": "Command completed"
                }
            }
        });

        // Act
        let message = extract_agent_message(&response_value);

        // Assert
        assert_eq!(message, None);
    }

    #[test]
    fn parse_turn_completed_returns_error_for_failed_turn() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "failed",
                    "error": {"message": "boom"}
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

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
                    "id": "active-turn",
                    "status": "completed"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(turn_result, Some(Ok(())));
    }

    #[test]
    fn parse_turn_completed_ignores_other_turn_ids() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "delegate-turn",
                    "status": "completed"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(turn_result, None);
    }

    #[test]
    fn parse_turn_completed_accepts_matching_turn_id() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "completed"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(turn_result, Some(Ok(())));
    }

    #[test]
    fn parse_turn_completed_ignores_missing_turn_id_for_expected_turn_id() {
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
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(turn_result, None);
    }

    #[test]
    fn parse_turn_completed_accepts_matching_flat_turn_id_fields() {
        // Arrange
        let camel_case_response = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turnId": "active-turn",
                "turn": {
                    "status": "completed"
                }
            }
        });
        let snake_case_response = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn_id": "active-turn",
                "turn": {
                    "status": "completed"
                }
            }
        });

        // Act
        let camel_case_result = parse_turn_completed(&camel_case_response, Some("active-turn"));
        let snake_case_result = parse_turn_completed(&snake_case_response, Some("active-turn"));

        // Assert
        assert_eq!(camel_case_result, Some(Ok(())));
        assert_eq!(snake_case_result, Some(Ok(())));
    }

    #[test]
    fn parse_turn_completed_ignores_notifications_before_turn_id_is_known() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "completed"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, None);

        // Assert
        assert_eq!(turn_result, None);
    }

    #[test]
    fn extract_turn_id_from_turn_start_response_returns_turn_id() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "turn-start-123",
            "result": {
                "turn": {
                    "id": "turn-456"
                }
            }
        });

        // Act
        let turn_id = extract_turn_id_from_turn_start_response(&response_value);

        // Assert
        assert_eq!(turn_id.as_deref(), Some("turn-456"));
    }

    #[test]
    fn extract_turn_id_from_turn_start_response_supports_flat_fields() {
        // Arrange
        let camel_case_response = serde_json::json!({
            "id": "turn-start-123",
            "result": {
                "turnId": "turn-camel"
            }
        });
        let snake_case_response = serde_json::json!({
            "id": "turn-start-123",
            "result": {
                "turn_id": "turn-snake"
            }
        });

        // Act
        let camel_case_turn_id = extract_turn_id_from_turn_start_response(&camel_case_response);
        let snake_case_turn_id = extract_turn_id_from_turn_start_response(&snake_case_response);

        // Assert
        assert_eq!(camel_case_turn_id.as_deref(), Some("turn-camel"));
        assert_eq!(snake_case_turn_id.as_deref(), Some("turn-snake"));
    }

    #[test]
    fn extract_turn_id_from_turn_started_notification_returns_turn_id() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/started",
            "params": {
                "turn": {
                    "id": "turn-789"
                }
            }
        });

        // Act
        let turn_id = extract_turn_id_from_turn_started_notification(&response_value);

        // Assert
        assert_eq!(turn_id.as_deref(), Some("turn-789"));
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
        assert_eq!(usage, Some((7, 3)));
    }

    #[test]
    fn extract_turn_usage_returns_none_when_usage_is_missing() {
        // Arrange
        let response_value = serde_json::json!({
            "params": {
                "turn": {
                    "status": "completed"
                }
            }
        });

        // Act
        let usage = extract_turn_usage(&response_value);

        // Assert
        assert_eq!(usage, None);
    }

    #[test]
    fn extract_turn_usage_for_turn_ignores_other_turn_ids() {
        // Arrange
        let response_value = serde_json::json!({
            "params": {
                "turn": {
                    "id": "delegate-turn",
                    "usage": {
                        "inputTokens": 9,
                        "outputTokens": 4
                    }
                }
            }
        });

        // Act
        let usage = extract_turn_usage_for_turn(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(usage, None);
    }

    #[test]
    fn extract_turn_usage_for_turn_reads_matching_turn_id() {
        // Arrange
        let response_value = serde_json::json!({
            "params": {
                "turn": {
                    "id": "active-turn",
                    "usage": {
                        "inputTokens": 9,
                        "outputTokens": 4
                    }
                }
            }
        });

        // Act
        let usage = extract_turn_usage_for_turn(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(usage, Some((9, 4)));
    }

    #[test]
    fn resolve_turn_usage_prefers_completed_usage_over_stream_usage() {
        // Arrange
        let completed_turn_usage = Some((13, 8));
        let latest_stream_usage = Some((9, 4));

        // Act
        let usage =
            RealCodexAppServerClient::resolve_turn_usage(completed_turn_usage, latest_stream_usage);

        // Assert
        assert_eq!(usage, (13, 8));
    }

    #[test]
    fn resolve_turn_usage_falls_back_to_stream_usage_when_completed_usage_is_missing() {
        // Arrange
        let completed_turn_usage = None;
        let latest_stream_usage = Some((9, 4));

        // Act
        let usage =
            RealCodexAppServerClient::resolve_turn_usage(completed_turn_usage, latest_stream_usage);

        // Assert
        assert_eq!(usage, (9, 4));
    }

    #[test]
    fn approval_policy_maps_auto_edit_mode() {
        // Act
        let auto_edit_policy = RealCodexAppServerClient::approval_policy();

        // Assert
        assert_eq!(auto_edit_policy, "on-request");
    }

    #[test]
    fn thread_sandbox_mode_maps_auto_edit_mode() {
        // Act
        let auto_edit_sandbox = RealCodexAppServerClient::thread_sandbox_mode();

        // Assert
        assert_eq!(auto_edit_sandbox, "workspace-write");
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
        let response_value =
            RealCodexAppServerClient::build_pre_action_approval_response(&request_value);

        // Assert
        assert_eq!(
            response_value,
            Some(serde_json::json!({
                "id": "approval-1",
                "result": {
                    "decision": "accept"
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
        let response_value =
            RealCodexAppServerClient::build_pre_action_approval_response(&request_value);

        // Assert
        assert_eq!(
            response_value,
            Some(serde_json::json!({
                "id": "approval-2",
                "result": {
                    "decision": "approved"
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
        let decision =
            RealCodexAppServerClient::build_pre_action_approval_response(&response_value);

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

    #[test]
    fn extract_item_started_progress_returns_progress_for_command_execution() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/started",
            "params": {
                "item": {
                    "type": "command_execution"
                }
            }
        });

        // Act
        let progress = extract_item_started_progress(&response_value);

        // Assert
        assert_eq!(progress, Some("Running a command".to_string()));
    }

    #[test]
    fn extract_item_started_progress_normalizes_camel_case_item_type() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/started",
            "params": {
                "item": {
                    "type": "commandExecution"
                }
            }
        });

        // Act
        let progress = extract_item_started_progress(&response_value);

        // Assert
        assert_eq!(progress, Some("Running a command".to_string()));
    }

    #[test]
    fn extract_item_started_progress_returns_none_for_agent_message() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/started",
            "params": {
                "item": {
                    "type": "agent_message"
                }
            }
        });

        // Act
        let progress = extract_item_started_progress(&response_value);

        // Assert
        assert_eq!(progress, None);
    }

    #[test]
    fn extract_item_started_progress_returns_none_for_non_item_started_method() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "command_execution"
                }
            }
        });

        // Act
        let progress = extract_item_started_progress(&response_value);

        // Assert
        assert_eq!(progress, None);
    }

    #[test]
    fn extract_item_started_progress_returns_thinking_for_reasoning() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/started",
            "params": {
                "item": {
                    "type": "reasoning"
                }
            }
        });

        // Act
        let progress = extract_item_started_progress(&response_value);

        // Assert
        assert_eq!(progress, Some("Thinking".to_string()));
    }

    #[test]
    fn camel_to_snake_converts_camel_case_strings() {
        // Act / Assert
        assert_eq!(camel_to_snake("commandExecution"), "command_execution");
        assert_eq!(camel_to_snake("agentMessage"), "agent_message");
        assert_eq!(camel_to_snake("webSearch"), "web_search");
        assert_eq!(camel_to_snake("reasoning"), "reasoning");
        assert_eq!(camel_to_snake("already_snake"), "already_snake");
    }
}
