use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::sync::mpsc;

use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::permission::PermissionMode;
use crate::infra::agent;
use crate::infra::agent::protocol::{agent_response_output_schema, parse_agent_response_strict};
use crate::infra::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::app_server_transport::{
    self, extract_json_error_message, response_id_matches, write_json_line,
};
use crate::infra::channel::{TurnPrompt, TurnPromptContentPart};

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
    turn_network_access: bool,
    turn_sandbox_type: &'static str,
    web_search_mode: &'static str,
}

const AUTO_EDIT_POLICY: PermissionModePolicy = PermissionModePolicy {
    approval_policy: "on-request",
    legacy_pre_action_decision: "approved",
    pre_action_decision: "accept",
    thread_sandbox_mode: "workspace-write",
    turn_network_access: true,
    turn_sandbox_type: "workspaceWrite",
    web_search_mode: "live",
};

/// Proactive compaction threshold for Codex models with a 400k context window.
///
/// [`AgentModel::Gpt54`] and [`AgentModel::Gpt53Codex`] use this larger
/// threshold to keep enough room for the active turn while delaying compaction.
const AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT: u64 = 300_000;

/// Proactive compaction threshold for Codex Spark models with a 128k context
/// window.
const AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT: u64 = 120_000;

/// Production [`AppServerClient`] backed by `codex app-server` process
/// instances.
pub(crate) struct RealCodexAppServerClient {
    sessions: AppServerSessionRegistry<CodexSessionRuntime>,
}

impl RealCodexAppServerClient {
    /// Creates an empty app-server runtime registry.
    pub(crate) fn new() -> Self {
        Self {
            sessions: AppServerSessionRegistry::new("Codex"),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<CodexSessionRuntime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, AppServerError> {
        let stream_tx = stream_tx.clone();
        let reasoning_level = request.reasoning_level;

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: CodexSessionRuntime::matches_request,
                pid: |runtime| runtime.child.id(),
                provider_conversation_id: CodexSessionRuntime::provider_conversation_id,
                restored_context: CodexSessionRuntime::restored_context,
            },
            |request| {
                let request = request.clone();

                Box::pin(async move { Self::start_runtime(&request).await })
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

                Box::pin(async move {
                    Self::run_turn_with_runtime(runtime, prompt, reasoning_level, stream_tx).await
                })
            },
            |runtime| Box::pin(Self::shutdown_runtime(runtime)),
        )
        .await
    }

    /// Starts `codex app-server`, initializes it, and creates a thread.
    async fn start_runtime(
        request: &AppServerTurnRequest,
    ) -> Result<CodexSessionRuntime, AppServerError> {
        let request_kind = crate::infra::channel::AgentRequestKind::SessionStart;
        let command = agent::create_backend(AgentKind::Codex)
            .build_command(agent::BuildCommandRequest {
                attachments: &[],
                folder: request.folder.as_path(),
                prompt: "",
                request_kind: &request_kind,
                model: &request.model,
                reasoning_level: request.reasoning_level,
            })
            .map_err(|error| {
                AppServerError::Provider(format!(
                    "Failed to build `codex app-server` command: {error}"
                ))
            })?;

        Self::start_runtime_with_built_command(command, request).await
    }

    /// Starts one pre-built Codex app-server command and bootstraps the
    /// session runtime around its stdio streams.
    async fn start_runtime_with_built_command(
        command: std::process::Command,
        request: &AppServerTurnRequest,
    ) -> Result<CodexSessionRuntime, AppServerError> {
        let mut command = tokio::process::Command::from(command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|error| {
            AppServerError::Provider(format!("Failed to spawn `codex app-server`: {error}"))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            AppServerError::Provider("Codex app-server stdin is unavailable".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AppServerError::Provider("Codex app-server stdout is unavailable".to_string())
        })?;
        let mut session = CodexSessionRuntime {
            child,
            latest_input_tokens: 0,
            folder: request.folder.clone(),
            model: request.model.clone(),
            restored_context: false,
            stdin: Some(stdin),
            stdout_lines: BufReader::new(stdout).lines(),
            thread_id: String::new(),
        };

        Self::initialize_runtime(&mut session).await?;
        let (thread_id, restored_context) = Self::start_or_resume_thread(
            &mut session,
            request.provider_conversation_id.as_deref(),
            request.reasoning_level,
        )
        .await?;
        session.thread_id = thread_id;
        session.restored_context = restored_context;

        Ok(session)
    }

    /// Sends the initialize handshake for one app-server process.
    async fn initialize_runtime(session: &mut CodexSessionRuntime) -> Result<(), AppServerError> {
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
        write_json_line(Self::runtime_stdin(session)?, &initialize_payload).await?;
        app_server_transport::wait_for_response_line(&mut session.stdout_lines, &initialize_id)
            .await?;

        let runtime_initialized_payload = serde_json::json!({
            "method": "initialized",
            "params": {}
        });
        write_json_line(Self::runtime_stdin(session)?, &runtime_initialized_payload).await?;

        Ok(())
    }

    /// Restores a known thread when possible and otherwise starts a fresh
    /// thread for the runtime.
    ///
    /// Returns the active thread id plus a flag indicating whether provider
    /// context was restored.
    ///
    /// # Errors
    /// Returns an error when a new thread cannot be started.
    async fn start_or_resume_thread(
        session: &mut CodexSessionRuntime,
        provider_conversation_id: Option<&str>,
        reasoning_level: ReasoningLevel,
    ) -> Result<(String, bool), AppServerError> {
        if let Some(provider_conversation_id) = provider_conversation_id
            && let Ok(thread_id) =
                Self::resume_thread(session, provider_conversation_id, reasoning_level).await
        {
            return Ok((thread_id, true));
        }

        let thread_id = Self::start_thread(session, reasoning_level).await?;

        Ok((thread_id, false))
    }

    /// Starts one Codex thread and returns its identifier.
    async fn start_thread(
        session: &mut CodexSessionRuntime,
        reasoning_level: ReasoningLevel,
    ) -> Result<String, AppServerError> {
        let thread_start_id = format!("thread-start-{}", uuid::Uuid::new_v4());
        let thread_start_payload = Self::build_thread_start_payload(
            &session.folder,
            &session.model,
            reasoning_level,
            &thread_start_id,
        );

        write_json_line(Self::runtime_stdin(session)?, &thread_start_payload).await?;
        let response_line = app_server_transport::wait_for_response_line(
            &mut session.stdout_lines,
            &thread_start_id,
        )
        .await?;
        let response_value = serde_json::from_str::<Value>(&response_line).map_err(|error| {
            AppServerError::Provider(format!(
                "Failed to parse thread/start response JSON: {error}"
            ))
        })?;

        response_value
            .get("result")
            .and_then(|result| result.get("thread"))
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| {
                AppServerError::Provider(
                    "Codex app-server `thread/start` response does not include a thread id"
                        .to_string(),
                )
            })
    }

