//! Gemini ACP-backed implementation of the shared app-server client.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use agent_client_protocol::{
    AGENT_METHOD_NAMES, CLIENT_METHOD_NAMES, ContentBlock, ImageContent, InitializeRequest,
    InitializeResponse, NewSessionRequest, NewSessionResponse, PermissionOption,
    PermissionOptionKind, PromptRequest, ProtocolVersion, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome, TextContent,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::sync::mpsc;

use crate::domain::agent::AgentKind;
use crate::infra::agent;
use crate::infra::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::app_server_transport::{
    self, extract_json_error_message, response_id_matches, write_json_line,
};
use crate::infra::channel::{TurnPrompt, TurnPromptAttachment, TurnPromptContentPart};

/// Boxed async result used by [`GeminiRuntimeTransport`] methods.
type GeminiTransportFuture<'scope, T> = Pin<Box<dyn Future<Output = T> + Send + 'scope>>;

/// Async ACP transport boundary for one running Gemini runtime.
///
/// Production uses [`GeminiStdioTransport`] backed by child process stdio,
/// while tests can inject `MockGeminiRuntimeTransport` to validate high-level
/// protocol workflows without spawning external commands.
#[cfg_attr(test, mockall::automock)]
trait GeminiRuntimeTransport {
    /// Writes one JSON-RPC payload to runtime stdin.
    fn write_json_line(&mut self, payload: Value) -> GeminiTransportFuture<'_, Result<(), String>>;

    /// Waits for one JSON-RPC response line matching `response_id`.
    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> GeminiTransportFuture<'_, Result<String, String>>;

    /// Reads the next raw stdout line from the runtime.
    fn next_stdout(&mut self) -> GeminiTransportFuture<'_, Result<Option<String>, String>>;
}

/// Production ACP transport backed by Gemini child process stdio streams.
struct GeminiStdioTransport {
    stdin: Option<tokio::process::ChildStdin>,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
}

impl GeminiStdioTransport {
    /// Creates a stdio transport over the provided child pipes.
    fn new(stdin: tokio::process::ChildStdin, stdout: tokio::process::ChildStdout) -> Self {
        Self {
            stdin: Some(stdin),
            stdout_lines: BufReader::new(stdout).lines(),
        }
    }
}

