//! Gemini ACP lifecycle and turn orchestration.

use std::path::{Path, PathBuf};

use agent_client_protocol::{
    AGENT_METHOD_NAMES, ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, ProtocolVersion, TextContent,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

use super::transport::{GeminiRuntimeTransport, GeminiStdioTransport};
use super::{policy, stream_parser, usage};
use crate::domain::agent::AgentKind;
use crate::infra::agent;
use crate::infra::app_server::{AppServerError, AppServerStreamEvent, AppServerTurnRequest};
use crate::infra::app_server_transport::{self, extract_json_error_message, response_id_matches};
use crate::infra::channel::{TurnPrompt, TurnPromptAttachment, TurnPromptContentPart};

/// Mutable runtime state required while a Gemini ACP process is active.
pub(super) struct GeminiRuntimeState {
    /// Session worktree folder used as the runtime cwd.
    pub(super) folder: PathBuf,
    /// Selected Gemini model identifier.
    pub(super) model: String,
    /// Whether startup restored provider-native context.
    pub(super) restored_context: bool,
    /// Active provider-native session identifier.
    pub(super) session_id: String,
}

impl GeminiRuntimeState {
    /// Creates runtime state for one pending Gemini bootstrap.
    pub(super) fn new(folder: PathBuf, model: String) -> Self {
        Self {
            folder,
            model,
            restored_context: false,
            session_id: String::new(),
        }
    }
}

/// Starts one Gemini ACP runtime, initializes it, and creates a session.
pub(super) async fn start_runtime(
    request: &AppServerTurnRequest,
) -> Result<
    (
        tokio::process::Child,
        GeminiStdioTransport,
        GeminiRuntimeState,
    ),
    AppServerError,
> {
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
            AppServerError::Provider(format!("Failed to build `gemini --acp` command: {error}"))
        })?;
    let (mut child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(command, "gemini --acp")?;
    let mut transport = GeminiStdioTransport::new(stdin, stdout);
    let mut state = GeminiRuntimeState::new(request.folder.clone(), request.model.clone());

    match bootstrap_runtime_session(&mut transport, state.folder.as_path()).await {
        Ok(session_id) => {
            state.session_id = session_id;

            Ok((child, transport, state))
        }
        Err(error) => {
            transport.close_stdin();
            app_server_transport::shutdown_child(&mut child).await;

            Err(error)
        }
    }
}

/// Completes ACP bootstrap by sending `initialize` and creating
/// `session/new`.
pub(super) async fn bootstrap_runtime_session<Transport: GeminiRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
) -> Result<String, AppServerError> {
    initialize_runtime(transport).await?;

    start_session(transport, folder).await
}

/// Sends the ACP initialize handshake.
pub(super) async fn initialize_runtime<Transport: GeminiRuntimeTransport>(
    transport: &mut Transport,
) -> Result<(), AppServerError> {
    let initialization_request_id = format!("init-{}", uuid::Uuid::new_v4());
    let initialization_request = build_initialize_request_payload(&initialization_request_id)?;
    transport.write_json_line(initialization_request).await?;
    let initialize_response_line = transport
        .wait_for_response_line(initialization_request_id)
        .await?;
    let initialize_response =
        serde_json::from_str::<Value>(&initialize_response_line).map_err(|error| {
            AppServerError::Provider(format!(
                "Failed to parse Gemini ACP initialize response: {error}"
            ))
        })?;
    if initialize_response.get("error").is_some() {
        return Err(AppServerError::Provider(
            extract_json_error_message(&initialize_response)
                .unwrap_or_else(|| "Gemini ACP returned an error for `initialize`".to_string()),
        ));
    }
    parse_json_rpc_result::<InitializeResponse>(&initialize_response, "`initialize`")?;

    let initialized_notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized"
    });
    transport.write_json_line(initialized_notification).await?;

    Ok(())
}