    /// Resumes one existing Codex thread and returns the active identifier.
    async fn resume_thread(
        session: &mut CodexSessionRuntime,
        thread_id: &str,
        reasoning_level: ReasoningLevel,
    ) -> Result<String, AppServerError> {
        let thread_resume_request_id = format!("thread-resume-{}", uuid::Uuid::new_v4());
        let thread_resume_payload = Self::build_thread_resume_payload(
            &thread_resume_request_id,
            thread_id,
            &session.model,
            reasoning_level,
        );

        write_json_line(Self::runtime_stdin(session)?, &thread_resume_payload).await?;
        let response_line = app_server_transport::wait_for_response_line(
            &mut session.stdout_lines,
            &thread_resume_request_id,
        )
        .await?;
        let response_value = serde_json::from_str::<Value>(&response_line).map_err(|error| {
            AppServerError::Provider(format!(
                "Failed to parse thread/resume response JSON: {error}"
            ))
        })?;

        response_value
            .get("result")
            .and_then(|result| result.get("thread"))
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| {
                AppServerError::Provider(
                    "Codex app-server `thread/resume` response does not include a thread id"
                        .to_string(),
                )
            })
    }

    /// Builds one `thread/start` request payload for a runtime folder.
    ///
    /// Root `AGENTS.md` content is intentionally not forwarded through
    /// app-server instruction fields. The payload only applies a minimal
    /// `config` override to enable `web_search`, preserving other runtime
    /// defaults (including configured MCP servers).
    fn build_thread_start_payload(
        folder: &Path,
        model: &str,
        reasoning_level: ReasoningLevel,
        thread_start_id: &str,
    ) -> Value {
        serde_json::json!({
            "method": "thread/start",
            "id": thread_start_id,
            "params": {
                "model": model,
                "cwd": folder.to_string_lossy(),
                "approvalPolicy": Self::approval_policy(),
                "sandbox": Self::thread_sandbox_mode(),
                "config": Self::thread_config(reasoning_level),
                "experimentalRawEvents": false,
                "persistExtendedHistory": false
            }
        })
    }

    /// Builds one `thread/resume` request payload.
    fn build_thread_resume_payload(
        thread_resume_request_id: &str,
        thread_id: &str,
        model: &str,
        reasoning_level: ReasoningLevel,
    ) -> Value {
        serde_json::json!({
            "method": "thread/resume",
            "id": thread_resume_request_id,
            "params": {
                "threadId": thread_id,
                "model": model,
                "approvalPolicy": Self::approval_policy(),
                "sandbox": Self::thread_sandbox_mode(),
                "config": Self::thread_config(reasoning_level),
                "experimentalRawEvents": false,
                "persistExtendedHistory": false
            }
        })
    }

    /// Sends one turn prompt and waits for terminal completion notification.
    ///
    /// Before executing the turn, proactive compaction is triggered when the
    /// model-specific cumulative input token threshold is reached. If the turn
    /// fails with a `ContextWindowExceeded` error, reactive compaction is
    /// attempted and the turn is retried once.
    ///
    /// Intermediate agent messages and progress updates are emitted through
    /// `stream_tx` as they arrive from the app-server event stream.
    async fn run_turn_with_runtime(
        session: &mut CodexSessionRuntime,
        prompt: impl Into<TurnPrompt>,
        reasoning_level: ReasoningLevel,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<(String, u64, u64), AppServerError> {
        let prompt = prompt.into();
        let auto_compact_threshold = Self::auto_compact_input_token_threshold(&session.model);

        if session.latest_input_tokens >= auto_compact_threshold {
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                "Compacting context".to_string(),
            ));
            Self::send_compact_request(session).await?;
        }

        let result =
            Self::execute_turn_event_loop(session, &prompt, reasoning_level, stream_tx.clone())
                .await;

        match result {
            Ok((message, input_tokens, output_tokens)) => {
                session.latest_input_tokens = input_tokens;

                Ok((message, input_tokens, output_tokens))
            }
            Err(ref error) if is_context_window_exceeded_error(&error.to_string()) => {
                // Fire-and-forget: receiver may be dropped during shutdown.
                let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                    "Compacting context".to_string(),
                ));
                Self::send_compact_request(session).await?;

                let (message, input_tokens, output_tokens) =
                    Self::execute_turn_event_loop(session, &prompt, reasoning_level, stream_tx)
                        .await?;
                session.latest_input_tokens = input_tokens;

                Ok((message, input_tokens, output_tokens))
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the proactive compaction threshold for one Codex model name.
    ///
    /// This parses through [`AgentModel`] via [`AgentKind::Codex`] so model
    /// mapping remains centralized in the domain enum instead of local string
    /// checks. It keeps larger-window Codex models from compacting too early
    /// while preserving the tighter threshold required by Spark models.
    fn auto_compact_input_token_threshold(model: &str) -> u64 {
        let is_400k_context_model = matches!(
            AgentKind::Codex.parse_model(model),
            Some(AgentModel::Gpt54 | AgentModel::Gpt53Codex)
        );
        if is_400k_context_model {
            return AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT;
        }

        AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT
    }

    /// Sends `thread/compact/start` and waits for compaction to complete.
    ///
    /// The request returns immediately, then progress is communicated via
    /// `turn/*` and `item/*` notifications. This method consumes events until
    /// a `turn/completed` notification is received, indicating compaction has
    /// finished. On success, the runtime's cumulative token counter is reset.
    async fn send_compact_request(session: &mut CodexSessionRuntime) -> Result<(), AppServerError> {
        Self::send_compact_request_with_timeout(session, app_server_transport::TURN_TIMEOUT).await
    }

    /// Sends `thread/compact/start` and waits for compaction to complete
    /// within one caller-provided timeout window.
    async fn send_compact_request_with_timeout(
        session: &mut CodexSessionRuntime,
        turn_timeout: Duration,
    ) -> Result<(), AppServerError> {
        let compact_id = format!("compact-{}", uuid::Uuid::new_v4());
        let compact_payload = serde_json::json!({
            "method": "thread/compact/start",
            "id": compact_id,
            "params": {
                "threadId": session.thread_id
            }
        });

        write_json_line(Self::runtime_stdin(session)?, &compact_payload).await?;
        app_server_transport::wait_for_response_line(&mut session.stdout_lines, &compact_id)
            .await?;

        tokio::time::timeout(turn_timeout, async {
            loop {
                let stdout_line = Self::read_stdout_line(session, " during compaction").await?;

                if stdout_line.trim().is_empty() {
                    continue;
                }

                let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                    continue;
                };

                if response_value.get("method").and_then(Value::as_str) == Some("turn/completed") {
                    let status = response_value
                        .get("params")
                        .and_then(|params| params.get("turn"))
                        .and_then(|turn| turn.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("");

                    if status == "completed" {
                        session.latest_input_tokens = 0;

                        return Ok(());
                    }

                    let error_message = extract_turn_completed_error_message(&response_value)
                        .unwrap_or_else(|| "Compaction failed".to_string());

                    return Err(AppServerError::Provider(format!(
                        "Codex context compaction failed: {error_message}"
                    )));
                }
            }
        })
        .await
        .map_err(|_| Self::compaction_timeout_error(turn_timeout))?
    }

    /// Sends one `turn/start` request and processes the event stream until
    /// `turn/completed` is received.
    ///
    /// This is the raw turn execution loop without compaction logic. Callers
    /// wrap it with proactive and reactive compaction in
    /// [`Self::run_turn_with_runtime`].
    ///
    /// `turn/completed` events that report `status: "interrupted"` without an
    /// error payload are treated as non-terminal handoffs so delegated/subagent
    /// flows can continue to the next active turn.
    ///
    /// Some delegated turn chains do not emit an intermediate `turn/started`
    /// notification. In that case, the loop adopts the delegated turn id from
    /// the next `turn/completed` notification so terminal completion is still
    /// observed.
    async fn execute_turn_event_loop(
        session: &mut CodexSessionRuntime,
        prompt: impl Into<TurnPrompt>,
        reasoning_level: ReasoningLevel,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<(String, u64, u64), AppServerError> {
        let prompt = prompt.into();
        Self::execute_turn_event_loop_with_timeout(
            session,
            &prompt,
            reasoning_level,
            stream_tx,
            app_server_transport::TURN_TIMEOUT,
        )
        .await
    }

    /// Sends one `turn/start` request and processes the event stream using a
    /// caller-provided timeout window.
    async fn execute_turn_event_loop_with_timeout(
        session: &mut CodexSessionRuntime,
        prompt: impl Into<TurnPrompt>,
        reasoning_level: ReasoningLevel,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
        turn_timeout: Duration,
    ) -> Result<(String, u64, u64), AppServerError> {
        let prompt = prompt.into();
        let turn_start_id = format!("turn-start-{}", uuid::Uuid::new_v4());
        let turn_start_payload = Self::build_turn_start_payload(
            &session.folder,
            &session.model,
            reasoning_level,
            &session.thread_id,
            &prompt,
            &turn_start_id,
        );
        write_json_line(Self::runtime_stdin(session)?, &turn_start_payload).await?;

        let mut assistant_messages = Vec::new();
        let mut active_turn_id: Option<String> = None;
        let mut active_phase: Option<String> = None;
        let mut waiting_for_handoff_turn_completion = false;
        let mut latest_stream_usage: Option<(u64, u64)> = None;
        let mut completed_turn_usage: Option<(u64, u64)> = None;
        tokio::time::timeout(turn_timeout, async {
            loop {
                let stdout_line =
                    Self::read_stdout_line(session, " before `turn/completed` was received")
                        .await?;

                if stdout_line.trim().is_empty() {
                    continue;
                }

                if let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) {
                    if response_id_matches(&response_value, &turn_start_id) {
                        if response_value.get("error").is_some() {
                            return Err(AppServerError::Provider(
                                extract_json_error_message(&response_value).unwrap_or_else(|| {
                                    "Codex app-server returned an error for `turn/start`"
                                        .to_string()
                                }),
                            ));
                        }
                        if active_turn_id.is_none() {
                            active_turn_id =
                                extract_turn_id_from_turn_start_response(&response_value);
                            if active_turn_id.is_some() {
                                waiting_for_handoff_turn_completion = false;
                            }
                        }
                        continue;
                    }

                    if let Some(approval_response) =
                        Self::build_pre_action_approval_response(&response_value)
                    {
                        write_json_line(Self::runtime_stdin(session)?, &approval_response).await?;

                        continue;
                    }

                    Self::update_active_turn_tracking_for_response(
                        &response_value,
                        &mut active_turn_id,
                        &mut waiting_for_handoff_turn_completion,
                    );

                    Self::stream_turn_content_from_response(
                        &response_value,
                        &stream_tx,
                        &mut assistant_messages,
                        &mut active_phase,
                    );

                    Self::update_turn_usage_from_response(
                        &response_value,
                        active_turn_id.as_deref(),
                        &mut completed_turn_usage,
                        &mut latest_stream_usage,
                    );

                    if is_interrupted_turn_completion_without_error(
                        &response_value,
                        active_turn_id.as_deref(),
                    ) {
                        active_turn_id = None;
                        waiting_for_handoff_turn_completion = true;

                        continue;
                    }

                    if let Some(turn_result) =
                        parse_turn_completed(&response_value, active_turn_id.as_deref())
                    {
                        let (input_tokens, output_tokens) =
                            Self::resolve_turn_usage(completed_turn_usage, latest_stream_usage);

                        return Self::finalize_turn_completion(
                            turn_result,
                            &assistant_messages,
                            &stream_tx,
                            input_tokens,
                            output_tokens,
                        );
                    }
                }
            }
        })
        .await
        .map_err(|_| Self::turn_completed_timeout_error(turn_timeout))?
    }

    /// Builds one `turn/start` request payload for the active thread.
    ///
    /// Includes `outputSchema` so Codex app-server enforces protocol-shaped
    /// responses for this turn.
    fn build_turn_start_payload(
        folder: &Path,
        model: &str,
        reasoning_level: ReasoningLevel,
        thread_id: &str,
        prompt: impl Into<TurnPrompt>,
        turn_start_id: &str,
    ) -> Value {
        let prompt = prompt.into();

        serde_json::json!({
            "method": "turn/start",
            "id": turn_start_id,
            "params": {
                "threadId": thread_id,
                "input": Self::build_turn_input_items(&prompt),
                "cwd": folder.to_string_lossy(),
                "approvalPolicy": Self::approval_policy(),
                "sandboxPolicy": Self::turn_sandbox_policy(),
                "model": model,
                "effort": reasoning_level.codex(),
                "summary": Value::Null,
                "personality": Value::Null,
                "outputSchema": agent_response_output_schema()
            }
        })
    }

    /// Builds ordered Codex `turn/start` input items from one structured
    /// prompt payload.
    fn build_turn_input_items(prompt: &TurnPrompt) -> Vec<Value> {
        if !prompt.has_attachments() {
            return vec![serde_json::json!({
                "type": "text",
                "text": prompt.text,
                "text_elements": []
            })];
        }

        let mut input_items = Vec::new();
        for content_part in prompt.content_parts() {
            match content_part {
                TurnPromptContentPart::Text(text) => {
                    if !text.is_empty() {
                        input_items.push(serde_json::json!({
                            "type": "text",
                            "text": text,
                            "text_elements": []
                        }));
                    }
                }
                TurnPromptContentPart::Attachment(attachment)
                | TurnPromptContentPart::OrphanAttachment(attachment) => {
                    input_items.push(Self::build_local_image_input_item(
                        &attachment.local_image_path,
                    ));
                }
            }
        }

        input_items
    }

    /// Builds one Codex `localImage` input item using a plain filesystem path
    /// string.
    fn build_local_image_input_item(local_image_path: &Path) -> Value {
        serde_json::json!({
            "type": "localImage",
            "path": local_image_path.to_string_lossy().into_owned()
        })
    }

    /// Builds a stable timeout error for compaction completion waits.
    fn compaction_timeout_error(turn_timeout: Duration) -> AppServerError {
        AppServerError::Provider(format!(
            "Timed out waiting for Codex app-server compaction to complete after {} seconds",
            turn_timeout.as_secs()
        ))
    }

    /// Builds a stable timeout error for turn completion waits.
    fn turn_completed_timeout_error(turn_timeout: Duration) -> AppServerError {
        AppServerError::Provider(format!(
            "Timed out waiting for Codex app-server `turn/completed` after {} seconds",
            turn_timeout.as_secs()
        ))
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

    /// Updates active turn tracking from one response notification.
    ///
    /// Active turn id is first sourced from `turn/started`. During delegated
    /// handoff waits, `turn/completed` can also provide the new active turn id
    /// when no `turn/started` notification is emitted.
    fn update_active_turn_tracking_for_response(
        response_value: &Value,
        active_turn_id: &mut Option<String>,
        waiting_for_handoff_turn_completion: &mut bool,
    ) {
        if active_turn_id.is_some() {
            return;
        }

        if let Some(turn_id) = extract_turn_id_from_turn_started_notification(response_value) {
            *active_turn_id = Some(turn_id);
            *waiting_for_handoff_turn_completion = false;

            return;
        }

        if let Some(turn_id) = extract_handoff_turn_id_from_completion(
            response_value,
            active_turn_id.as_deref(),
            *waiting_for_handoff_turn_completion,
        ) {
            *active_turn_id = Some(turn_id);
        }
    }

    /// Streams progress updates plus assistant delta/completed items from one
    /// response.
    ///
    /// Delta notifications from [`extract_agent_message_delta`] are forwarded
    /// as `is_delta: true` events. Completed assistant items are forwarded
    /// as `is_delta: false` events and accumulated for final turn assembly.
    ///
    /// When an assistant message includes an optional `phase` label, the phase
    /// is surfaced as a progress update when it changes from the previous
    /// value.
    fn stream_turn_content_from_response(
        response_value: &Value,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
        assistant_messages: &mut Vec<String>,
        active_phase: &mut Option<String>,
    ) {
        if let Some(progress) = extract_item_started_progress(response_value) {
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(progress));
        }

        if let Some(agent_message) = extract_agent_message_delta(response_value) {
            if let Some(phase) = agent_message.phase.as_deref() {
                Self::emit_phase_progress_update(stream_tx, active_phase, phase);
            }

            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                is_delta: true,
                message: agent_message.message,
                phase: agent_message.phase,
            });
        }

        if let Some(agent_message) = extract_agent_message(response_value) {
            if let Some(phase) = agent_message.phase.as_deref() {
                Self::emit_phase_progress_update(stream_tx, active_phase, phase);
            }

            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: agent_message.message.clone(),
                phase: agent_message.phase.clone(),
            });
            assistant_messages.push(agent_message.message);
        }
    }

    /// Emits a phase progress event when an assistant item reports a new phase.
    fn emit_phase_progress_update(
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
        active_phase: &mut Option<String>,
        phase: &str,
    ) {
        if active_phase.as_deref() == Some(phase) {
            return;
        }

        *active_phase = Some(phase.to_string());
        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(format!(
            "Phase: {phase}"
        )));
    }

    /// Finalizes one parsed `turn/completed` result into the normalized turn
    /// response tuple.
    ///
    /// Successful completions prefer the latest structured protocol payload
    /// when one was emitted, then fall back to the latest non-empty completed
    /// assistant message.
    ///
    /// Codex can emit multiple assistant items in one turn, including a plain
    /// text echo after an earlier JSON payload that still carries `summary`
    /// data. Preferring the newest valid protocol payload preserves summary
    /// persistence without replaying stale drafts when only plain text exists.
    ///
    /// Non-completed terminal statuses are surfaced as visible assistant
    /// output so the session never lands in `Review` silently.
    fn finalize_turn_completion(
        turn_result: Result<(), String>,
        assistant_messages: &[String],
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<(String, u64, u64), AppServerError> {
        match turn_result {
            Ok(()) => {
                let assistant_message = preferred_completed_assistant_message(assistant_messages);

                Ok((assistant_message, input_tokens, output_tokens))
            }
            Err(error) => {
                let streamed_error = format!("[Codex app-server] {error}");
                // Fire-and-forget: receiver may be dropped during shutdown.
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    is_delta: false,
                    message: streamed_error,
                    phase: None,
                });

                Err(AppServerError::Provider(error))
            }
        }
    }

    /// Updates usage trackers for one app-server response line.
    ///
    /// Usage is read from modern `thread/tokenUsage/updated` notifications
    /// first, then from legacy `turn.usage` payloads for backwards
    /// compatibility.
    ///
    /// Completion usage is still tracked separately when available so final
    /// usage selection can prefer `turn/completed` payload usage and only fall
    /// back to the latest non-completion usage update when needed.
    fn update_turn_usage_from_response(
        response_value: &Value,
        expected_turn_id: Option<&str>,
        completed_turn_usage: &mut Option<(u64, u64)>,
        latest_stream_usage: &mut Option<(u64, u64)>,
    ) {
        if let Some(turn_usage) =
            extract_thread_token_usage_for_turn(response_value, expected_turn_id)
        {
            *latest_stream_usage = Some(turn_usage);

            return;
        }

        if let Some(turn_usage) = extract_turn_usage_for_turn(response_value, expected_turn_id) {
            if response_value.get("method").and_then(Value::as_str) == Some("turn/completed") {
                *completed_turn_usage = Some(turn_usage);
            } else {
                *latest_stream_usage = Some(turn_usage);
            }
        }
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
        let policy = Self::permission_mode_policy(PermissionMode::default());
        let mut turn_sandbox_policy = serde_json::json!({
            "type": policy.turn_sandbox_type
        });

        if policy.turn_sandbox_type == "workspaceWrite"
            && let Some(policy_object) = turn_sandbox_policy.as_object_mut()
        {
            policy_object.insert(
                "networkAccess".to_string(),
                Value::Bool(policy.turn_network_access),
            );
        }

        turn_sandbox_policy
    }

    /// Returns per-thread config overrides for one permission mode.
    ///
    /// This keeps overrides minimal while enabling live `web_search` and
    /// applying the selected Codex reasoning effort.
    fn thread_config(reasoning_level: ReasoningLevel) -> Value {
        serde_json::json!({
            "web_search": Self::web_search_mode(),
            "model_reasoning_effort": reasoning_level.codex(),
        })
    }

    /// Returns the `web_search` mode for one permission mode.
    fn web_search_mode() -> &'static str {
        Self::permission_mode_policy(PermissionMode::default()).web_search_mode
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

    /// Returns a mutable stdin handle for one active Codex runtime.
    ///
    /// Runtime shutdown takes ownership of `stdin` so callers can signal EOF
    /// before waiting for child process exit. This accessor returns a clear
    /// error when writes are attempted after shutdown has already started.
    fn runtime_stdin(
        session: &mut CodexSessionRuntime,
    ) -> Result<&mut tokio::process::ChildStdin, AppServerError> {
        session.stdin.as_mut().ok_or_else(|| {
            AppServerError::Provider("Codex app-server stdin is unavailable".to_string())
        })
    }

    /// Reads the next non-empty stdout line from the app-server runtime.
    ///
    /// Returns a provider error when the read fails or the process terminates
    /// before producing output.
    async fn read_stdout_line(
        session: &mut CodexSessionRuntime,
        context: &str,
    ) -> Result<String, AppServerError> {
        session
            .stdout_lines
            .next_line()
            .await
            .map_err(|error| {
                AppServerError::Provider(format!(
                    "Failed reading Codex app-server stdout{context}: {error}"
                ))
            })?
            .ok_or_else(|| {
                AppServerError::Provider(format!("Codex app-server terminated{context}"))
            })
    }

    /// Terminates one runtime process and waits for process exit.
    async fn shutdown_runtime(session: &mut CodexSessionRuntime) {
        drop(session.stdin.take());
        app_server_transport::shutdown_child(&mut session.child).await;
    }
}

