//! Gemini ACP-backed implementation of the shared app-server client.

use std::path::PathBuf;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::sync::mpsc;

use crate::infra::app_server::{
    self, AppServerClient, AppServerFuture, AppServerSessionRegistry, AppServerStreamEvent,
    AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::app_server_transport::{
    self, extract_json_error_message, response_id_matches, write_json_line,
};

/// Production [`AppServerClient`] backed by `gemini --experimental-acp`.
pub struct RealGeminiAcpClient {
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
    pub fn new() -> Self {
        Self {
            sessions: AppServerSessionRegistry::new("Gemini ACP"),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<GeminiSessionRuntime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, String> {
        let stream_tx = stream_tx.clone();

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            GeminiSessionRuntime::matches_request,
            |runtime| runtime.child.id(),
            |request| {
                let request = request.clone();

                Box::pin(async move { Self::start_runtime(&request).await })
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

                Box::pin(Self::run_turn_with_runtime(runtime, prompt, stream_tx))
            },
            |runtime| Box::pin(Self::shutdown_runtime(runtime)),
        )
        .await
    }

    /// Starts one Gemini ACP runtime, initializes it, and creates a session.
    async fn start_runtime(request: &AppServerTurnRequest) -> Result<GeminiSessionRuntime, String> {
        let mut command = tokio::process::Command::new("gemini");
        command
            .arg("--experimental-acp")
            .arg("--model")
            .arg(&request.model)
            .current_dir(&request.folder)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

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
            session_id: String::new(),
            stdin,
            stdout_lines: BufReader::new(stdout).lines(),
        };

        Self::initialize_runtime(&mut session).await?;
        session.session_id = Self::start_session(&mut session).await?;

        Ok(session)
    }

    /// Sends the ACP initialize handshake.
    async fn initialize_runtime(session: &mut GeminiSessionRuntime) -> Result<(), String> {
        let initialization_request_id = format!("init-{}", uuid::Uuid::new_v4());
        let initialization_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": initialization_request_id,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {}
            }
        });
        write_json_line(&mut session.stdin, &initialization_request).await?;
        let initialize_response_line = app_server_transport::wait_for_response_line(
            &mut session.stdout_lines,
            &initialization_request_id,
        )
        .await?;
        let initialize_response = serde_json::from_str::<Value>(&initialize_response_line)
            .map_err(|error| format!("Failed to parse Gemini ACP initialize response: {error}"))?;
        if initialize_response.get("error").is_some() {
            return Err(extract_json_error_message(&initialize_response)
                .unwrap_or_else(|| "Gemini ACP returned an error for `initialize`".to_string()));
        }

        let initialized_notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized"
        });
        write_json_line(&mut session.stdin, &initialized_notification).await?;

        Ok(())
    }

    /// Creates one ACP session and returns the assigned `sessionId`.
    ///
    /// JSON-RPC `error` payloads are surfaced directly to keep diagnostics
    /// actionable when session creation fails.
    async fn start_session(session: &mut GeminiSessionRuntime) -> Result<String, String> {
        let session_new_id = format!("session-new-{}", uuid::Uuid::new_v4());
        let session_new_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": session_new_id,
            "method": "session/new",
            "params": {
                "cwd": session.folder.to_string_lossy(),
                "mcpServers": []
            }
        });
        write_json_line(&mut session.stdin, &session_new_payload).await?;
        let response_line = app_server_transport::wait_for_response_line(
            &mut session.stdout_lines,
            &session_new_id,
        )
        .await?;
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

        response_value
            .get("result")
            .and_then(|result| result.get("sessionId"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| "Gemini ACP `session/new` response missing `sessionId`".to_string())
    }

    /// Sends one prompt turn and waits for the matching prompt response id.
    ///
    /// Streaming progress updates are forwarded to the UI while assistant text
    /// chunks are streamed to the UI and accumulated for the final response.
    async fn run_turn_with_runtime(
        session: &mut GeminiSessionRuntime,
        prompt: &str,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<(String, u64, u64), String> {
        let prompt_id = format!("session-prompt-{}", uuid::Uuid::new_v4());
        let session_prompt_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": prompt_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session.session_id,
                "prompt": [{
                    "type": "text",
                    "text": prompt
                }]
            }
        });
        write_json_line(&mut session.stdin, &session_prompt_payload).await?;

        let mut assistant_message = String::new();
        tokio::time::timeout(app_server_transport::TURN_TIMEOUT, async {
            loop {
                let stdout_line = session
                    .stdout_lines
                    .next_line()
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
                    build_permission_response(&response_value, &session.session_id)
                {
                    write_json_line(&mut session.stdin, &permission_response).await?;

                    continue;
                }

                if response_id_matches(&response_value, &prompt_id) {
                    if response_value.get("error").is_some() {
                        return Err(extract_json_error_message(&response_value).unwrap_or_else(
                            || "Gemini ACP returned an error for `session/prompt`".to_string(),
                        ));
                    }
                    let prompt_completion = parse_prompt_completion_response(&response_value)?;
                    if assistant_message.trim().is_empty()
                        && let Some(fallback_message) = prompt_completion.assistant_message
                    {
                        assistant_message = fallback_message;
                    }

                    return Ok((
                        assistant_message,
                        prompt_completion.input_tokens,
                        prompt_completion.output_tokens,
                    ));
                }

                if let Some(progress) =
                    extract_progress_update(&response_value, &session.session_id)
                {
                    let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(progress));
                }

                if let Some(chunk) =
                    extract_assistant_message_chunk(&response_value, &session.session_id)
                {
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

    /// Streams one non-empty assistant chunk to the UI.
    fn stream_assistant_chunk(
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
        chunk: String,
    ) {
        if chunk.is_empty() {
            return;
        }

        let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage(chunk));
    }

    /// Terminates one Gemini ACP runtime process.
    async fn shutdown_runtime(session: &mut GeminiSessionRuntime) {
        app_server_transport::shutdown_child(&mut session.child).await;
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
    ) -> AppServerFuture<Result<AppServerTurnResponse, String>> {
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
    session_id: String,
    stdin: tokio::process::ChildStdin,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
}

impl GeminiSessionRuntime {
    /// Returns whether the runtime matches one incoming turn request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.folder == request.folder && self.model == request.model
    }
}

/// Builds a `session/request_permission` response for the active session.
///
/// The response follows ACP's `RequestPermissionResponse` shape. When an allow
/// option is available, this selects it to match auto-edit behavior. When no
/// options are provided or parsable, this returns a `cancelled` outcome to
/// avoid leaving the turn blocked indefinitely.
fn build_permission_response(response_value: &Value, expected_session_id: &str) -> Option<Value> {
    if response_value.get("method").and_then(Value::as_str) != Some("session/request_permission") {
        return None;
    }

    let params = response_value.get("params")?;
    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    let request_id = response_value.get("id")?.clone();
    if let Some(option_id) = params.get("options").and_then(select_permission_option_id) {
        return Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id
                }
            }
        }));
    }

    Some(serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "outcome": {
                "outcome": "cancelled"
            }
        }
    }))
}

/// Selects the preferred allow option from ACP permission choices.
///
/// Preference order is `allow_always`, then `allow_once`, then the first
/// listed option when no allow-kind option is available.
fn select_permission_option_id(options: &Value) -> Option<String> {
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
/// objects.
fn extract_text_from_content_value(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let mut combined_text = String::new();
            for part in parts {
                if let Some(part_text) = part.as_str() {
                    combined_text.push_str(part_text);

                    continue;
                }

                if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                    combined_text.push_str(part_text);
                }
            }
            if combined_text.is_empty() {
                return None;
            }

            Some(combined_text)
        }
        Value::Object(_) => content
            .get("text")
            .and_then(Value::as_str)
            .map(ToString::to_string),
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

    response_value
        .get("params")
        .and_then(|params| params.get("update"))
        .and_then(|update| update.get("content"))
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
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
    if response_value.get("method").and_then(Value::as_str) != Some("session/update") {
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
    use super::*;

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
            Some(AppServerStreamEvent::AssistantMessage(
                "Hello from Gemini".to_string()
            ))
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
}