/// Builds a typed ACP `initialize` request with conservative client
/// capabilities.
pub(super) fn build_initialize_request_payload(request_id: &str) -> Result<Value, AppServerError> {
    let initialize_params = InitializeRequest::new(ProtocolVersion::LATEST);
    let mut initialize_payload = build_json_rpc_request_payload(
        request_id,
        AGENT_METHOD_NAMES.initialize,
        initialize_params,
    )?;
    let Some(params) = initialize_payload.get_mut("params") else {
        return Err(AppServerError::Provider(
            "Failed to build Gemini ACP `initialize` request params".to_string(),
        ));
    };
    let Some(params) = params.as_object_mut() else {
        return Err(AppServerError::Provider(
            "Failed to build Gemini ACP `initialize` request params object".to_string(),
        ));
    };
    params.insert(
        "clientCapabilities".to_string(),
        Value::Object(serde_json::Map::new()),
    );

    Ok(initialize_payload)
}

/// Builds a typed JSON-RPC request payload.
pub(super) fn build_json_rpc_request_payload<T: Serialize>(
    request_id: &str,
    method: &str,
    params: T,
) -> Result<Value, AppServerError> {
    let params_value = serde_json::to_value(params).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to serialize `{method}` request params: {error}"
        ))
    })?;

    Ok(serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params_value
    }))
}

/// Extracts one typed JSON-RPC `result` payload.
pub(super) fn parse_json_rpc_result<T: serde::de::DeserializeOwned>(
    response_value: &Value,
    method: &str,
) -> Result<T, AppServerError> {
    let result_value = response_value.get("result").cloned().ok_or_else(|| {
        AppServerError::Provider(format!("Gemini ACP `{method}` response missing `result`"))
    })?;

    serde_json::from_value::<T>(result_value).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to parse Gemini ACP `{method}` result: {error}"
        ))
    })
}

/// Creates one ACP session and returns the assigned `sessionId`.
pub(super) async fn start_session<Transport: GeminiRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
) -> Result<String, AppServerError> {
    let session_new_id = format!("session-new-{}", uuid::Uuid::new_v4());
    let session_new_payload = build_json_rpc_request_payload(
        &session_new_id,
        AGENT_METHOD_NAMES.session_new,
        NewSessionRequest::new(folder.to_path_buf()),
    )?;
    transport.write_json_line(session_new_payload).await?;
    let response_line = transport.wait_for_response_line(session_new_id).await?;
    let response_value = serde_json::from_str::<Value>(&response_line).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to parse session/new response JSON: {error}"
        ))
    })?;

    parse_session_new_response(&response_value)
}

/// Parses one ACP `session/new` response into a session identifier.
pub(super) fn parse_session_new_response(response_value: &Value) -> Result<String, AppServerError> {
    if response_value.get("error").is_some() {
        return Err(AppServerError::Provider(
            extract_json_error_message(response_value)
                .unwrap_or_else(|| "Gemini ACP returned an error for `session/new`".to_string()),
        ));
    }

    let session_new_result =
        parse_json_rpc_result::<NewSessionResponse>(response_value, "`session/new`").map_err(
            |error| {
                let error_message = error.to_string();
                if error_message.contains("missing field `sessionId`") {
                    return AppServerError::Provider(
                        "Gemini ACP `session/new` response missing `sessionId`".to_string(),
                    );
                }

                error
            },
        )?;

    Ok(session_new_result.session_id.to_string())
}