impl GeminiRuntimeTransport for GeminiStdioTransport {
    fn write_json_line(&mut self, payload: Value) -> GeminiTransportFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| "Gemini ACP stdin is unavailable".to_string())?;

            write_json_line(stdin, &payload).await
        })
    }

    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> GeminiTransportFuture<'_, Result<String, String>> {
        Box::pin(async move {
            app_server_transport::wait_for_response_line(&mut self.stdout_lines, &response_id).await
        })
    }

    fn next_stdout(&mut self) -> GeminiTransportFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move {
            self.stdout_lines
                .next_line()
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// Production [`AppServerClient`] backed by `gemini --experimental-acp`.
pub(crate) struct RealGeminiAcpClient {
    sessions: AppServerSessionRegistry<GeminiSessionRuntime>,
}

/// Normalized data extracted from one ACP `session/prompt` completion response.
struct PromptCompletion {
    assistant_message: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
}

impl RealGeminiAcpClient {
    /// Creates an empty ACP runtime registry for Gemini sessions.
    pub(crate) fn new() -> Self {
        Self {
            sessions: AppServerSessionRegistry::new("Gemini ACP"),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<GeminiSessionRuntime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, AppServerError> {
        let stream_tx = stream_tx.clone();

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: GeminiSessionRuntime::matches_request,
                pid: |runtime| runtime.child.id(),
                provider_conversation_id: GeminiSessionRuntime::provider_conversation_id,
                restored_context: GeminiSessionRuntime::restored_context,
            },
            |request| {
                let request = request.clone();

                Box::pin(async move {
                    Self::start_runtime(&request)
                        .await
                        .map_err(AppServerError::Provider)
                })
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

                Box::pin(async move {
                    Self::run_turn_with_runtime(
                        &mut runtime.transport,
                        &runtime.session_id,
                        prompt,
                        stream_tx,
                    )
                    .await
                    .map_err(AppServerError::Provider)
                })
            },
            |runtime| Box::pin(Self::shutdown_runtime(runtime)),
        )
        .await
    }

    /// Starts one Gemini ACP runtime, initializes it, and creates a session.
    ///
    /// If bootstrap fails after spawn, the child is shut down before returning
    /// the error to avoid leaking an orphaned runtime process.
    async fn start_runtime(request: &AppServerTurnRequest) -> Result<GeminiSessionRuntime, String> {
        let request_kind = crate::infra::channel::AgentRequestKind::SessionStart;
        let command = agent::create_backend(AgentKind::Gemini)
            .build_command(agent::BuildCommandRequest {
                attachments: &[],
                folder: request.folder.as_path(),
                prompt: "",
                request_kind: &request_kind,
                model: &request.model,
                reasoning_level: request.reasoning_level,
            })
            .map_err(|error| {
                format!("Failed to build `gemini --experimental-acp` command: {error}")
            })?;
        let mut command = tokio::process::Command::from(command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|error| format!("Failed to spawn `gemini --experimental-acp`: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Gemini ACP stdin is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Gemini ACP stdout is unavailable".to_string())?;
        let mut session = GeminiSessionRuntime {
            child,
            folder: request.folder.clone(),
            model: request.model.clone(),
            restored_context: false,
            session_id: String::new(),
            transport: GeminiStdioTransport::new(stdin, stdout),
        };

        let bootstrap_result =
            Self::bootstrap_runtime_session(&mut session.transport, session.folder.as_path()).await;
        session.session_id = match bootstrap_result {
            Ok(session_id) => session_id,
            Err(error) => {
                app_server_transport::shutdown_child(&mut session.child).await;

                return Err(error);
            }
        };

        Ok(session)
    }

    /// Completes ACP bootstrap by sending `initialize` and creating
    /// `session/new`.
    async fn bootstrap_runtime_session<Transport: GeminiRuntimeTransport>(
        transport: &mut Transport,
        folder: &Path,
    ) -> Result<String, String> {
        Self::initialize_runtime(transport).await?;

        Self::start_session(transport, folder).await
    }

    /// Sends the ACP initialize handshake.
    async fn initialize_runtime<Transport: GeminiRuntimeTransport>(
        transport: &mut Transport,
    ) -> Result<(), String> {
        let initialization_request_id = format!("init-{}", uuid::Uuid::new_v4());
        let initialization_request =
            Self::build_initialize_request_payload(&initialization_request_id)?;
        transport.write_json_line(initialization_request).await?;
        let initialize_response_line = transport
            .wait_for_response_line(initialization_request_id)
            .await?;
        let initialize_response = serde_json::from_str::<Value>(&initialize_response_line)
            .map_err(|error| format!("Failed to parse Gemini ACP initialize response: {error}"))?;
        if initialize_response.get("error").is_some() {
            return Err(extract_json_error_message(&initialize_response)
                .unwrap_or_else(|| "Gemini ACP returned an error for `initialize`".to_string()));
        }
        Self::parse_json_rpc_result::<InitializeResponse>(&initialize_response, "`initialize`")?;

        let initialized_notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized"
        });
        transport.write_json_line(initialized_notification).await?;

        Ok(())
    }

    /// Builds a typed ACP `initialize` request with conservative client
    /// capabilities.
    ///
    /// Gemini ACP runtime behavior expects unsupported client capabilities to
    /// be omitted entirely instead of advertised with explicit `false` values.
    /// To prevent unsupported `fs/*` and `terminal/*` calls from being sent to
    /// this client, this function overwrites `clientCapabilities` with an empty
    /// object after typed serialization.
    fn build_initialize_request_payload(request_id: &str) -> Result<Value, String> {
        let initialize_params = InitializeRequest::new(ProtocolVersion::LATEST);
        let mut initialize_payload = Self::build_json_rpc_request_payload(
            request_id,
            AGENT_METHOD_NAMES.initialize,
            initialize_params,
        )?;
        let Some(params) = initialize_payload.get_mut("params") else {
            return Err("Failed to build Gemini ACP `initialize` request params".to_string());
        };
        let Some(params) = params.as_object_mut() else {
            return Err(
                "Failed to build Gemini ACP `initialize` request params object".to_string(),
            );
        };
        params.insert(
            "clientCapabilities".to_string(),
            Value::Object(serde_json::Map::new()),
        );

        Ok(initialize_payload)
    }

    /// Builds a typed JSON-RPC request payload.
    fn build_json_rpc_request_payload<T: Serialize>(
        request_id: &str,
        method: &str,
        params: T,
    ) -> Result<Value, String> {
        let params_value = serde_json::to_value(params)
            .map_err(|error| format!("Failed to serialize `{method}` request params: {error}"))?;

        Ok(serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params_value
        }))
    }

    /// Extracts one typed JSON-RPC `result` payload.
    fn parse_json_rpc_result<T: serde::de::DeserializeOwned>(
        response_value: &Value,
        method: &str,
    ) -> Result<T, String> {
        let result_value = response_value
            .get("result")
            .cloned()
            .ok_or_else(|| format!("Gemini ACP `{method}` response missing `result`"))?;

        serde_json::from_value::<T>(result_value)
            .map_err(|error| format!("Failed to parse Gemini ACP `{method}` result: {error}"))
    }

    /// Creates one ACP session and returns the assigned `sessionId`.
    ///
    /// JSON-RPC `error` payloads are surfaced directly to keep diagnostics
    /// actionable when session creation fails.
    async fn start_session<Transport: GeminiRuntimeTransport>(
        transport: &mut Transport,
        folder: &Path,
    ) -> Result<String, String> {
        let session_new_id = format!("session-new-{}", uuid::Uuid::new_v4());
        let session_new_payload = Self::build_json_rpc_request_payload(
            &session_new_id,
            AGENT_METHOD_NAMES.session_new,
            NewSessionRequest::new(folder.to_path_buf()),
        )?;
        transport.write_json_line(session_new_payload).await?;
        let response_line = transport.wait_for_response_line(session_new_id).await?;
        let response_value = serde_json::from_str::<Value>(&response_line)
            .map_err(|error| format!("Failed to parse session/new response JSON: {error}"))?;

        Self::parse_session_new_response(&response_value)
    }

    /// Parses one ACP `session/new` response into a session identifier.
    ///
    /// Returns a JSON-RPC error message when present; otherwise extracts
    /// `result.sessionId`.
    fn parse_session_new_response(response_value: &Value) -> Result<String, String> {
        if response_value.get("error").is_some() {
            return Err(extract_json_error_message(response_value)
                .unwrap_or_else(|| "Gemini ACP returned an error for `session/new`".to_string()));
        }

        let session_new_result =
            Self::parse_json_rpc_result::<NewSessionResponse>(response_value, "`session/new`")
                .map_err(|error| {
                    if error.contains("missing field `sessionId`") {
                        return "Gemini ACP `session/new` response missing `sessionId`".to_string();
                    }

                    error
                })?;

        Ok(session_new_result.session_id.to_string())
    }

    /// Sends one prompt turn and waits for the matching prompt response id.
    ///
    /// Streaming progress updates are forwarded to the UI while assistant text
    /// chunks are streamed to the UI and accumulated for the final response.
    async fn run_turn_with_runtime<Transport: GeminiRuntimeTransport>(
        transport: &mut Transport,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<(String, u64, u64), String> {
        let prompt = prompt.into();
        let content_blocks = build_prompt_content_blocks(&prompt).await?;
        let prompt_id = format!("session-prompt-{}", uuid::Uuid::new_v4());
        let session_prompt_payload = Self::build_json_rpc_request_payload(
            &prompt_id,
            AGENT_METHOD_NAMES.session_prompt,
            PromptRequest::new(session_id.to_string(), content_blocks),
        )?;
        transport.write_json_line(session_prompt_payload).await?;

        let mut assistant_message = String::new();
        tokio::time::timeout(app_server_transport::TURN_TIMEOUT, async {
            loop {
                let stdout_line = transport
                    .next_stdout()
                    .await
                    .map_err(|error| format!("Failed reading Gemini ACP stdout: {error}"))?
                    .ok_or_else(|| {
                        "Gemini ACP terminated before prompt completion response".to_string()
                    })?;

                if stdout_line.trim().is_empty() {
                    continue;
                }

                let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                    continue;
                };

                if let Some(permission_response) =
                    build_permission_response(&response_value, session_id)
                {
                    transport.write_json_line(permission_response).await?;

                    continue;
                }

                if response_id_matches(&response_value, &prompt_id) {
                    if response_value.get("error").is_some() {
                        return Err(extract_json_error_message(&response_value).unwrap_or_else(
                            || "Gemini ACP returned an error for `session/prompt`".to_string(),
                        ));
                    }
                    let prompt_completion = parse_prompt_completion_response(&response_value)?;
                    assistant_message = select_preferred_assistant_message(
                        &assistant_message,
                        prompt_completion.assistant_message.as_deref(),
                    );

                    return Ok((
                        assistant_message,
                        prompt_completion.input_tokens,
                        prompt_completion.output_tokens,
                    ));
                }

                if let Some(progress) = extract_progress_update(&response_value, session_id) {
                    // Fire-and-forget: receiver may be dropped during shutdown.
                    let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(progress));
                }

                if let Some(chunk) = extract_assistant_message_chunk(&response_value, session_id) {
                    assistant_message.push_str(chunk.as_str());
                    Self::stream_assistant_chunk(&stream_tx, chunk);
                }
            }
        })
        .await
        .map_err(|_| {
            format!(
                "Timed out waiting for Gemini ACP prompt completion after {} seconds",
                app_server_transport::TURN_TIMEOUT.as_secs()
            )
        })?
    }

    /// Streams one non-empty assistant delta chunk to the UI.
    fn stream_assistant_chunk(
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
        chunk: String,
    ) {
        if chunk.is_empty() {
            return;
        }

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
            is_delta: true,
            message: chunk,
            phase: None,
        });
    }

    /// Terminates one Gemini ACP runtime process.
    async fn shutdown_runtime(session: &mut GeminiSessionRuntime) {
        drop(session.transport.stdin.take());
        app_server_transport::shutdown_child(&mut session.child).await;
    }
}