/// Returns the completed assistant message that should back the final turn
/// result.
///
/// Preference order is:
/// 1. Latest valid protocol payload that includes `summary`
/// 2. Latest valid protocol payload without `summary`
/// 3. Latest non-empty plain-text assistant message
fn preferred_completed_assistant_message(assistant_messages: &[String]) -> String {
    if let Some(protocol_with_summary) = assistant_messages.iter().rev().find_map(|message| {
        let trimmed_message = message.trim();
        if trimmed_message.is_empty() {
            return None;
        }

        let response = parse_agent_response_strict(trimmed_message).ok()?;
        response.summary.as_ref()?;

        Some(trimmed_message.to_string())
    }) {
        return protocol_with_summary;
    }

    if let Some(protocol_payload) = assistant_messages.iter().rev().find_map(|message| {
        let trimmed_message = message.trim();
        if trimmed_message.is_empty() {
            return None;
        }

        let response = parse_agent_response_strict(trimmed_message).ok()?;
        if response.answer.trim().is_empty()
            && response.questions.is_empty()
            && response.follow_up_task_items().is_empty()
            && response.summary.is_none()
        {
            return None;
        }

        Some(trimmed_message.to_string())
    }) {
        return protocol_payload;
    }

    assistant_messages
        .iter()
        .rev()
        .find_map(|message| {
            let trimmed_message = message.trim();
            if trimmed_message.is_empty() {
                return None;
            }

            Some(trimmed_message.to_string())
        })
        .unwrap_or_default()
}

impl Default for RealCodexAppServerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AppServerClient for RealCodexAppServerClient {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        let sessions = self.sessions.clone();

        Box::pin(async move { Self::run_turn_internal(&sessions, request, &stream_tx).await })
    }

    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()> {
        let sessions = self.sessions.clone();

        Box::pin(async move {
            let Ok(Some(mut session_runtime)) = sessions.take_session(&session_id) else {
                return;
            };

            RealCodexAppServerClient::shutdown_runtime(&mut session_runtime).await;
        })
    }
}

struct CodexSessionRuntime {
    child: tokio::process::Child,
    /// Most recent input token count reported by the app-server.
    ///
    /// The app-server reports `input_tokens` as the total context size (full
    /// conversation history plus new input), not incremental tokens. This
    /// value is compared against the model-specific proactive compaction
    /// threshold returned by
    /// [`RealCodexAppServerClient::auto_compact_input_token_threshold`]. It
    /// resets to zero after compaction or runtime restart.
    latest_input_tokens: u64,
    folder: PathBuf,
    model: String,
    restored_context: bool,
    stdin: Option<tokio::process::ChildStdin>,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
    thread_id: String,
}