/// Sends one prompt turn and waits for the matching prompt response id.
pub(super) async fn run_turn_with_runtime<Transport: GeminiRuntimeTransport>(
    transport: &mut Transport,
    session_id: &str,
    prompt: impl Into<TurnPrompt>,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
) -> Result<(String, u64, u64), AppServerError> {
    let prompt = prompt.into();
    let content_blocks = build_prompt_content_blocks(&prompt).await?;
    let prompt_id = format!("session-prompt-{}", uuid::Uuid::new_v4());
    let session_prompt_payload = build_json_rpc_request_payload(
        &prompt_id,
        AGENT_METHOD_NAMES.session_prompt,
        PromptRequest::new(session_id.to_string(), content_blocks),
    )?;
    transport.write_json_line(session_prompt_payload).await?;

    let mut assistant_message = String::new();
    tokio::time::timeout(app_server_transport::TURN_TIMEOUT, async {
        loop {
            let stdout_line = transport.next_stdout().await?.ok_or_else(|| {
                AppServerError::Provider(
                    "Gemini ACP terminated before prompt completion response".to_string(),
                )
            })?;

            if stdout_line.trim().is_empty() {
                continue;
            }

            let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                continue;
            };

            if let Some(permission_response) =
                policy::build_permission_response(&response_value, session_id)
            {
                transport.write_json_line(permission_response).await?;

                continue;
            }

            if response_id_matches(&response_value, &prompt_id) {
                if response_value.get("error").is_some() {
                    return Err(AppServerError::Provider(
                        extract_json_error_message(&response_value).unwrap_or_else(|| {
                            "Gemini ACP returned an error for `session/prompt`".to_string()
                        }),
                    ));
                }
                let prompt_completion = usage::parse_prompt_completion_response(&response_value)?;
                assistant_message = stream_parser::select_preferred_assistant_message(
                    &assistant_message,
                    prompt_completion.assistant_message.as_deref(),
                );

                return Ok((
                    assistant_message,
                    prompt_completion.input_tokens,
                    prompt_completion.output_tokens,
                ));
            }

            if let Some(progress) =
                stream_parser::extract_progress_update(&response_value, session_id)
            {
                let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(progress));
            }

            if let Some(chunk) =
                stream_parser::extract_assistant_message_chunk(&response_value, session_id)
            {
                assistant_message.push_str(chunk.as_str());
                stream_assistant_chunk(&stream_tx, chunk);
            }
        }
    })
    .await
    .map_err(|_| {
        AppServerError::Provider(format!(
            "Timed out waiting for Gemini ACP prompt completion after {} seconds",
            app_server_transport::TURN_TIMEOUT.as_secs()
        ))
    })?
}

/// Streams one non-empty assistant delta chunk to the UI.
pub(super) fn stream_assistant_chunk(
    stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    chunk: String,
) {
    if chunk.is_empty() {
        return;
    }

    let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
        is_delta: true,
        message: chunk,
        phase: None,
    });
}

/// Builds Gemini ACP content blocks for one structured prompt payload.
pub(super) async fn build_prompt_content_blocks(
    prompt: &TurnPrompt,
) -> Result<Vec<ContentBlock>, AppServerError> {
    let prompt = prompt.clone();

    tokio::task::spawn_blocking(move || build_prompt_content_blocks_blocking(&prompt))
        .await
        .map_err(|error| {
            AppServerError::Provider(format!("Gemini prompt-image task failed: {error}"))
        })?
}

/// Builds Gemini ACP content blocks for one prompt on a blocking worker
/// thread.
pub(super) fn build_prompt_content_blocks_blocking(
    prompt: &TurnPrompt,
) -> Result<Vec<ContentBlock>, AppServerError> {
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
pub(super) fn push_text_content_block(content_blocks: &mut Vec<ContentBlock>, text: &str) {
    if text.is_empty() {
        return;
    }

    content_blocks.push(ContentBlock::Text(TextContent::new(text.to_string())));
}

/// Builds one Gemini ACP image content block from a persisted local prompt
/// attachment.
pub(super) fn build_image_content_block(
    attachment: &TurnPromptAttachment,
) -> Result<ContentBlock, AppServerError> {
    let image_bytes = std::fs::read(&attachment.local_image_path).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to read Gemini prompt image `{}`: {error}",
            attachment.local_image_path.display()
        ))
    })?;
    let mime_type = prompt_image_mime_type(&attachment.local_image_path);

    Ok(ContentBlock::Image(
        agent_client_protocol::ImageContent::new(BASE64_STANDARD.encode(image_bytes), mime_type),
    ))
}