/// Builds Gemini ACP content blocks for one structured prompt payload.
///
/// Text spans are interleaved with inline image attachments in placeholder
/// order so Gemini receives the same multimodal sequence the composer shows.
async fn build_prompt_content_blocks(prompt: &TurnPrompt) -> Result<Vec<ContentBlock>, String> {
    let prompt = prompt.clone();

    tokio::task::spawn_blocking(move || build_prompt_content_blocks_blocking(&prompt))
        .await
        .map_err(|error| format!("Gemini prompt-image task failed: {error}"))?
}

/// Builds Gemini ACP content blocks for one prompt on a blocking worker
/// thread.
fn build_prompt_content_blocks_blocking(prompt: &TurnPrompt) -> Result<Vec<ContentBlock>, String> {
    if !prompt.has_attachments() {
        return Ok(vec![ContentBlock::Text(TextContent::new(
            prompt.text.clone(),
        ))]);
    }

    let mut content_blocks = Vec::new();
    for content_part in prompt.content_parts() {
        match content_part {
            TurnPromptContentPart::Text(text) => {
                push_text_content_block(&mut content_blocks, text);
            }
            TurnPromptContentPart::Attachment(attachment)
            | TurnPromptContentPart::OrphanAttachment(attachment) => {
                content_blocks.push(build_image_content_block(attachment)?);
            }
        }
    }

    Ok(content_blocks)
}

/// Appends one non-empty Gemini text content block.
fn push_text_content_block(content_blocks: &mut Vec<ContentBlock>, text: &str) {
    if text.is_empty() {
        return;
    }

    content_blocks.push(ContentBlock::Text(TextContent::new(text.to_string())));
}

/// Builds one Gemini ACP image content block from a persisted local prompt
/// attachment.
///
/// This helper performs blocking filesystem I/O with `std::fs::read`, so it
/// must only be called from the blocking worker used by
/// `build_prompt_content_blocks()`.
fn build_image_content_block(attachment: &TurnPromptAttachment) -> Result<ContentBlock, String> {
    let image_bytes = std::fs::read(&attachment.local_image_path).map_err(|error| {
        format!(
            "Failed to read Gemini prompt image `{}`: {error}",
            attachment.local_image_path.display()
        )
    })?;
    let mime_type = prompt_image_mime_type(&attachment.local_image_path);

    Ok(ContentBlock::Image(ImageContent::new(
        BASE64_STANDARD.encode(image_bytes),
        mime_type,
    )))
}

/// Returns the MIME type Gemini should use for one persisted prompt image.
#[must_use]
fn prompt_image_mime_type(local_image_path: &Path) -> &'static str {
    let Some(extension) = local_image_path
        .extension()
        .and_then(|extension| extension.to_str())
    else {
        return "image/png";
    };

    match extension.to_ascii_lowercase().as_str() {
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

impl Default for RealGeminiAcpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AppServerClient for RealGeminiAcpClient {
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

            Self::shutdown_runtime(&mut session_runtime).await;
        })
    }
}

struct GeminiSessionRuntime {
    child: tokio::process::Child,
    folder: PathBuf,
    model: String,
    restored_context: bool,
    session_id: String,
    transport: GeminiStdioTransport,
}

impl GeminiSessionRuntime {
    /// Returns whether the runtime matches one incoming turn request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.folder == request.folder && self.model == request.model
    }

    /// Returns whether runtime startup restored prior provider context.
    ///
    /// Gemini ACP currently starts a new `session/new` runtime session on each
    /// process bootstrap, so restarts rely on transcript replay.
    fn restored_context(&self) -> bool {
        self.restored_context
    }

    /// Returns the active provider-native Gemini ACP `sessionId`, or `None`
    /// when the runtime has not yet started a session.
    fn provider_conversation_id(&self) -> Option<String> {
        if self.session_id.is_empty() {
            None
        } else {
            Some(self.session_id.clone())
        }
    }
}

/// Builds a `session/request_permission` response for the active session.
///
/// The response follows ACP's `RequestPermissionResponse` shape. When an allow
/// option is available, this selects it to match auto-edit behavior. When no
/// options are provided or parsable, this returns a `cancelled` outcome to
/// avoid leaving the turn blocked indefinitely.
fn build_permission_response(response_value: &Value, expected_session_id: &str) -> Option<Value> {
    if response_value.get("method").and_then(Value::as_str)
        != Some(CLIENT_METHOD_NAMES.session_request_permission)
    {
        return None;
    }

    let params = response_value.get("params")?;
    let request_id = response_value.get("id")?.clone();
    if let Ok(permission_request) =
        serde_json::from_value::<RequestPermissionRequest>(params.clone())
    {
        if permission_request.session_id.to_string() != expected_session_id {
            return None;
        }

        let selected_option_id = select_permission_option(&permission_request.options)
            .map(|option| option.option_id.clone().to_string());

        return Some(build_permission_result_payload(
            &request_id,
            selected_option_id,
        ));
    }

    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    let selected_option_id = params
        .get("options")
        .and_then(select_permission_option_id_from_value);

    Some(build_permission_result_payload(
        &request_id,
        selected_option_id,
    ))
}

/// Builds a JSON-RPC `result` payload from a typed ACP permission decision.
fn build_permission_result_payload(
    request_id: &Value,
    selected_option_id: Option<String>,
) -> Value {
    let outcome =
        selected_option_id
            .as_ref()
            .map_or(RequestPermissionOutcome::Cancelled, |option_id| {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    option_id.clone(),
                ))
            });
    let permission_response = RequestPermissionResponse::new(outcome);
    let result_value = match serde_json::to_value(permission_response) {
        Ok(result_value) => result_value,
        Err(_) => build_permission_result_value_fallback(selected_option_id),
    };

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": result_value
    })
}

/// Builds a fallback ACP permission response result payload from raw values.
fn build_permission_result_value_fallback(selected_option_id: Option<String>) -> Value {
    if let Some(option_id) = selected_option_id {
        return serde_json::json!({
            "outcome": {
                "outcome": "selected",
                "optionId": option_id
            }
        });
    }

    serde_json::json!({
        "outcome": {
            "outcome": "cancelled"
        }
    })
}