impl CodexSessionRuntime {
    /// Returns whether the stored runtime configuration matches one request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.folder == request.folder && self.model == request.model
    }

    /// Returns whether the runtime was bootstrapped by resuming stored thread
    /// context.
    fn restored_context(&self) -> bool {
        self.restored_context
    }

    /// Returns the active provider-native thread identifier, or `None` when
    /// the runtime has not yet started a thread.
    fn provider_conversation_id(&self) -> Option<String> {
        if self.thread_id.is_empty() {
            None
        } else {
            Some(self.thread_id.clone())
        }
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
                .get("turn")
                .and_then(|turn| turn.get("turnId"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            result
                .get("turn")
                .and_then(|turn| turn.get("turn_id"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
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
///
/// Supports nested `params.turn.id` and flat `params.turnId` /
/// `params.turn_id` shapes.
fn extract_turn_id_from_turn_started_notification(response_value: &Value) -> Option<String> {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/started") {
        return None;
    }

    let params = response_value.get("params")?;

    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turnId"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turn_id"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            params
                .get("turnId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            params
                .get("turn_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

/// Extracted assistant message payload from one `item/completed` line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ExtractedAgentMessage {
    /// Message text extracted from Codex `agentMessage` content.
    message: String,
    /// Optional phase label emitted by Codex for the assistant item.
    phase: Option<String>,
}

/// Extracts incremental assistant message text from delta notifications.
///
/// Supports both legacy `item/updated` payloads and current v2 thought-delta
/// notifications:
/// - `item/plan/delta`
/// - `item/reasoning/textDelta`
/// - `item/reasoning/summaryTextDelta`
///
/// When a legacy `item/updated` delta includes a `phase` label, it is
/// preserved so callers can propagate phase transitions before the item is
/// complete.
fn extract_agent_message_delta(response_value: &Value) -> Option<ExtractedAgentMessage> {
    let method = response_value.get("method").and_then(Value::as_str)?;
    if matches!(
        method,
        "item/plan/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/text_delta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/summary_text_delta"
    ) {
        let delta = response_value
            .get("params")?
            .get("delta")
            .and_then(Value::as_str)?;
        if delta.trim().is_empty() {
            return None;
        }
        let phase = if method == "item/plan/delta" {
            Some("plan".to_string())
        } else {
            Some("thinking".to_string())
        };

        return Some(ExtractedAgentMessage {
            message: delta.to_string(),
            phase,
        });
    }

    if method != "item/updated" {
        return None;
    }

    let item = response_value.get("params")?.get("item")?;
    let phase = item
        .get("phase")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let item_type = item.get("type")?.as_str()?.to_ascii_lowercase();
    if item_type != "reasoning" && item_type != "thought" {
        return None;
    }

    let delta = item.get("delta").and_then(Value::as_str)?;

    if delta.trim().is_empty() {
        return None;
    }

    Some(ExtractedAgentMessage {
        message: delta.to_string(),
        phase: phase.or(Some("thinking".to_string())),
    })
}

/// Extracts completed assistant message text from an `item/completed` line.
///
/// Only completed assistant-message item types are treated as final assistant
/// output. Internal planning/reasoning items are intentionally ignored so
/// thought text does not leak into persisted turn responses.
///
/// Synthetic completion status lines (for example `Command completed`) are
/// ignored so only real assistant messages are streamed to chat output.
///
/// When available, the assistant item `phase` is preserved so callers can
/// propagate phase transitions without changing visible assistant text.
fn extract_agent_message(response_value: &Value) -> Option<ExtractedAgentMessage> {
    if response_value.get("method").and_then(Value::as_str) != Some("item/completed") {
        return None;
    }

    let item = response_value.get("params")?.get("item")?;
    let phase = item
        .get("phase")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let item_type = item.get("type")?.as_str()?.to_ascii_lowercase();
    if !is_completed_assistant_message_item_type(&item_type) {
        return None;
    }
    if is_codex_thought_phase(phase.as_deref()) {
        return None;
    }

    if let Some(item_text) = item.get("text").and_then(Value::as_str) {
        if agent::is_codex_completion_status_message(item_text) {
            return None;
        }

        return Some(ExtractedAgentMessage {
            message: item_text.to_string(),
            phase,
        });
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
    if agent::is_codex_completion_status_message(&message) {
        return None;
    }

    Some(ExtractedAgentMessage { message, phase })
}

/// Returns whether one `item/completed` type should be finalized as assistant
/// output.
///
/// Codex streams internal thoughts separately through reasoning/plan item
/// types, so this matcher intentionally accepts only assistant-message item
/// variants.
fn is_completed_assistant_message_item_type(item_type: &str) -> bool {
    matches!(
        item_type,
        "agentmessage" | "agent_message" | "assistantmessage" | "assistant_message"
    )
}

/// Returns whether one Codex assistant item `phase` denotes thought/planning
/// text instead of final assistant output.
///
/// Phase matching is case-insensitive to handle provider variations such as
/// `Thinking` and `PLAN`.
fn is_codex_thought_phase(phase: Option<&str>) -> bool {
    let Some(phase_value) = phase else {
        return false;
    };

    let normalized_phase = phase_value.trim();

    normalized_phase.eq_ignore_ascii_case("thinking")
        || normalized_phase.eq_ignore_ascii_case("plan")
        || normalized_phase.eq_ignore_ascii_case("reasoning")
        || normalized_phase.eq_ignore_ascii_case("thought")
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
///
/// A terminal status is only treated as success when it is exactly
/// `"completed"`. Any other terminal status (for example `"interrupted"` with
/// an error payload or `"unfinished"`) is mapped to an error so callers do not
/// mistake an unfinished turn for a completed response.
///
/// `status: "interrupted"` with no error payload is handled before this parser
/// by [`is_interrupted_turn_completion_without_error`] so delegated turn
/// handoffs can continue.
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

    match status {
        "completed" => Some(Ok(())),
        "failed" => Some(Err(extract_turn_completed_error_message(response_value)
            .unwrap_or_else(|| "Codex app-server turn failed".to_string()))),
        "" => Some(Err("Codex app-server `turn/completed` response is \
                        missing `turn.status`"
            .to_string())),
        other => Some(Err(extract_turn_completed_error_message(response_value)
            .unwrap_or_else(|| {
                format!("Codex app-server turn ended with non-completed status `{other}`")
            }))),
    }
}

/// Returns `true` when `turn/completed` indicates a delegated-turn handoff.
///
/// Codex can report `status: "interrupted"` with no turn error while handing
/// execution to another turn (for example, subagent/delegated flows). This is
/// not treated as terminal failure; callers should reset active turn tracking
/// and keep consuming events.
fn is_interrupted_turn_completion_without_error(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> bool {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return false;
    }

    let Some(expected_turn_id) = expected_turn_id else {
        return false;
    };

    let Some(turn_id) = extract_turn_id_from_turn_completed_notification(response_value) else {
        return false;
    };
    if turn_id != expected_turn_id {
        return false;
    }

    let status = response_value
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "interrupted" {
        return false;
    }

    extract_turn_completed_error_message(response_value).is_none()
}

/// Extracts a delegated turn id from `turn/completed` during handoff waits.
///
/// When a delegated flow emits `turn/completed` without a preceding
/// `turn/started`, callers can adopt this turn id and continue normal
/// completion parsing. The id is only extracted while waiting for a handoff
/// completion and when no active turn id is currently tracked.
fn extract_handoff_turn_id_from_completion(
    response_value: &Value,
    expected_turn_id: Option<&str>,
    waiting_for_handoff_turn_completion: bool,
) -> Option<String> {
    if expected_turn_id.is_some() || !waiting_for_handoff_turn_completion {
        return None;
    }

    if response_value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return None;
    }

    extract_turn_id_from_turn_completed_notification(response_value).map(ToString::to_string)
}

/// Extracts an optional turn-level error message from `turn/completed`.
///
/// When the error payload includes a `codexErrorInfo` discriminant (for example
/// `ContextWindowExceeded`), the discriminant is prefixed to the message so
/// downstream callers can detect structured error classes.
fn extract_turn_completed_error_message(response_value: &Value) -> Option<String> {
    let error = response_value
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("error"))?;
    let message = error.get("message").and_then(Value::as_str)?;
    let error_info = error
        .get("codexErrorInfo")
        .and_then(Value::as_str)
        .unwrap_or("");

    if error_info.is_empty() {
        Some(message.to_string())
    } else {
        Some(format!("[{error_info}] {message}"))
    }
}

/// Returns whether a turn error message indicates context window overflow.
///
/// Checks for the structured `codexErrorInfo` tag and common text patterns
/// that Codex app-server uses when the context window is exhausted.
fn is_context_window_exceeded_error(error_message: &str) -> bool {
    error_message.contains("ContextWindowExceeded")
        || error_message.contains("context_window_exceeded")
}

/// Extracts one turn id from a `turn/completed` notification payload.
///
/// Supports nested `params.turn.id`, `params.turn.turnId`,
/// `params.turn.turn_id`, and legacy flat `params.turnId` /
/// `params.turn_id` shapes.
fn extract_turn_id_from_turn_completed_notification(response_value: &Value) -> Option<&str> {
    let params = response_value.get("params")?;

    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turnId"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turn_id"))
                .and_then(Value::as_str)
        })
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

    agent::compact_codex_progress_message(&normalized_item_type)
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

/// Extracts per-turn usage from `thread/tokenUsage/updated` notifications.
///
/// Current Codex app-server v2 emits token usage on this thread-level
/// notification instead of embedding usage in `turn/completed`.
///
/// Returns `None` when the payload does not represent a token-usage update or
/// when it is for a different turn than `expected_turn_id`.
fn extract_thread_token_usage_for_turn(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<(u64, u64)> {
    let method = response_value.get("method").and_then(Value::as_str)?;
    if method != "thread/tokenUsage/updated" && method != "thread/token_usage/updated" {
        return None;
    }

    let params = response_value.get("params")?;
    if let Some(expected_turn_id) = expected_turn_id {
        let payload_turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .or_else(|| params.get("turn_id").and_then(Value::as_str));
        if payload_turn_id.is_some_and(|turn_id| turn_id != expected_turn_id) {
            return None;
        }
    }

    let token_usage = params
        .get("tokenUsage")
        .or_else(|| params.get("token_usage"))?;
    let breakdown = token_usage
        .get("last")
        .or_else(|| token_usage.get("last_token_usage"))
        .or_else(|| token_usage.get("total"))
        .or_else(|| token_usage.get("total_token_usage"))?;

    let input_tokens = breakdown
        .get("inputTokens")
        .and_then(Value::as_u64)
        .or_else(|| breakdown.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = breakdown
        .get("outputTokens")
        .and_then(Value::as_u64)
        .or_else(|| breakdown.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);

    Some((input_tokens, output_tokens))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::infra::channel::TurnPromptAttachment;

    /// Spawns a shell-backed fake Codex runtime that prints scripted stdout
    /// responses while keeping stdin open for request writes.
    fn spawn_scripted_runtime(script: &str) -> tokio::process::Child {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        command
            .spawn()
            .expect("failed to spawn scripted Codex runtime")
    }

    /// Writes one executable fake `codex` binary that runs the provided shell
    /// body when invoked by lifecycle tests.
    fn write_fake_codex_binary(script_body: &str) -> (TempDir, std::path::PathBuf) {
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let executable = temp_dir.path().join("fake-codex");
        let script = format!("#!/bin/sh\n{script_body}\n");
        fs::write(&executable, script).expect("failed to write fake codex binary");
        let mut permissions = fs::metadata(&executable)
            .expect("failed to stat fake codex binary")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)
            .expect("failed to mark fake codex binary executable");

        (temp_dir, executable)
    }

    /// Builds a minimal turn request used by runtime lifecycle tests.
    fn build_turn_request(
        folder: std::path::PathBuf,
        provider_conversation_id: Option<&str>,
    ) -> AppServerTurnRequest {
        AppServerTurnRequest {
            folder,
            live_session_output: None,
            model: AgentModel::Gpt53Codex.as_str().to_string(),
            prompt: "Implement the task".into(),
            request_kind: crate::infra::channel::AgentRequestKind::SessionStart,
            provider_conversation_id: provider_conversation_id.map(ToString::to_string),
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-123".to_string(),
        }
    }

    /// Builds a `CodexSessionRuntime` backed by a scripted shell child for
    /// protocol lifecycle tests.
    fn build_scripted_runtime_session(
        script: &str,
        thread_id: &str,
        latest_input_tokens: u64,
    ) -> (TempDir, CodexSessionRuntime) {
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let folder = temp_dir.path().to_path_buf();
        let mut child = spawn_scripted_runtime(script);
        let stdin = child
            .stdin
            .take()
            .expect("scripted runtime stdin should be piped");
        let stdout = child
            .stdout
            .take()
            .expect("scripted runtime stdout should be piped");

        (
            temp_dir,
            CodexSessionRuntime {
                child,
                latest_input_tokens,
                folder,
                model: AgentModel::Gpt53Codex.as_str().to_string(),
                restored_context: false,
                stdin: Some(stdin),
                stdout_lines: BufReader::new(stdout).lines(),
                thread_id: thread_id.to_string(),
            },
        )
    }

    #[test]
    fn turn_completed_timeout_error_includes_timeout_seconds() {
        // Arrange
        let timeout = Duration::from_secs(9_001);

        // Act
        let error = RealCodexAppServerClient::turn_completed_timeout_error(timeout);

        // Assert
        let error_message = error.to_string();
        assert!(error_message.contains("9001"));
        assert!(error_message.contains("turn/completed"));
    }

    #[test]
    fn compaction_timeout_error_includes_timeout_seconds() {
        // Arrange
        let timeout = Duration::from_mins(70);

        // Act
        let error = RealCodexAppServerClient::compaction_timeout_error(timeout);

        // Assert
        let error_message = error.to_string();
        assert!(error_message.contains("4200"));
        assert!(error_message.contains("compaction"));
    }

    #[test]
    fn auto_compact_input_token_threshold_uses_400k_limit_for_codex_models() {
        // Arrange
        let gpt_54_model = AgentModel::Gpt54.as_str();
        let gpt_53_codex_model = AgentModel::Gpt53Codex.as_str();

        // Act
        let gpt_54_threshold =
            RealCodexAppServerClient::auto_compact_input_token_threshold(gpt_54_model);
        let gpt_53_threshold =
            RealCodexAppServerClient::auto_compact_input_token_threshold(gpt_53_codex_model);

        // Assert
        assert_eq!(
            gpt_54_threshold,
            AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT
        );
        assert_eq!(
            gpt_53_threshold,
            AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT
        );
    }

    #[test]
    fn auto_compact_input_token_threshold_uses_128k_limit_for_codex_spark() {
        // Arrange
        let gpt_53_codex_spark_model = AgentModel::Gpt53CodexSpark.as_str();

        // Act
        let threshold =
            RealCodexAppServerClient::auto_compact_input_token_threshold(gpt_53_codex_spark_model);

        // Assert
        assert_eq!(threshold, AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT);
    }

    #[test]
    fn auto_compact_input_token_threshold_falls_back_to_128k_limit_for_unknown_models() {
        // Arrange
        let unknown_codex_model = "gpt-unknown-codex";

        // Act
        let threshold =
            RealCodexAppServerClient::auto_compact_input_token_threshold(unknown_codex_model);

        // Assert
        assert_eq!(threshold, AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT);
    }

    /// Verifies `thread/start` extracts the returned thread identifier from a
    /// matching response payload.
    #[tokio::test]
    async fn start_thread_returns_thread_id_from_matching_response() {
        // Arrange
        let script = r#"
read request
request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"thread":{"id":"thread-123"}}}\n' "$request_id"
cat >/dev/null
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(script, "", 0);

        // Act
        let thread_id =
            RealCodexAppServerClient::start_thread(&mut session, ReasoningLevel::default()).await;

        // Assert
        assert_eq!(
            thread_id.expect("start_thread should succeed"),
            "thread-123"
        );
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies `thread/resume` returns a descriptive error when the runtime
    /// omits the nested thread identifier.
    #[tokio::test]
    async fn resume_thread_returns_error_when_response_omits_thread_id() {
        // Arrange
        let script = r#"
read request
request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"thread":{}}}\n' "$request_id"
cat >/dev/null
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(script, "", 0);

        // Act
        let thread_id = RealCodexAppServerClient::resume_thread(
            &mut session,
            "thread-previous",
            ReasoningLevel::default(),
        )
        .await;

        // Assert
        let error = thread_id.expect_err("resume_thread should fail");
        assert_eq!(
            error.to_string(),
            "Codex app-server `thread/resume` response does not include a thread id"
        );
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies runtime startup keeps provider context when
    /// `thread/resume` succeeds.
    #[tokio::test]
    async fn start_runtime_with_command_marks_restored_context_after_resume() {
        // Arrange
        let script = r#"
read initialize_request
initialize_id=$(printf '%s\n' "$initialize_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{}}\n' "$initialize_id"
read initialized_notification
read resume_request
resume_id=$(printf '%s\n' "$resume_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"thread":{"id":"thread-resumed"}}}\n' "$resume_id"
cat >/dev/null
"#;
        let (temp_dir, executable) = write_fake_codex_binary(script);
        let request = build_turn_request(temp_dir.path().to_path_buf(), Some("thread-existing"));

        // Act
        let mut command = std::process::Command::new(&executable);
        command
            .arg("--model")
            .arg(&request.model)
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .current_dir(&request.folder);
        let mut session =
            RealCodexAppServerClient::start_runtime_with_built_command(command, &request)
                .await
                .expect("runtime should start");

        // Assert
        assert_eq!(session.thread_id, "thread-resumed");
        assert!(session.restored_context);
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies runtime startup falls back to `thread/start` when
    /// `thread/resume` cannot restore the prior provider thread.
    #[tokio::test]
    async fn start_runtime_with_command_falls_back_to_thread_start_after_resume_failure() {
        // Arrange
        let script = r#"
read initialize_request
initialize_id=$(printf '%s\n' "$initialize_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{}}\n' "$initialize_id"
read initialized_notification
read resume_request
resume_id=$(printf '%s\n' "$resume_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"thread":{}}}\n' "$resume_id"
read start_request
start_id=$(printf '%s\n' "$start_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"thread":{"id":"thread-started"}}}\n' "$start_id"
cat >/dev/null
"#;
        let (temp_dir, executable) = write_fake_codex_binary(script);
        let request = build_turn_request(temp_dir.path().to_path_buf(), Some("thread-existing"));

        // Act
        let mut command = std::process::Command::new(&executable);
        command
            .arg("--model")
            .arg(&request.model)
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .current_dir(&request.folder);
        let mut session =
            RealCodexAppServerClient::start_runtime_with_built_command(command, &request)
                .await
                .expect("runtime should start");

        // Assert
        assert_eq!(session.thread_id, "thread-started");
        assert!(!session.restored_context);
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies context compaction resets cumulative input tokens after the
    /// runtime confirms successful completion.
    #[tokio::test]
    async fn send_compact_request_resets_latest_input_tokens_on_success() {
        // Arrange
        let script = r#"
read request
request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{}}\n' "$request_id"
printf '{"method":"turn/completed","params":{"turn":{"status":"completed"}}}\n'
cat >/dev/null
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(script, "thread-1", 450_000);

        // Act
        let compact_result = RealCodexAppServerClient::send_compact_request(&mut session).await;

        // Assert
        compact_result.expect("compact request should succeed");
        assert_eq!(session.latest_input_tokens, 0);
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies context compaction surfaces failure details from
    /// `turn/completed` error payloads.
    #[tokio::test]
    async fn send_compact_request_returns_turn_completed_error_details() {
        // Arrange
        let script = r#"
read request
request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{}}\n' "$request_id"
printf '{"method":"turn/completed","params":{"turn":{"status":"failed","error":{"message":"compaction boom"}}}}\n'
cat >/dev/null
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(script, "thread-1", 450_000);

        // Act
        let compact_result = RealCodexAppServerClient::send_compact_request(&mut session).await;

        // Assert
        let error = compact_result.expect_err("compact request should fail");
        assert_eq!(
            error.to_string(),
            "Codex context compaction failed: compaction boom"
        );
        assert_eq!(session.latest_input_tokens, 450_000);
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies compaction waits fail with the configured timeout message
    /// when no `turn/completed` event ever arrives.
    #[tokio::test]
    async fn send_compact_request_with_timeout_reports_missing_completion() {
        // Arrange
        let script = r#"
read request
request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{}}\n' "$request_id"
sleep 5
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(script, "thread-1", 450_000);

        // Act
        let compact_result = RealCodexAppServerClient::send_compact_request_with_timeout(
            &mut session,
            Duration::from_millis(10),
        )
        .await;

        // Assert
        let error = compact_result.expect_err("compact request should time out");
        assert_eq!(
            error.to_string(),
            "Timed out waiting for Codex app-server compaction to complete after 0 seconds"
        );
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies proactive compaction runs before the turn event loop when the
    /// accumulated input token count already exceeds the model threshold.
    #[tokio::test]
    async fn run_turn_with_runtime_compacts_proactively_before_turn_start() {
        // Arrange
        let script = r#"
read compact_request
compact_id=$(printf '%s\n' "$compact_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{}}\n' "$compact_id"
printf '{"method":"turn/completed","params":{"turn":{"status":"completed"}}}\n'
read turn_request
turn_id=$(printf '%s\n' "$turn_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"turn":{"id":"turn-123"}}}\n' "$turn_id"
printf '{"method":"item/completed","params":{"item":{"type":"agentMessage","text":"Completed after compact"}}}\n'
printf '{"method":"turn/completed","params":{"turn":{"id":"turn-123","status":"completed","usage":{"inputTokens":12,"outputTokens":3}}}}\n'
cat >/dev/null
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(
            script,
            "thread-1",
            AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT,
        );
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        // Act
        let result = RealCodexAppServerClient::run_turn_with_runtime(
            &mut session,
            "Implement the task",
            ReasoningLevel::default(),
            stream_tx,
        )
        .await;

        // Assert
        let (message, input_tokens, output_tokens) =
            result.expect("run_turn_with_runtime should succeed");
        assert_eq!(message, "Completed after compact");
        assert_eq!(input_tokens, 12);
        assert_eq!(output_tokens, 3);
        assert_eq!(session.latest_input_tokens, 12);
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Compacting context".to_string()
            ))
        );
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies a `ContextWindowExceeded` turn failure triggers reactive
    /// compaction and retries the turn once.
    #[tokio::test]
    async fn run_turn_with_runtime_retries_after_context_window_exceeded() {
        // Arrange
        let script = r#"
read first_turn_request
first_turn_id=$(printf '%s\n' "$first_turn_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"turn":{"id":"turn-1"}}}\n' "$first_turn_id"
printf '{"method":"turn/completed","params":{"turn":{"id":"turn-1","status":"failed","error":{"message":"Token limit exceeded","codexErrorInfo":"ContextWindowExceeded"}}}}\n'
read compact_request
compact_id=$(printf '%s\n' "$compact_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{}}\n' "$compact_id"
printf '{"method":"turn/completed","params":{"turn":{"status":"completed"}}}\n'
read second_turn_request
second_turn_id=$(printf '%s\n' "$second_turn_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"turn":{"id":"turn-2"}}}\n' "$second_turn_id"
printf '{"method":"item/completed","params":{"item":{"type":"agentMessage","text":"Recovered response"}}}\n'
printf '{"method":"turn/completed","params":{"turn":{"id":"turn-2","status":"completed","usage":{"inputTokens":25,"outputTokens":5}}}}\n'
cat >/dev/null
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(script, "thread-1", 0);
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        // Act
        let result = RealCodexAppServerClient::run_turn_with_runtime(
            &mut session,
            "Implement the task",
            ReasoningLevel::default(),
            stream_tx,
        )
        .await;

        // Assert
        let (message, input_tokens, output_tokens) =
            result.expect("run_turn_with_runtime should succeed after retry");
        assert_eq!(message, "Recovered response");
        assert_eq!(input_tokens, 25);
        assert_eq!(output_tokens, 5);
        assert_eq!(session.latest_input_tokens, 25);
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: "[Codex app-server] [ContextWindowExceeded] Token limit exceeded"
                    .to_string(),
                phase: None,
            })
        );
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Compacting context".to_string()
            ))
        );
        app_server_transport::shutdown_child(&mut session.child).await;
    }

    /// Verifies turn execution reports the configured timeout when the runtime
    /// never emits a terminal `turn/completed` event.
    #[tokio::test]
    async fn execute_turn_event_loop_with_timeout_reports_missing_completion() {
        // Arrange
        let script = r#"
read turn_request
turn_id=$(printf '%s\n' "$turn_request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"id":"%s","result":{"turn":{"id":"turn-123"}}}\n' "$turn_id"
sleep 5
"#;
        let (_temp_dir, mut session) = build_scripted_runtime_session(script, "thread-1", 0);
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

        // Act
        let result = RealCodexAppServerClient::execute_turn_event_loop_with_timeout(
            &mut session,
            "Implement the task",
            ReasoningLevel::default(),
            stream_tx,
            Duration::from_millis(10),
        )
        .await;

        // Assert
        let error = result.expect_err("execute_turn_event_loop should time out");
        assert_eq!(
            error.to_string(),
            "Timed out waiting for Codex app-server `turn/completed` after 0 seconds"
        );
        app_server_transport::shutdown_child(&mut session.child).await;
    }

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
        assert_eq!(
            message,
            Some(ExtractedAgentMessage {
                message: "Line 1\n\nLine 2".to_string(),
                phase: None,
            })
        );
    }

    #[test]
    fn extract_agent_message_preserves_phase_for_agent_message_item() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "agentMessage",
                    "phase": "draft",
                    "text": "Draft response"
                }
            }
        });

        // Act
        let message = extract_agent_message(&response_value);

        // Assert
        assert_eq!(
            message,
            Some(ExtractedAgentMessage {
                message: "Draft response".to_string(),
                phase: Some("draft".to_string()),
            })
        );
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
    fn parse_turn_completed_returns_error_for_interrupted_turn() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "interrupted",
                    "error": {"message": "turn interrupted"}
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(turn_result, Some(Err("turn interrupted".to_string())));
    }

    #[test]
    fn interrupted_turn_completion_without_error_is_treated_as_handoff() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "interrupted"
                }
            }
        });

        // Act
        let is_handoff =
            is_interrupted_turn_completion_without_error(&response_value, Some("active-turn"));

        // Assert
        assert!(is_handoff);
    }

    #[test]
    fn interrupted_turn_completion_with_error_is_not_treated_as_handoff() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "interrupted",
                    "error": {"message": "turn interrupted"}
                }
            }
        });

        // Act
        let is_handoff =
            is_interrupted_turn_completion_without_error(&response_value, Some("active-turn"));

        // Assert
        assert!(!is_handoff);
    }

    #[test]
    fn extract_handoff_turn_id_from_completion_returns_turn_id_when_waiting() {
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
        let delegated_turn_id =
            extract_handoff_turn_id_from_completion(&response_value, None, true);

        // Assert
        assert_eq!(delegated_turn_id.as_deref(), Some("delegate-turn"));
    }

    #[test]
    fn extract_handoff_turn_id_from_completion_returns_none_when_not_waiting_or_active() {
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
        let turn_id_without_wait =
            extract_handoff_turn_id_from_completion(&response_value, None, false);
        let turn_id_with_active_turn =
            extract_handoff_turn_id_from_completion(&response_value, Some("active-turn"), true);

        // Assert
        assert_eq!(turn_id_without_wait, None);
        assert_eq!(turn_id_with_active_turn, None);
    }

    #[test]
    fn parse_turn_completed_returns_error_for_unfinished_turn_without_error_payload() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "unfinished"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(
            turn_result,
            Some(Err("Codex app-server turn ended with non-completed \
                      status `unfinished`"
                .to_string()))
        );
    }

    #[test]
    fn parse_turn_completed_returns_error_when_status_is_missing() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(
            turn_result,
            Some(Err("Codex app-server `turn/completed` response is \
                      missing `turn.status`"
                .to_string()))
        );
    }

    #[test]
    fn parse_turn_completed_returns_success_for_completed_turn() {
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
    fn finalize_turn_completion_returns_latest_non_empty_assistant_message_for_completed_turn() {
        // Arrange
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let turn_result = Ok(());
        let assistant_messages = vec![
            "First draft".to_string(),
            "   ".to_string(),
            "Final response".to_string(),
        ];

        // Act
        let result = RealCodexAppServerClient::finalize_turn_completion(
            turn_result,
            &assistant_messages,
            &stream_tx,
            7,
            3,
        );

        // Assert
        let (message, input_tokens, output_tokens) =
            result.expect("finalize_turn_completion should succeed");
        assert_eq!(message, "Final response");
        assert_eq!(input_tokens, 7);
        assert_eq!(output_tokens, 3);
        assert!(stream_rx.try_recv().is_err());
    }

    #[test]
    /// Verifies completed turns keep the latest protocol payload carrying
    /// `summary` instead of a later plain-text echo.
    fn finalize_turn_completion_prefers_protocol_payload_with_summary() {
        // Arrange
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let turn_result = Ok(());
        let protocol_payload = r#"{"answer":"Hi.","questions":[],"follow_up_tasks":[],"summary":{"turn":"- Replied to greeting.","session":"- Greeting response added on branch."}}"#;
        let assistant_messages = vec![protocol_payload.to_string(), "Hi.".to_string()];

        // Act
        let result = RealCodexAppServerClient::finalize_turn_completion(
            turn_result,
            &assistant_messages,
            &stream_tx,
            11,
            4,
        );

        // Assert
        let (message, input_tokens, output_tokens) =
            result.expect("finalize_turn_completion should succeed");
        assert_eq!(message, protocol_payload);
        assert_eq!(input_tokens, 11);
        assert_eq!(output_tokens, 4);
        assert!(stream_rx.try_recv().is_err());
    }

    #[test]
    /// Verifies completed turns keep the latest protocol payload even when it
    /// does not include `summary`.
    fn finalize_turn_completion_prefers_latest_protocol_payload() {
        // Arrange
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let turn_result = Ok(());
        let protocol_payload =
            r#"{"answer":"Structured answer","questions":[],"follow_up_tasks":[],"summary":null}"#;
        let assistant_messages = vec![
            "Draft".to_string(),
            protocol_payload.to_string(),
            "Structured answer".to_string(),
        ];

        // Act
        let result = RealCodexAppServerClient::finalize_turn_completion(
            turn_result,
            &assistant_messages,
            &stream_tx,
            5,
            2,
        );

        // Assert
        let (message, input_tokens, output_tokens) =
            result.expect("finalize_turn_completion should succeed");
        assert_eq!(message, protocol_payload);
        assert_eq!(input_tokens, 5);
        assert_eq!(output_tokens, 2);
        assert!(stream_rx.try_recv().is_err());
    }

    #[test]
    fn finalize_turn_completion_streams_error_message_for_non_completed_turn() {
        // Arrange
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let turn_result = Err("turn interrupted".to_string());

        // Act
        let result =
            RealCodexAppServerClient::finalize_turn_completion(turn_result, &[], &stream_tx, 0, 0);

        // Assert
        let error = result.expect_err("finalize_turn_completion should fail");
        assert_eq!(error.to_string(), "turn interrupted");
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: "[Codex app-server] turn interrupted".to_string(),
                phase: None,
            })
        );
    }

    #[test]
    fn stream_turn_content_from_response_emits_phase_progress_once_per_phase() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "agentMessage",
                    "phase": "final",
                    "text": "Final answer"
                }
            }
        });
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let mut assistant_messages = Vec::new();
        let mut active_phase = None;

        // Act
        RealCodexAppServerClient::stream_turn_content_from_response(
            &response_value,
            &stream_tx,
            &mut assistant_messages,
            &mut active_phase,
        );
        RealCodexAppServerClient::stream_turn_content_from_response(
            &response_value,
            &stream_tx,
            &mut assistant_messages,
            &mut active_phase,
        );

        // Assert
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Phase: final".to_string()
            ))
        );
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: "Final answer".to_string(),
                phase: Some("final".to_string()),
            })
        );
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: "Final answer".to_string(),
                phase: Some("final".to_string()),
            })
        );
        assert_eq!(stream_rx.try_recv().ok(), None);
    }

    #[test]
    fn stream_turn_content_from_response_ignores_agent_message_delta_notifications() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/agentMessage/delta",
            "params": {
                "delta": "{\"answer\":\"partial\"",
                "itemId": "item-1",
                "threadId": "thread-1",
                "turnId": "turn-1"
            }
        });
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let mut assistant_messages = Vec::new();
        let mut active_phase = None;

        // Act
        RealCodexAppServerClient::stream_turn_content_from_response(
            &response_value,
            &stream_tx,
            &mut assistant_messages,
            &mut active_phase,
        );

        // Assert
        assert!(assistant_messages.is_empty());
        assert!(stream_rx.try_recv().is_err());
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
    fn parse_turn_completed_accepts_matching_nested_flat_turn_id_fields() {
        // Arrange
        let camel_case_response = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "turnId": "active-turn",
                    "status": "completed"
                }
            }
        });
        let snake_case_response = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "turn_id": "active-turn",
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
    fn build_thread_start_payload_uses_thread_folder_as_cwd() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let thread_start_id = "thread-start-1";

        // Act
        let payload = RealCodexAppServerClient::build_thread_start_payload(
            temp_directory.path(),
            "gpt-5.3-codex",
            ReasoningLevel::High,
            thread_start_id,
        );
        let payload_cwd = payload
            .get("params")
            .and_then(|params| params.get("cwd"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        // Assert
        assert_eq!(
            payload_cwd,
            temp_directory.path().to_string_lossy().as_ref()
        );
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("model"))
                .and_then(Value::as_str),
            Some("gpt-5.3-codex")
        );
    }

    #[test]
    fn build_thread_resume_payload_sets_thread_id_and_model() {
        // Arrange
        let thread_resume_request_id = "thread-resume-1";
        let thread_id = "thread-123";

        // Act
        let payload = RealCodexAppServerClient::build_thread_resume_payload(
            thread_resume_request_id,
            thread_id,
            "gpt-5.3-codex",
            ReasoningLevel::High,
        );

        // Assert
        assert_eq!(
            payload.get("method").and_then(Value::as_str),
            Some("thread/resume")
        );
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("threadId"))
                .and_then(Value::as_str),
            Some(thread_id)
        );
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("model"))
                .and_then(Value::as_str),
            Some("gpt-5.3-codex")
        );
    }

    #[test]
    fn build_turn_start_payload_uses_thread_folder_as_cwd() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let prompt = "Implement task";
        let turn_start_id = "turn-start-1";
        let thread_id = "thread-1";

        // Act
        let payload = RealCodexAppServerClient::build_turn_start_payload(
            temp_directory.path(),
            "gpt-5.3-codex",
            ReasoningLevel::High,
            thread_id,
            prompt,
            turn_start_id,
        );
        let payload_cwd = payload
            .get("params")
            .and_then(|params| params.get("cwd"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        // Assert
        assert_eq!(
            payload_cwd,
            temp_directory.path().to_string_lossy().as_ref()
        );
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("model"))
                .and_then(Value::as_str),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("effort"))
                .and_then(Value::as_str),
            Some("high")
        );
    }

    #[test]
    /// Ensures `turn/start` includes a structured output schema object.
    fn build_turn_start_payload_sets_structured_output_schema() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");

        // Act
        let payload = RealCodexAppServerClient::build_turn_start_payload(
            temp_directory.path(),
            "gpt-5.3-codex",
            ReasoningLevel::Medium,
            "thread-1",
            "Implement task",
            "turn-start-1",
        );
        let output_schema = payload
            .get("params")
            .and_then(|params| params.get("outputSchema"))
            .and_then(Value::as_object)
            .expect("output schema should be an object");
        let output_schema_properties = output_schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("output schema should define properties");

        // Assert
        assert_eq!(
            output_schema.get("type").and_then(Value::as_str),
            Some("object")
        );
        assert!(output_schema_properties.contains_key("answer"));
        assert!(output_schema_properties.contains_key("questions"));
        assert!(output_schema_properties.contains_key("follow_up_tasks"));
        assert!(output_schema_properties.contains_key("summary"));
    }

    #[test]
    /// Verifies Codex input items interleave prompt text with local-image
    /// placeholders.
    fn build_turn_input_items_interleaves_text_and_local_images() {
        // Arrange
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-1.png"),
            }],
            text: "Review [Image #1] before merge".to_string(),
        };

        // Act
        let input_items = RealCodexAppServerClient::build_turn_input_items(&prompt);

        // Assert
        assert_eq!(input_items.len(), 3);
        assert_eq!(input_items[0]["type"], Value::String("text".to_string()));
        assert_eq!(input_items[0]["text"], Value::String("Review ".to_string()));
        assert_eq!(
            input_items[1]["type"],
            Value::String("localImage".to_string())
        );
        assert_eq!(
            input_items[1]["path"],
            Value::String("/tmp/image-1.png".to_string())
        );
        assert_eq!(input_items[2]["type"], Value::String("text".to_string()));
        assert_eq!(
            input_items[2]["text"],
            Value::String(" before merge".to_string())
        );
    }

    #[test]
    /// Verifies Codex input items follow placeholder order and keep orphaned
    /// images after the trailing text span.
    fn build_turn_input_items_orders_placeholder_images_and_appends_orphans() {
        // Arrange
        let prompt = TurnPrompt {
            attachments: vec![
                TurnPromptAttachment {
                    placeholder: "[Image #1]".to_string(),
                    local_image_path: PathBuf::from("/tmp/image-1.png"),
                },
                TurnPromptAttachment {
                    placeholder: "[Image #2]".to_string(),
                    local_image_path: PathBuf::from("/tmp/image-2.png"),
                },
                TurnPromptAttachment {
                    placeholder: "[Image #3]".to_string(),
                    local_image_path: PathBuf::from("/tmp/image-3.png"),
                },
            ],
            text: "Compare [Image #2] with [Image #1] now".to_string(),
        };

        // Act
        let input_items = RealCodexAppServerClient::build_turn_input_items(&prompt);

        // Assert
        assert_eq!(input_items.len(), 6);
        assert_eq!(
            input_items[0]["text"],
            Value::String("Compare ".to_string())
        );
        assert_eq!(
            input_items[1]["path"],
            Value::String("/tmp/image-2.png".to_string())
        );
        assert_eq!(input_items[2]["text"], Value::String(" with ".to_string()));
        assert_eq!(
            input_items[3]["path"],
            Value::String("/tmp/image-1.png".to_string())
        );
        assert_eq!(input_items[4]["text"], Value::String(" now".to_string()));
        assert_eq!(
            input_items[5]["path"],
            Value::String("/tmp/image-3.png".to_string())
        );
    }

    #[test]
    fn build_turn_start_payload_serializes_local_image_items_for_codex() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-1.png"),
            }],
            text: "Review [Image #1]".to_string(),
        };

        // Act
        let payload = RealCodexAppServerClient::build_turn_start_payload(
            temp_directory.path(),
            "gpt-5.3-codex",
            ReasoningLevel::Medium,
            "thread-1",
            prompt,
            "turn-start-1",
        );
        let input_items = payload
            .get("params")
            .and_then(|params| params.get("input"))
            .and_then(Value::as_array)
            .expect("input items should be an array");

        // Assert
        assert_eq!(input_items.len(), 2);
        assert_eq!(
            input_items[1]["type"],
            Value::String("localImage".to_string())
        );
        assert_eq!(
            input_items[1]["path"],
            Value::String("/tmp/image-1.png".to_string())
        );
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
    fn extract_turn_id_from_turn_start_response_supports_nested_flat_turn_fields() {
        // Arrange
        let camel_case_response = serde_json::json!({
            "id": "turn-start-123",
            "result": {
                "turn": {
                    "turnId": "turn-camel"
                }
            }
        });
        let snake_case_response = serde_json::json!({
            "id": "turn-start-123",
            "result": {
                "turn": {
                    "turn_id": "turn-snake"
                }
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
    fn extract_turn_id_from_turn_started_notification_supports_flat_fields() {
        // Arrange
        let camel_case_response = serde_json::json!({
            "method": "turn/started",
            "params": {
                "turnId": "turn-camel"
            }
        });
        let snake_case_response = serde_json::json!({
            "method": "turn/started",
            "params": {
                "turn_id": "turn-snake"
            }
        });

        // Act
        let camel_case_turn_id =
            extract_turn_id_from_turn_started_notification(&camel_case_response);
        let snake_case_turn_id =
            extract_turn_id_from_turn_started_notification(&snake_case_response);

        // Assert
        assert_eq!(camel_case_turn_id.as_deref(), Some("turn-camel"));
        assert_eq!(snake_case_turn_id.as_deref(), Some("turn-snake"));
    }

    #[test]
    fn extract_turn_id_from_turn_started_notification_supports_nested_flat_turn_fields() {
        // Arrange
        let camel_case_response = serde_json::json!({
            "method": "turn/started",
            "params": {
                "turn": {
                    "turnId": "turn-camel"
                }
            }
        });
        let snake_case_response = serde_json::json!({
            "method": "turn/started",
            "params": {
                "turn": {
                    "turn_id": "turn-snake"
                }
            }
        });

        // Act
        let camel_case_turn_id =
            extract_turn_id_from_turn_started_notification(&camel_case_response);
        let snake_case_turn_id =
            extract_turn_id_from_turn_started_notification(&snake_case_response);

        // Assert
        assert_eq!(camel_case_turn_id.as_deref(), Some("turn-camel"));
        assert_eq!(snake_case_turn_id.as_deref(), Some("turn-snake"));
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
    fn extract_thread_token_usage_for_turn_reads_last_usage_for_matching_turn_id() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "turnId": "active-turn",
                "tokenUsage": {
                    "last": {
                        "inputTokens": 33,
                        "outputTokens": 11
                    },
                    "total": {
                        "inputTokens": 100,
                        "outputTokens": 40
                    }
                }
            }
        });

        // Act
        let usage = extract_thread_token_usage_for_turn(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(usage, Some((33, 11)));
    }

    #[test]
    fn extract_thread_token_usage_for_turn_ignores_other_turn_ids() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "turnId": "delegate-turn",
                "tokenUsage": {
                    "last": {
                        "inputTokens": 33,
                        "outputTokens": 11
                    }
                }
            }
        });

        // Act
        let usage = extract_thread_token_usage_for_turn(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(usage, None);
    }

    #[test]
    fn update_turn_usage_from_response_prefers_thread_token_usage_updates() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "turnId": "active-turn",
                "tokenUsage": {
                    "last": {
                        "inputTokens": 21,
                        "outputTokens": 8
                    }
                }
            }
        });
        let mut completed_turn_usage = None;
        let mut latest_stream_usage = None;

        // Act
        RealCodexAppServerClient::update_turn_usage_from_response(
            &response_value,
            Some("active-turn"),
            &mut completed_turn_usage,
            &mut latest_stream_usage,
        );

        // Assert
        assert_eq!(completed_turn_usage, None);
        assert_eq!(latest_stream_usage, Some((21, 8)));
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
        // Arrange

        // Act
        let auto_edit_policy = RealCodexAppServerClient::approval_policy();

        // Assert
        assert_eq!(auto_edit_policy, "on-request");
    }

    #[test]
    fn thread_sandbox_mode_maps_auto_edit_mode() {
        // Arrange

        // Act
        let auto_edit_sandbox = RealCodexAppServerClient::thread_sandbox_mode();

        // Assert
        assert_eq!(auto_edit_sandbox, "workspace-write");
    }

    #[test]
    fn turn_sandbox_policy_enables_network_access_for_workspace_write() {
        // Arrange

        // Act
        let turn_sandbox_policy = RealCodexAppServerClient::turn_sandbox_policy();

        // Assert
        assert_eq!(
            turn_sandbox_policy.get("type").and_then(Value::as_str),
            Some("workspaceWrite")
        );
        assert_eq!(
            turn_sandbox_policy
                .get("networkAccess")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn web_search_mode_maps_auto_edit_mode() {
        // Arrange

        // Act
        let web_search_mode = RealCodexAppServerClient::web_search_mode();

        // Assert
        assert_eq!(web_search_mode, "live");
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
    fn turn_prompt_for_runtime_adds_repo_root_path_instructions_without_context_reset() {
        // Arrange
        let prompt = "Implement feature";
        let session_output = Some("prior context");

        // Act
        let turn_prompt = app_server::turn_prompt_for_runtime(
            prompt,
            &crate::infra::channel::AgentRequestKind::SessionStart,
            session_output,
            false,
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn turn_prompt_for_runtime_replays_session_output_after_context_reset_with_path_instructions() {
        // Arrange
        let prompt = "Implement feature";
        let session_output = Some("assistant: proposed plan");

        // Act
        let turn_prompt = app_server::turn_prompt_for_runtime(
            prompt,
            &crate::infra::channel::AgentRequestKind::SessionResume {
                session_output: session_output.map(ToString::to_string),
            },
            session_output,
            true,
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
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
        // Arrange, Act, Assert
        assert_eq!(camel_to_snake("commandExecution"), "command_execution");
        assert_eq!(camel_to_snake("agentMessage"), "agent_message");
        assert_eq!(camel_to_snake("webSearch"), "web_search");
        assert_eq!(camel_to_snake("reasoning"), "reasoning");
        assert_eq!(camel_to_snake("already_snake"), "already_snake");
    }

    #[test]
    fn build_thread_start_payload_does_not_set_root_instruction_fields() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let thread_start_id = "thread-start-1";

        // Act
        let payload = RealCodexAppServerClient::build_thread_start_payload(
            temp_directory.path(),
            "gpt-5.3-codex",
            ReasoningLevel::High,
            thread_start_id,
        );

        // Assert
        let params = payload.get("params").expect("params should exist");
        assert!(
            params.get("baseInstructions").is_none(),
            "baseInstructions should be omitted"
        );
        assert!(
            params.get("developerInstructions").is_none(),
            "developerInstructions should be omitted"
        );
    }

    #[test]
    fn build_thread_start_payload_sets_live_web_search_config_without_dynamic_tools() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let thread_start_id = "thread-start-1";

        // Act
        let payload = RealCodexAppServerClient::build_thread_start_payload(
            temp_directory.path(),
            "gpt-5.3-codex",
            ReasoningLevel::Low,
            thread_start_id,
        );
        let params = payload.get("params").unwrap_or(&Value::Null);

        // Assert
        assert_eq!(
            params
                .get("config")
                .and_then(|config| config.get("web_search"))
                .and_then(Value::as_str),
            Some("live")
        );
        assert_eq!(
            params
                .get("config")
                .and_then(|config| config.get("model_reasoning_effort"))
                .and_then(Value::as_str),
            Some("low")
        );
        assert!(params.get("dynamicTools").is_none());
    }

    #[test]
    fn is_context_window_exceeded_error_detects_structured_error() {
        // Arrange, Act, Assert
        assert!(is_context_window_exceeded_error(
            "[ContextWindowExceeded] Token limit exceeded"
        ));
        assert!(is_context_window_exceeded_error(
            "context_window_exceeded: too many tokens"
        ));
    }

    #[test]
    fn is_context_window_exceeded_error_returns_false_for_other_errors() {
        // Arrange, Act, Assert
        assert!(!is_context_window_exceeded_error("Connection reset"));
        assert!(!is_context_window_exceeded_error("Rate limit exceeded"));
        assert!(!is_context_window_exceeded_error(""));
    }

    #[test]
    fn extract_turn_completed_error_message_includes_codex_error_info() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "failed",
                    "error": {
                        "message": "Token limit exceeded",
                        "codexErrorInfo": "ContextWindowExceeded"
                    }
                }
            }
        });

        // Act
        let error_message = extract_turn_completed_error_message(&response_value);

        // Assert
        assert_eq!(
            error_message,
            Some("[ContextWindowExceeded] Token limit exceeded".to_string())
        );
    }

    #[test]
    fn extract_turn_completed_error_message_omits_absent_codex_error_info() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "active-turn",
                    "status": "failed",
                    "error": {
                        "message": "Something else went wrong"
                    }
                }
            }
        });

        // Act
        let error_message = extract_turn_completed_error_message(&response_value);

        // Assert
        assert_eq!(error_message, Some("Something else went wrong".to_string()));
    }

    #[test]
    fn extract_agent_message_delta_ignores_content_delta_for_agent_message_item() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/updated",
            "params": {
                "item": {
                    "type": "agentMessage",
                    "delta": "partial content"
                }
            }
        });

        // Act
        let message = extract_agent_message_delta(&response_value);

        // Assert
        assert_eq!(message, None);
    }

    #[test]
    fn extract_agent_message_delta_returns_content_delta_for_reasoning_item() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/updated",
            "params": {
                "item": {
                    "type": "reasoning",
                    "delta": "thinking..."
                }
            }
        });

        // Act
        let message = extract_agent_message_delta(&response_value);

        // Assert
        assert_eq!(
            message,
            Some(ExtractedAgentMessage {
                message: "thinking...".to_string(),
                phase: Some("thinking".to_string()),
            })
        );
    }

    #[test]
    fn extract_agent_message_delta_ignores_content_for_agent_message_delta_notification() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/agentMessage/delta",
            "params": {
                "delta": "streamed thought chunk",
                "itemId": "item-1",
                "threadId": "thread-1",
                "turnId": "turn-1"
            }
        });

        // Act
        let message = extract_agent_message_delta(&response_value);

        // Assert
        assert_eq!(message, None);
    }

    #[test]
    fn extract_agent_message_delta_returns_content_for_plan_delta_notification() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/plan/delta",
            "params": {
                "delta": "1. Inspect files",
                "itemId": "plan-1",
                "threadId": "thread-1",
                "turnId": "turn-1"
            }
        });

        // Act
        let message = extract_agent_message_delta(&response_value);

        // Assert
        assert_eq!(
            message,
            Some(ExtractedAgentMessage {
                message: "1. Inspect files".to_string(),
                phase: Some("plan".to_string()),
            })
        );
    }

    #[test]
    fn extract_agent_message_delta_returns_content_for_reasoning_text_delta_notification() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/reasoning/textDelta",
            "params": {
                "delta": "Tracing event stream",
                "itemId": "reasoning-1",
                "contentIndex": 0,
                "threadId": "thread-1",
                "turnId": "turn-1"
            }
        });

        // Act
        let message = extract_agent_message_delta(&response_value);

        // Assert
        assert_eq!(
            message,
            Some(ExtractedAgentMessage {
                message: "Tracing event stream".to_string(),
                phase: Some("thinking".to_string()),
            })
        );
    }

    #[test]
    fn extract_agent_message_ignores_reasoning_item() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "reasoning",
                    "text": "Thought complete"
                }
            }
        });

        // Act
        let message = extract_agent_message(&response_value);

        // Assert
        assert_eq!(message, None);
    }

    #[test]
    fn extract_agent_message_accepts_assistant_message_item_alias() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "assistant_message",
                    "text": "Final response"
                }
            }
        });

        // Act
        let message = extract_agent_message(&response_value);

        // Assert
        assert_eq!(
            message,
            Some(ExtractedAgentMessage {
                message: "Final response".to_string(),
                phase: None,
            })
        );
    }

    #[test]
    fn extract_agent_message_ignores_agent_message_with_thought_phase() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "agentMessage",
                    "phase": "Thinking",
                    "text": "I will inspect stream handling first."
                }
            }
        });

        // Act
        let message = extract_agent_message(&response_value);

        // Assert
        assert_eq!(message, None);
    }
}