/// Returns the MIME type Gemini should use for one persisted prompt image.
#[must_use]
pub(super) fn prompt_image_mime_type(local_image_path: &Path) -> &'static str {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use mockall::Sequence;
    use tempfile::tempdir;

    use super::*;
    use crate::infra::agent::app_server::gemini::MockGeminiRuntimeTransport;
    use crate::infra::app_server_transport::AppServerTransportError;
    use crate::infra::channel::TurnPromptAttachment;

    /// Captures the dynamic JSON-RPC `id` from a written payload through the
    /// supplied mutex so the response side of a mock can echo it back.
    fn remember_request_id(id_store: &Arc<Mutex<Option<String>>>, payload: &Value) {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if let Ok(mut guard) = id_store.lock() {
            *guard = id;
        }
    }

    #[test]
    fn gemini_runtime_state_new_initializes_empty_session_id_and_no_restored_context() {
        // Arrange
        let folder = PathBuf::from("/tmp/agentty-gemini-state");
        let model = "gemini-3-flash-preview".to_string();

        // Act
        let state = GeminiRuntimeState::new(folder.clone(), model.clone());

        // Assert
        assert_eq!(state.folder, folder);
        assert_eq!(state.model, model);
        assert!(state.session_id.is_empty());
        assert!(!state.restored_context);
    }

    #[test]
    fn build_initialize_request_payload_carries_jsonrpc_method_and_client_capabilities_object() {
        // Arrange / Act
        let payload =
            build_initialize_request_payload("init-1").expect("initialize payload should build");

        // Assert
        assert_eq!(payload.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
        assert_eq!(payload.get("id").and_then(Value::as_str), Some("init-1"));
        assert_eq!(
            payload.get("method").and_then(Value::as_str),
            Some(AGENT_METHOD_NAMES.initialize)
        );
        let params = payload.get("params").expect("initialize params present");
        let client_capabilities = params
            .get("clientCapabilities")
            .expect("initialize payload should include clientCapabilities");
        assert!(client_capabilities.is_object());
    }

    #[test]
    fn build_json_rpc_request_payload_serializes_typed_params_and_metadata() {
        // Arrange
        #[derive(serde::Serialize)]
        struct TestParams {
            value: i32,
        }

        // Act
        let payload =
            build_json_rpc_request_payload("req-7", "custom/method", TestParams { value: 42 })
                .expect("json-rpc payload should build");

        // Assert
        assert_eq!(payload.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
        assert_eq!(payload.get("id").and_then(Value::as_str), Some("req-7"));
        assert_eq!(
            payload.get("method").and_then(Value::as_str),
            Some("custom/method")
        );
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("value"))
                .and_then(Value::as_i64),
            Some(42)
        );
    }

    #[test]
    fn parse_json_rpc_result_returns_provider_error_when_result_field_is_missing() {
        // Arrange
        let response = serde_json::json!({"jsonrpc": "2.0", "id": "x"});

        // Act
        let result = parse_json_rpc_result::<serde_json::Map<String, Value>>(&response, "`x`");

        // Assert
        let error = result.expect_err("parse should fail when result missing");
        assert!(
            matches!(error, AppServerError::Provider(ref message) if message.contains("missing `result`"))
        );
    }

    #[test]
    fn parse_session_new_response_returns_provider_error_when_response_carries_error_field() {
        // Arrange
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "session-new-1",
            "error": {"code": -32603, "message": "session/new failed"}
        });

        // Act
        let result = parse_session_new_response(&response);

        // Assert
        let error = result.expect_err("session/new error should propagate");
        assert!(
            matches!(error, AppServerError::Provider(ref message) if message.contains("session/new failed"))
        );
    }

    #[test]
    fn parse_session_new_response_returns_helpful_error_when_session_id_is_missing() {
        // Arrange
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "session-new-1",
            "result": {}
        });

        // Act
        let result = parse_session_new_response(&response);

        // Assert
        let error = result.expect_err("missing sessionId should be reported");
        assert!(
            matches!(error, AppServerError::Provider(ref message) if message.contains("missing `sessionId`"))
        );
    }

    #[test]
    fn parse_session_new_response_returns_session_id_for_well_formed_response() {
        // Arrange
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "session-new-1",
            "result": {"sessionId": "session-abc"}
        });

        // Act
        let session_id =
            parse_session_new_response(&response).expect("session id should be parsed");

        // Assert
        assert_eq!(session_id, "session-abc");
    }

    #[test]
    fn push_text_content_block_skips_empty_text_and_appends_non_empty_text() {
        // Arrange
        let mut blocks = Vec::new();

        // Act
        push_text_content_block(&mut blocks, "");
        push_text_content_block(&mut blocks, "hello");

        // Assert
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Text(text_content) => assert_eq!(text_content.text, "hello"),
            other => unreachable!("expected text content block, got {other:?}"),
        }
    }

    #[test]
    fn prompt_image_mime_type_maps_known_extensions_and_falls_back_to_png() {
        // Arrange
        let cases = [
            ("/tmp/a.gif", "image/gif"),
            ("/tmp/a.GIF", "image/gif"),
            ("/tmp/a.jpg", "image/jpeg"),
            ("/tmp/a.JPEG", "image/jpeg"),
            ("/tmp/a.webp", "image/webp"),
            ("/tmp/a.png", "image/png"),
            ("/tmp/a.bmp", "image/png"),
            ("/tmp/no_extension", "image/png"),
        ];

        // Act / Assert
        for (path, expected_mime) in cases {
            let actual_mime = prompt_image_mime_type(Path::new(path));
            assert_eq!(actual_mime, expected_mime, "mime mismatch for {path}");
        }
    }

    #[test]
    fn build_prompt_content_blocks_blocking_returns_single_text_block_when_no_attachments_present()
    {
        // Arrange
        let prompt = TurnPrompt::from_text("Hi there".to_string());

        // Act
        let blocks = build_prompt_content_blocks_blocking(&prompt).expect("blocks should build");

        // Assert
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Text(text_content) => assert_eq!(text_content.text, "Hi there"),
            other => unreachable!("expected text content block, got {other:?}"),
        }
    }

    #[test]
    fn build_prompt_content_blocks_blocking_interleaves_text_and_image_blocks_in_placeholder_order()
    {
        // Arrange
        let temp_dir = tempdir().expect("temp directory should be created");
        let attachment_path = temp_dir.path().join("sample.gif");
        std::fs::write(&attachment_path, b"fake-gif-bytes")
            .expect("attachment file should be written");
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: attachment_path,
            }],
            text: "Look at [Image #1] now".to_string(),
        };

        // Act
        let blocks = build_prompt_content_blocks_blocking(&prompt)
            .expect("blocks should build with attachments");

        // Assert
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            ContentBlock::Text(text_content) => assert_eq!(text_content.text, "Look at "),
            other => unreachable!("expected text content block at 0, got {other:?}"),
        }
        match &blocks[1] {
            ContentBlock::Image(image_content) => {
                assert_eq!(image_content.mime_type, "image/gif");
                assert!(!image_content.data.is_empty());
            }
            other => unreachable!("expected image content block at 1, got {other:?}"),
        }
        match &blocks[2] {
            ContentBlock::Text(text_content) => assert_eq!(text_content.text, " now"),
            other => unreachable!("expected text content block at 2, got {other:?}"),
        }
    }

    #[test]
    fn build_image_content_block_returns_provider_error_when_local_path_does_not_exist() {
        // Arrange
        let attachment = TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from(
                "/nonexistent/agentty-gemini-test-image-which-does-not-exist.png",
            ),
        };

        // Act
        let result = build_image_content_block(&attachment);

        // Assert
        let error = result.expect_err("missing image should produce error");
        assert!(
            matches!(error, AppServerError::Provider(ref message) if message.contains("Failed to read Gemini prompt image"))
        );
    }

    #[test]
    fn stream_assistant_chunk_skips_empty_chunk_and_emits_assistant_message_for_non_empty_chunk() {
        // Arrange
        let (sender, mut receiver) = mpsc::unbounded_channel();

        // Act
        stream_assistant_chunk(&sender, String::new());
        stream_assistant_chunk(&sender, "delta payload".to_string());
        drop(sender);

        // Assert
        let event = receiver
            .blocking_recv()
            .expect("non-empty chunk should produce one event");
        match event {
            AppServerStreamEvent::AssistantMessage {
                is_delta,
                message,
                phase,
            } => {
                assert!(is_delta);
                assert_eq!(message, "delta payload");
                assert!(phase.is_none());
            }
            other @ AppServerStreamEvent::ProgressUpdate(_) => {
                unreachable!("expected AssistantMessage event, got {other:?}")
            }
        }
        assert!(
            receiver.blocking_recv().is_none(),
            "empty chunk should not have been sent"
        );
    }

    #[tokio::test]
    async fn initialize_runtime_writes_initialize_then_initialized_notification() {
        // Arrange
        let request_id = Arc::new(Mutex::new(None));
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();

        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| {
                payload.get("method").and_then(Value::as_str) == Some(AGENT_METHOD_NAMES.initialize)
            })
            .returning({
                let request_id = Arc::clone(&request_id);

                move |payload| {
                    remember_request_id(&request_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let request_id = Arc::clone(&request_id);

                move |_| {
                    let response_id = request_id
                        .lock()
                        .expect("initialize id mutex should lock")
                        .clone()
                        .expect("initialize id should be recorded");

                    Box::pin(async move {
                        Ok(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "result": {
                                "protocolVersion": ProtocolVersion::LATEST,
                                "agentCapabilities": {}
                            }
                        })
                        .to_string())
                    })
                }
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialized"))
            .returning(|_| Box::pin(async { Ok(()) }));

        // Act
        let result = initialize_runtime(&mut transport).await;

        // Assert
        assert!(
            result.is_ok(),
            "initialize_runtime should succeed: {result:?}"
        );
    }

    #[tokio::test]
    async fn initialize_runtime_returns_provider_error_when_response_carries_error_field() {
        // Arrange
        let request_id = Arc::new(Mutex::new(None));
        let mut transport = MockGeminiRuntimeTransport::new();

        transport.expect_write_json_line().times(1).returning({
            let request_id = Arc::clone(&request_id);

            move |payload| {
                remember_request_id(&request_id, &payload);

                Box::pin(async { Ok(()) })
            }
        });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .returning({
                let request_id = Arc::clone(&request_id);

                move |_| {
                    let response_id = request_id
                        .lock()
                        .expect("initialize id mutex should lock")
                        .clone()
                        .expect("initialize id should be recorded");

                    Box::pin(async move {
                        Ok(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "error": {"code": -32603, "message": "initialize failed"}
                        })
                        .to_string())
                    })
                }
            });

        // Act
        let result = initialize_runtime(&mut transport).await;

        // Assert
        let error = result.expect_err("initialize_runtime should surface provider error");
        assert!(
            matches!(error, AppServerError::Provider(ref message) if message.contains("initialize failed"))
        );
    }

    #[tokio::test]
    async fn start_session_returns_session_id_from_well_formed_session_new_response() {
        // Arrange
        let folder = tempdir().expect("temp folder should be created");
        let request_id = Arc::new(Mutex::new(None));
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();

        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf({
                let folder_path = folder.path().to_path_buf();

                move |payload| {
                    payload.get("method").and_then(Value::as_str)
                        == Some(AGENT_METHOD_NAMES.session_new)
                        && payload
                            .get("params")
                            .and_then(|params| params.get("cwd"))
                            .and_then(Value::as_str)
                            == Some(folder_path.to_string_lossy().as_ref())
                }
            })
            .returning({
                let request_id = Arc::clone(&request_id);

                move |payload| {
                    remember_request_id(&request_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let request_id = Arc::clone(&request_id);

                move |_| {
                    let response_id = request_id
                        .lock()
                        .expect("session-new id mutex should lock")
                        .clone()
                        .expect("session-new id should be recorded");

                    Box::pin(async move {
                        Ok(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": response_id,
                            "result": {"sessionId": "session-xyz"}
                        })
                        .to_string())
                    })
                }
            });

        // Act
        let session_id = start_session(&mut transport, folder.path()).await;

        // Assert
        assert_eq!(
            session_id.expect("session_id should be returned"),
            "session-xyz"
        );
    }

    #[tokio::test]
    async fn start_session_propagates_transport_termination_error() {
        // Arrange
        let folder = tempdir().expect("temp folder should be created");
        let mut transport = MockGeminiRuntimeTransport::new();

        transport
            .expect_write_json_line()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line()
            .times(1)
            .returning(|_| Box::pin(async { Err(AppServerTransportError::ProcessTerminated) }));

        // Act
        let result = start_session(&mut transport, folder.path()).await;

        // Assert
        let error = result.expect_err("start_session should propagate transport error");
        assert!(matches!(error, AppServerError::Transport(_)));
    }
}