/// Selects the preferred allow option from typed ACP permission choices.
///
/// Preference order is [`PermissionOptionKind::AllowAlways`], then
/// [`PermissionOptionKind::AllowOnce`], then the first listed option.
fn select_permission_option(options: &[PermissionOption]) -> Option<&PermissionOption> {
    for preferred_kind in [
        PermissionOptionKind::AllowAlways,
        PermissionOptionKind::AllowOnce,
    ] {
        if let Some(option) = options.iter().find(|option| option.kind == preferred_kind) {
            return Some(option);
        }
    }

    options.first()
}

/// Selects the preferred allow option identifier from raw ACP choices.
///
/// Preference order is `allow_always`, then `allow_once`, then the first
/// listed option when no allow-kind option is available.
fn select_permission_option_id_from_value(options: &Value) -> Option<String> {
    let options = options.as_array()?;
    for preferred_kind in ["allow_always", "allow_once"] {
        if let Some(option_id) = options.iter().find_map(|option| {
            if option.get("kind").and_then(Value::as_str) == Some(preferred_kind) {
                return option
                    .get("optionId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
            }

            None
        }) {
            return Some(option_id);
        }
    }

    options
        .first()
        .and_then(|option| option.get("optionId"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Selects the most reliable final assistant payload for one Gemini turn.
///
/// Gemini ACP can stream partial assistant chunks before it returns the final
/// `session/prompt` completion payload. When the streamed accumulation is not
/// valid protocol JSON but the completion payload is, prefer the completion
/// payload so strict protocol validation sees the fully structured response.
fn select_preferred_assistant_message(
    streamed_message: &str,
    completion_message: Option<&str>,
) -> String {
    let Some(completion_message) = completion_message.filter(|message| !message.trim().is_empty())
    else {
        return streamed_message.to_string();
    };

    if streamed_message.trim().is_empty() {
        return completion_message.to_string();
    }

    let streamed_is_protocol =
        agent::protocol::parse_agent_response_strict(streamed_message).is_ok();
    let completion_is_protocol =
        agent::protocol::parse_agent_response_strict(completion_message).is_ok();

    if completion_is_protocol && !streamed_is_protocol {
        return completion_message.to_string();
    }

    streamed_message.to_string()
}

/// Parses one completed `session/prompt` response into normalized turn fields.
fn parse_prompt_completion_response(response_value: &Value) -> Result<PromptCompletion, String> {
    let result = response_value
        .get("result")
        .ok_or_else(|| "Gemini ACP `session/prompt` response missing `result`".to_string())?;
    let (input_tokens, output_tokens) = extract_prompt_usage_tokens(result);
    let assistant_message = extract_prompt_result_text(result);

    Ok(PromptCompletion {
        assistant_message,
        input_tokens,
        output_tokens,
    })
}

/// Extracts prompt completion usage values from ACP result payloads.
fn extract_prompt_usage_tokens(result: &Value) -> (u64, u64) {
    let Some(usage) = result.get("usage") else {
        return (0, 0);
    };
    let input_tokens = usage
        .get("inputTokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("outputTokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    (input_tokens, output_tokens)
}

/// Extracts assistant text from known ACP prompt completion result shapes.
fn extract_prompt_result_text(result: &Value) -> Option<String> {
    if let Some(response_text) = result.get("response").and_then(Value::as_str) {
        return Some(response_text.to_string());
    }

    if let Some(message_text) = result.get("text").and_then(Value::as_str) {
        return Some(message_text.to_string());
    }

    if let Some(content) = result.get("content")
        && let Some(content_text) = extract_text_from_content_value(content)
        && !content_text.is_empty()
    {
        return Some(content_text);
    }

    if let Some(message) = result.get("message") {
        if let Some(message_text) = message.get("text").and_then(Value::as_str) {
            return Some(message_text.to_string());
        }

        if let Some(content) = message.get("content")
            && let Some(content_text) = extract_text_from_content_value(content)
            && !content_text.is_empty()
        {
            return Some(content_text);
        }
    }

    let output_items = result.get("output").and_then(Value::as_array)?;
    let mut output_text = String::new();
    for output_item in output_items {
        if let Some(item_text) = output_item.get("text").and_then(Value::as_str) {
            output_text.push_str(item_text);

            continue;
        }

        if let Some(content) = output_item.get("content")
            && let Some(content_text) = extract_text_from_content_value(content)
        {
            output_text.push_str(&content_text);
        }
    }
    if output_text.is_empty() {
        return None;
    }

    Some(output_text)
}

/// Extracts text from ACP content values represented as strings, arrays, or
/// nested objects, including `parts` and `content` fields.
fn extract_text_from_content_value(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let mut combined_text = String::new();
            for part in parts {
                if let Some(part_text) = extract_text_from_content_value(part) {
                    combined_text.push_str(&part_text);
                }
            }
            if combined_text.is_empty() {
                return None;
            }

            Some(combined_text)
        }
        Value::Object(_) => {
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }

            if let Some(parts_text) = content
                .get("parts")
                .and_then(extract_text_from_content_value)
                && !parts_text.is_empty()
            {
                return Some(parts_text);
            }

            if let Some(nested_content_text) = content
                .get("content")
                .and_then(extract_text_from_content_value)
                && !nested_content_text.is_empty()
            {
                return Some(nested_content_text);
            }

            None
        }
        _ => None,
    }
}

/// Extracts assistant text chunks from ACP `session/update` events.
fn extract_assistant_message_chunk(
    response_value: &Value,
    expected_session_id: &str,
) -> Option<String> {
    if extract_session_update_kind(response_value, expected_session_id)? != "agent_message_chunk" {
        return None;
    }

    let content = response_value
        .get("params")
        .and_then(|params| params.get("update"))
        .and_then(|update| update.get("content"))
        .and_then(extract_text_from_content_value)?;
    if content.trim().is_empty() {
        return None;
    }

    Some(content)
}

/// Extracts a short user-facing progress label from ACP `session/update`.
fn extract_progress_update(response_value: &Value, expected_session_id: &str) -> Option<String> {
    let session_update = extract_session_update_kind(response_value, expected_session_id)?;
    match session_update {
        "agent_thought_chunk" => Some("Thinking".to_string()),
        "tool_call" | "tool_call_update" => {
            let update = response_value.get("params")?.get("update")?;
            if let Some(status) = update.get("status").and_then(Value::as_str)
                && status.eq_ignore_ascii_case("completed")
            {
                return Some("Tool completed".to_string());
            }

            if let Some(title) = update.get("title").and_then(Value::as_str) {
                return Some(format!("Using tool: {title}"));
            }

            if let Some(kind) = update.get("kind").and_then(Value::as_str) {
                return Some(format!("Using tool: {kind}"));
            }

            Some("Using tool".to_string())
        }
        _ => None,
    }
}

/// Returns the ACP `sessionUpdate` kind for the matching session update.
fn extract_session_update_kind<'value>(
    response_value: &'value Value,
    expected_session_id: &str,
) -> Option<&'value str> {
    if response_value.get("method").and_then(Value::as_str)
        != Some(CLIENT_METHOD_NAMES.session_update)
    {
        return None;
    }

    let params = response_value.get("params")?;
    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    params
        .get("update")
        .and_then(|update| update.get("sessionUpdate"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use mockall::Sequence;
    use tempfile::tempdir;

    use super::*;

    /// Creates an isolated temporary worktree path for ACP tests.
    fn create_temp_folder() -> tempfile::TempDir {
        tempdir().expect("failed to create temporary test directory")
    }

    /// Configures the expected `session/prompt` request and stores its dynamic
    /// JSON-RPC `id` for a later completion response.
    fn expect_session_prompt_request(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
        prompt_id: Arc<Mutex<Option<String>>>,
    ) {
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(move |payload| {
                payload.get("method").and_then(Value::as_str) == Some("session/prompt")
                    && payload
                        .get("params")
                        .and_then(|params| params.get("sessionId"))
                        .and_then(Value::as_str)
                        == Some("session-1")
            })
            .returning(move |payload| {
                let id = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                if let Ok(mut guard) = prompt_id.lock() {
                    *guard = id;
                }

                Box::pin(async { Ok(()) })
            });
    }

    /// Configures one ACP permission request notification.
    fn expect_permission_request(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .returning(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": "permission-1",
                            "method": "session/request_permission",
                            "params": {
                                "sessionId": "session-1",
                                "toolCall": {
                                    "toolCallId": "tool-call-1",
                                    "title": "read_file"
                                },
                                "options": [{
                                    "optionId": "allow-once",
                                    "name": "Allow once",
                                    "kind": "allow_once"
                                }]
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    /// Configures the expected permission response write for `allow_once`.
    fn expect_permission_response_write(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
    ) {
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(|payload| {
                payload.get("id") == Some(&Value::String("permission-1".to_string()))
                    && payload
                        .get("result")
                        .and_then(|result| result.get("outcome"))
                        .and_then(|outcome| outcome.get("optionId"))
                        .and_then(Value::as_str)
                        == Some("allow-once")
            })
            .returning(|_| Box::pin(async { Ok(()) }));
    }

    /// Configures one `tool_call` progress update notification.
    fn expect_tool_call_update(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .returning(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": "session-1",
                                "update": {
                                    "sessionUpdate": "tool_call",
                                    "title": "read_file"
                                }
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    /// Configures one assistant chunk update notification.
    fn expect_assistant_chunk_update(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .returning(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": "session-1",
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {
                                        "type": "text",
                                        "text": "Chunk text"
                                    }
                                }
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    /// Configures one prompt completion response with usage counters.
    fn expect_prompt_completion(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
        prompt_id: Arc<Mutex<Option<String>>>,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .returning(move || {
                let response_id = prompt_id
                    .lock()
                    .ok()
                    .and_then(|guard| guard.clone())
                    .unwrap_or_else(|| "session-prompt-1".to_string());

                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "result": {
                                "usage": {
                                    "inputTokens": 2,
                                    "outputTokens": 3
                                }
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    /// Configures one prompt completion response that includes final assistant
    /// text in the `result.message.content` payload.
    fn expect_prompt_completion_with_message(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
        prompt_id: Arc<Mutex<Option<String>>>,
        assistant_message: String,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .returning(move || {
                let response_id = prompt_id
                    .lock()
                    .ok()
                    .and_then(|guard| guard.clone())
                    .unwrap_or_else(|| "session-prompt-1".to_string());
                let assistant_message = assistant_message.clone();

                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "result": {
                                "usage": {
                                    "inputTokens": 5,
                                    "outputTokens": 8
                                },
                                "message": {
                                    "content": [{
                                        "text": assistant_message
                                    }]
                                }
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    /// Configures one prompt completion error response using the captured
    /// `session/prompt` request id.
    fn expect_prompt_completion_error(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
        prompt_id: Arc<Mutex<Option<String>>>,
        error_message: &'static str,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .returning(move || {
                let response_id = prompt_id
                    .lock()
                    .ok()
                    .and_then(|guard| guard.clone())
                    .unwrap_or_else(|| "session-prompt-1".to_string());

                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "error": {
                                "code": -32000,
                                "message": error_message
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    /// Verifies progress and chunk events emitted for one streamed turn.
    fn assert_turn_stream_events(stream_rx: &mut mpsc::UnboundedReceiver<AppServerStreamEvent>) {
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Using tool: read_file".to_string()
            ))
        );
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::AssistantMessage {
                is_delta: true,
                message: "Chunk text".to_string(),
                phase: None,
            })
        );
    }

    #[tokio::test]
    async fn initialize_runtime_writes_initialize_then_initialized_notification() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| {
                let client_capabilities = payload
                    .get("params")
                    .and_then(|params| params.get("clientCapabilities"));

                payload.get("method").and_then(Value::as_str) == Some("initialize")
                    && client_capabilities == Some(&Value::Object(serde_json::Map::new()))
            })
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|response_id| response_id.starts_with("init-"))
            .returning(|_| {
                Box::pin(async {
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": "init-1",
                        "result": {
                            "protocolVersion": 1
                        }
                    })
                    .to_string())
                })
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialized"))
            .returning(|_| Box::pin(async { Ok(()) }));

        // Act
        let initialize_result = RealGeminiAcpClient::initialize_runtime(&mut transport).await;

        // Assert
        assert_eq!(initialize_result, Ok(()));
    }

    #[tokio::test]
    async fn initialize_runtime_returns_error_for_json_rpc_error_payload() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async {
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": "init-1",
                        "error": {
                            "code": -32000,
                            "message": "initialize failed"
                        }
                    })
                    .to_string())
                })
            });

        // Act
        let initialize_result = RealGeminiAcpClient::initialize_runtime(&mut transport).await;

        // Assert
        assert_eq!(initialize_result, Err("initialize failed".to_string()));
    }

    /// Verifies initialize handshake surfaces malformed JSON responses with a
    /// parse error.
    #[tokio::test]
    async fn initialize_runtime_returns_error_for_invalid_json_response() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok("not-json".to_string()) }));

        // Act
        let initialize_result = RealGeminiAcpClient::initialize_runtime(&mut transport).await;

        // Assert
        assert!(
            initialize_result
                .expect_err("invalid JSON should fail initialization")
                .contains("Failed to parse Gemini ACP initialize response"),
            "error should mention initialize JSON parsing"
        );
    }

    #[tokio::test]
    async fn start_session_writes_session_new_and_returns_session_id() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let temp_dir = create_temp_folder();
        let folder = temp_dir.path().to_path_buf();
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf({
                let folder = folder.clone();
                move |payload| {
                    payload.get("method").and_then(Value::as_str) == Some("session/new")
                        && payload
                            .get("params")
                            .and_then(|params| params.get("cwd"))
                            .and_then(Value::as_str)
                            .is_some_and(|cwd| cwd == folder.to_string_lossy())
                }
            })
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|response_id| response_id.starts_with("session-new-"))
            .returning(|_| {
                Box::pin(async {
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": "session-new-1",
                        "result": {
                            "sessionId": "session-123"
                        }
                    })
                    .to_string())
                })
            });

        // Act
        let session_id = RealGeminiAcpClient::start_session(&mut transport, &folder).await;

        // Assert
        assert_eq!(session_id, Ok("session-123".to_string()));
    }

    #[tokio::test]
    async fn bootstrap_runtime_session_initializes_then_creates_session() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let temp_dir = create_temp_folder();
        let folder = temp_dir.path().to_path_buf();
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialize"))
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|response_id| response_id.starts_with("init-"))
            .returning(|_| {
                Box::pin(async {
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": "init-1",
                        "result": {
                            "protocolVersion": 1
                        }
                    })
                    .to_string())
                })
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialized"))
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("session/new"))
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|response_id| response_id.starts_with("session-new-"))
            .returning(|_| {
                Box::pin(async {
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": "session-new-1",
                        "result": {
                            "sessionId": "session-123"
                        }
                    })
                    .to_string())
                })
            });

        // Act
        let session_id =
            RealGeminiAcpClient::bootstrap_runtime_session(&mut transport, folder.as_path()).await;

        // Assert
        assert_eq!(session_id, Ok("session-123".to_string()));
    }

    #[tokio::test]
    async fn bootstrap_runtime_session_returns_initialize_error_without_session_creation() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let temp_dir = create_temp_folder();
        let folder = temp_dir.path().to_path_buf();
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async {
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": "init-1",
                        "error": {
                            "code": -32000,
                            "message": "initialize failed"
                        }
                    })
                    .to_string())
                })
            });

        // Act
        let session_id =
            RealGeminiAcpClient::bootstrap_runtime_session(&mut transport, folder.as_path()).await;

        // Assert
        assert_eq!(session_id, Err("initialize failed".to_string()));
    }

    /// Verifies Gemini keeps streamed chunk text when the completion payload
    /// only supplies usage metadata.
    #[tokio::test]
    async fn run_turn_with_runtime_handles_permission_progress_chunk_and_completion() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let prompt_id = Arc::new(Mutex::new(None::<String>));
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        expect_session_prompt_request(&mut transport, &mut sequence, Arc::clone(&prompt_id));
        expect_permission_request(&mut transport, &mut sequence);
        expect_permission_response_write(&mut transport, &mut sequence);
        expect_tool_call_update(&mut transport, &mut sequence);
        expect_assistant_chunk_update(&mut transport, &mut sequence);
        expect_prompt_completion(&mut transport, &mut sequence, Arc::clone(&prompt_id));

        // Act
        let run_turn_result = RealGeminiAcpClient::run_turn_with_runtime(
            &mut transport,
            "session-1",
            "List files",
            stream_tx,
        )
        .await;

        // Assert
        assert_eq!(
            run_turn_result,
            Ok(("Chunk text".to_string(), 2_u64, 3_u64))
        );
        assert_turn_stream_events(&mut stream_rx);
    }

    /// Verifies Gemini prefers the final completion payload when it contains a
    /// full structured response and the streamed chunk text does not.
    #[tokio::test]
    async fn run_turn_with_runtime_prefers_structured_completion_payload() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let prompt_id = Arc::new(Mutex::new(None::<String>));
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let structured_response = serde_json::json!({
            "answer": "I have successfully added \"hello world\" to the top of your `README.md` file.",
            "questions": [],
            "summary": {
                "session": "Added \"hello world\" to `README.md`.",
                "turn": "Added \"hello world\" to `README.md` using string replacement."
            }
        })
        .to_string();

        expect_session_prompt_request(&mut transport, &mut sequence, Arc::clone(&prompt_id));
        expect_assistant_chunk_update(&mut transport, &mut sequence);
        expect_prompt_completion_with_message(
            &mut transport,
            &mut sequence,
            Arc::clone(&prompt_id),
            structured_response.clone(),
        );

        // Act
        let run_turn_result = RealGeminiAcpClient::run_turn_with_runtime(
            &mut transport,
            "session-1",
            "Prompt",
            stream_tx,
        )
        .await;

        // Assert
        assert_eq!(run_turn_result, Ok((structured_response, 5_u64, 8_u64)));
        let stream_event = stream_rx
            .try_recv()
            .expect("streamed chunk should still be forwarded");
        assert_eq!(
            stream_event,
            AppServerStreamEvent::AssistantMessage {
                is_delta: true,
                message: "Chunk text".to_string(),
                phase: None,
            }
        );
        assert!(
            stream_rx.try_recv().is_err(),
            "only the streamed assistant chunk should be emitted"
        );
    }

    /// Verifies prompt execution returns the JSON-RPC error message from the
    /// matching completion response.
    #[tokio::test]
    async fn run_turn_with_runtime_returns_prompt_error_message() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let prompt_id = Arc::new(Mutex::new(None));
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        expect_session_prompt_request(&mut transport, &mut sequence, Arc::clone(&prompt_id));
        expect_prompt_completion_error(&mut transport, &mut sequence, prompt_id, "prompt failed");

        // Act
        let turn_result = RealGeminiAcpClient::run_turn_with_runtime(
            &mut transport,
            "session-1",
            "Prompt",
            stream_tx,
        )
        .await;

        // Assert
        assert_eq!(turn_result, Err("prompt failed".to_string()));
        assert!(
            stream_rx.try_recv().is_err(),
            "error responses should not stream events"
        );
    }

    /// Verifies prompt execution falls back to the completion payload text
    /// when the runtime does not stream message chunks.
    #[tokio::test]
    async fn run_turn_with_runtime_uses_completion_message_without_chunks() {
        // Arrange
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let prompt_id = Arc::new(Mutex::new(None));
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        expect_session_prompt_request(&mut transport, &mut sequence, Arc::clone(&prompt_id));
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move || {
                let response_id = prompt_id
                    .lock()
                    .ok()
                    .and_then(|guard| guard.clone())
                    .unwrap_or_else(|| "session-prompt-1".to_string());

                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "result": {
                                "usage": {
                                    "inputTokens": 5,
                                    "outputTokens": 8
                                },
                                "message": {
                                    "content": [{
                                        "text": "Completion fallback"
                                    }]
                                }
                            }
                        })
                        .to_string(),
                    ))
                })
            });

        // Act
        let turn_result = RealGeminiAcpClient::run_turn_with_runtime(
            &mut transport,
            "session-1",
            "Prompt",
            stream_tx,
        )
        .await;

        // Assert
        assert_eq!(turn_result, Ok(("Completion fallback".to_string(), 5, 8)));
        assert!(
            stream_rx.try_recv().is_err(),
            "completion fallback should not emit delta chunks"
        );
    }

    #[test]
    fn parse_session_new_response_returns_session_id_for_success_payload() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "session-new-1",
            "result": {
                "sessionId": "session-1"
            }
        });

        // Act
        let session_id = RealGeminiAcpClient::parse_session_new_response(&response_value);

        // Assert
        assert_eq!(session_id, Ok("session-1".to_string()));
    }

    #[test]
    fn parse_session_new_response_returns_json_rpc_error_message() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "session-new-1",
            "error": {
                "code": -32000,
                "message": "Session creation failed"
            }
        });

        // Act
        let session_id = RealGeminiAcpClient::parse_session_new_response(&response_value);

        // Assert
        assert_eq!(session_id, Err("Session creation failed".to_string()));
    }

    #[test]
    fn parse_session_new_response_returns_error_when_session_id_is_missing() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "session-new-1",
            "result": {}
        });

        // Act
        let session_id = RealGeminiAcpClient::parse_session_new_response(&response_value);

        // Assert
        assert_eq!(
            session_id,
            Err("Gemini ACP `session/new` response missing `sessionId`".to_string())
        );
    }

    #[test]
    fn stream_assistant_chunk_sends_assistant_message_event_for_non_empty_chunks() {
        // Arrange
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        // Act
        RealGeminiAcpClient::stream_assistant_chunk(&stream_tx, "Hello from Gemini".to_string());

        // Assert
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::AssistantMessage {
                is_delta: true,
                message: "Hello from Gemini".to_string(),
                phase: None,
            })
        );
    }

    #[test]
    fn stream_assistant_chunk_ignores_empty_chunks() {
        // Arrange
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        // Act
        RealGeminiAcpClient::stream_assistant_chunk(&stream_tx, String::new());

        // Assert
        assert!(stream_rx.try_recv().is_err());
    }

    #[test]
    fn extract_session_update_kind_reads_matching_update_kind() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk"
                }
            }
        });

        // Act
        let session_update_kind = extract_session_update_kind(&response_value, "session-1");

        // Assert
        assert_eq!(session_update_kind, Some("agent_message_chunk"));
    }

    #[test]
    fn extract_session_update_kind_ignores_mismatched_session() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-2",
                "update": {
                    "sessionUpdate": "agent_message_chunk"
                }
            }
        });

        // Act
        let session_update_kind = extract_session_update_kind(&response_value, "session-1");

        // Assert
        assert_eq!(session_update_kind, None);
    }

    #[test]
    fn extract_assistant_message_chunk_returns_text_for_message_chunk() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": "Hello from ACP"
                    }
                }
            }
        });

        // Act
        let message_chunk = extract_assistant_message_chunk(&response_value, "session-1");

        // Assert
        assert_eq!(message_chunk, Some("Hello from ACP".to_string()));
    }

    #[test]
    fn extract_assistant_message_chunk_reads_text_from_nested_parts() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "parts": [
                            {"type": "text", "text": "Hello"},
                            {"type": "text", "text": " nested"}
                        ]
                    }
                }
            }
        });

        // Act
        let message_chunk = extract_assistant_message_chunk(&response_value, "session-1");

        // Assert
        assert_eq!(message_chunk, Some("Hello nested".to_string()));
    }

    #[test]
    fn parse_prompt_completion_response_reads_output_content_parts() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "session-prompt-1",
            "result": {
                "output": [{
                    "content": {
                        "parts": [
                            {"type": "text", "text": "Final"},
                            {"type": "text", "text": " message"}
                        ]
                    }
                }]
            }
        });

        // Act
        let prompt_completion = parse_prompt_completion_response(&response_value);

        // Assert
        assert!(prompt_completion.is_ok());
        let prompt_completion = match prompt_completion {
            Ok(prompt_completion) => prompt_completion,
            Err(_) => PromptCompletion {
                assistant_message: None,
                input_tokens: 0,
                output_tokens: 0,
            },
        };
        assert_eq!(
            prompt_completion.assistant_message,
            Some("Final message".to_string())
        );
    }

    #[test]
    fn extract_progress_update_returns_thinking_for_thought_chunks() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {
                        "type": "text",
                        "text": "internal thought"
                    }
                }
            }
        });

        // Act
        let progress_update = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(progress_update, Some("Thinking".to_string()));
    }

    #[test]
    fn extract_progress_update_returns_tool_title_for_in_progress_tool_call() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "status": "in_progress",
                    "title": "Cargo.toml"
                }
            }
        });

        // Act
        let progress_update = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(progress_update, Some("Using tool: Cargo.toml".to_string()));
    }

    #[test]
    fn extract_progress_update_returns_tool_completed_for_completed_tool_call() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "status": "completed"
                }
            }
        });

        // Act
        let progress_update = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(progress_update, Some("Tool completed".to_string()));
    }

    #[test]
    fn parse_prompt_completion_response_returns_usage_and_assistant_message() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "session-prompt-1",
            "result": {
                "usage": {
                    "inputTokens": 7,
                    "outputTokens": 11
                },
                "message": {
                    "content": [
                        {"type": "text", "text": "Hello"},
                        {"type": "text", "text": " ACP"}
                    ]
                }
            }
        });

        // Act
        let prompt_completion = parse_prompt_completion_response(&response_value);

        // Assert
        assert!(prompt_completion.is_ok());
        let prompt_completion = match prompt_completion {
            Ok(prompt_completion) => prompt_completion,
            Err(_) => PromptCompletion {
                assistant_message: None,
                input_tokens: 0,
                output_tokens: 0,
            },
        };
        assert_eq!(
            prompt_completion.assistant_message,
            Some("Hello ACP".to_string())
        );
        assert_eq!(prompt_completion.input_tokens, 7);
        assert_eq!(prompt_completion.output_tokens, 11);
    }

    #[test]
    fn parse_prompt_completion_response_reads_snake_case_usage_fields() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "session-prompt-1",
            "result": {
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 5
                },
                "text": "Done"
            }
        });

        // Act
        let prompt_completion = parse_prompt_completion_response(&response_value);

        // Assert
        assert!(prompt_completion.is_ok());
        let prompt_completion = match prompt_completion {
            Ok(prompt_completion) => prompt_completion,
            Err(_) => PromptCompletion {
                assistant_message: None,
                input_tokens: 0,
                output_tokens: 0,
            },
        };
        assert_eq!(
            prompt_completion.assistant_message,
            Some("Done".to_string())
        );
        assert_eq!(prompt_completion.input_tokens, 3);
        assert_eq!(prompt_completion.output_tokens, 5);
    }

    #[test]
    fn parse_prompt_completion_response_returns_error_without_result_payload() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "session-prompt-1"
        });

        // Act
        let prompt_completion = parse_prompt_completion_response(&response_value);

        // Assert
        assert_eq!(
            prompt_completion.err(),
            Some("Gemini ACP `session/prompt` response missing `result`".to_string())
        );
    }

    /// Verifies structured completion payloads override non-protocol streamed
    /// text during final Gemini turn assembly.
    #[test]
    fn select_preferred_assistant_message_prefers_structured_completion_payload() {
        // Arrange
        let streamed_message = "I have successfully added hello world.";
        let completion_message = r#"{"answer":"Structured.","questions":[],"summary":{"session":"Session summary","turn":"Turn summary"}}"#;

        // Act
        let selected_message =
            select_preferred_assistant_message(streamed_message, Some(completion_message));

        // Assert
        assert_eq!(selected_message, completion_message);
    }

    #[test]
    fn build_permission_response_prefers_allow_always_over_allow_once() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1",
                "options": [
                    {
                        "optionId": "allow-once",
                        "kind": "allow_once"
                    },
                    {
                        "optionId": "allow-always",
                        "kind": "allow_always"
                    }
                ]
            }
        });

        // Act
        let permission_response = build_permission_response(&response_value, "session-1");

        // Assert
        assert_eq!(
            permission_response,
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "result": {
                    "outcome": {
                        "outcome": "selected",
                        "optionId": "allow-always"
                    }
                }
            }))
        );
    }

    #[test]
    fn build_permission_response_selects_allow_once_when_allow_always_is_missing() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1",
                "options": [
                    {
                        "optionId": "reject-once",
                        "kind": "reject_once"
                    },
                    {
                        "optionId": "allow-once",
                        "kind": "allow_once"
                    }
                ]
            }
        });

        // Act
        let permission_response = build_permission_response(&response_value, "session-1");

        // Assert
        assert_eq!(
            permission_response,
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "result": {
                    "outcome": {
                        "outcome": "selected",
                        "optionId": "allow-once"
                    }
                }
            }))
        );
    }

    #[test]
    fn build_permission_response_returns_cancelled_without_options() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "perm-1",
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1",
                "options": []
            }
        });

        // Act
        let permission_response = build_permission_response(&response_value, "session-1");

        // Assert
        assert_eq!(
            permission_response,
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": "perm-1",
                "result": {
                    "outcome": {
                        "outcome": "cancelled"
                    }
                }
            }))
        );
    }

    #[test]
    fn build_permission_response_returns_cancelled_when_options_field_is_missing() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "perm-1",
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1"
            }
        });

        // Act
        let permission_response = build_permission_response(&response_value, "session-1");

        // Assert
        assert_eq!(
            permission_response,
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": "perm-1",
                "result": {
                    "outcome": {
                        "outcome": "cancelled"
                    }
                }
            }))
        );
    }

    #[test]
    fn build_permission_response_ignores_mismatched_session_id() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "perm-1",
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-2",
                "options": [
                    {
                        "optionId": "allow-once",
                        "kind": "allow_once"
                    }
                ]
            }
        });

        // Act
        let permission_response = build_permission_response(&response_value, "session-1");

        // Assert
        assert_eq!(permission_response, None);
    }

    /// Verifies raw-value permission parsing still selects an allow option
    /// when typed deserialization cannot consume the payload.
    #[test]
    fn build_permission_response_falls_back_to_raw_option_selection() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "permission-1",
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1",
                "options": [{
                    "optionId": "allow-once",
                    "kind": "allow_once"
                }]
            }
        });

        // Act
        let permission_response = build_permission_response(&response_value, "session-1");

        // Assert
        assert_eq!(
            permission_response,
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": "permission-1",
                "result": {
                    "outcome": {
                        "outcome": "selected",
                        "optionId": "allow-once"
                    }
                }
            }))
        );
    }

    #[tokio::test]
    /// Verifies Gemini prompt content blocks interleave text spans and local
    /// images in placeholder order.
    async fn build_prompt_content_blocks_interleaves_text_and_local_images() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let first_image_path = temp_directory.path().join("first.png");
        let second_image_path = temp_directory.path().join("second.png");
        std::fs::write(&first_image_path, b"first-image-bytes")
            .expect("first image should be written");
        std::fs::write(&second_image_path, b"second-image-bytes")
            .expect("second image should be written");
        let prompt = TurnPrompt {
            attachments: vec![
                TurnPromptAttachment {
                    placeholder: "[Image #1]".to_string(),
                    local_image_path: first_image_path,
                },
                TurnPromptAttachment {
                    placeholder: "[Image #2]".to_string(),
                    local_image_path: second_image_path,
                },
            ],
            text: "Compare [Image #2] against [Image #1] now".to_string(),
        };

        // Act
        let content_blocks = build_prompt_content_blocks(&prompt)
            .await
            .expect("content blocks should build");

        // Assert
        assert_eq!(
            content_blocks,
            vec![
                ContentBlock::Text(TextContent::new("Compare ".to_string())),
                ContentBlock::Image(ImageContent::new(
                    BASE64_STANDARD.encode("second-image-bytes"),
                    "image/png"
                )),
                ContentBlock::Text(TextContent::new(" against ".to_string())),
                ContentBlock::Image(ImageContent::new(
                    BASE64_STANDARD.encode("first-image-bytes"),
                    "image/png"
                )),
                ContentBlock::Text(TextContent::new(" now".to_string())),
            ]
        );
    }

    #[tokio::test]
    /// Verifies Gemini prompt content blocks preserve orphaned attachments by
    /// appending them after the text content.
    async fn build_prompt_content_blocks_appends_orphaned_attachments() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let image_path = temp_directory.path().join("orphan.png");
        std::fs::write(&image_path, b"orphan-image-bytes").expect("image should be written");
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: image_path,
            }],
            text: "Review this change".to_string(),
        };

        // Act
        let content_blocks = build_prompt_content_blocks(&prompt)
            .await
            .expect("content blocks should build");

        // Assert
        assert_eq!(
            content_blocks,
            vec![
                ContentBlock::Text(TextContent::new("Review this change".to_string())),
                ContentBlock::Image(ImageContent::new(
                    BASE64_STANDARD.encode("orphan-image-bytes"),
                    "image/png"
                )),
            ]
        );
    }

    #[test]
    /// Verifies Gemini prompt image MIME detection tracks common image
    /// extensions.
    fn prompt_image_mime_type_supports_common_extensions() {
        // Arrange
        let png_path = PathBuf::from("/tmp/image.png");
        let jpeg_path = PathBuf::from("/tmp/image.jpeg");
        let webp_path = PathBuf::from("/tmp/image.webp");

        // Act
        let png_mime_type = prompt_image_mime_type(&png_path);
        let jpeg_mime_type = prompt_image_mime_type(&jpeg_path);
        let webp_mime_type = prompt_image_mime_type(&webp_path);

        // Assert
        assert_eq!(png_mime_type, "image/png");
        assert_eq!(jpeg_mime_type, "image/jpeg");
        assert_eq!(webp_mime_type, "image/webp");
    }
}
