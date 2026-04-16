//! Gemini ACP client orchestration.

use tokio::sync::mpsc;

use super::lifecycle::{self, GeminiRuntimeState};
use super::transport::GeminiStdioTransport;
use crate::infra::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::app_server_transport;

/// Production [`AppServerClient`] backed by `gemini --acp`.
pub(crate) struct RealGeminiAcpClient {
    sessions: AppServerSessionRegistry<GeminiSessionRuntime>,
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
                    let (child, transport, state) = lifecycle::start_runtime(&request).await?;

                    Ok(GeminiSessionRuntime {
                        child,
                        state,
                        transport,
                    })
                })
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

                Box::pin(async move {
                    lifecycle::run_turn_with_runtime(
                        &mut runtime.transport,
                        &runtime.state.session_id,
                        prompt,
                        stream_tx,
                    )
                    .await
                })
            },
            |runtime| Box::pin(Self::shutdown_runtime(runtime)),
        )
        .await
    }

    /// Terminates one Gemini ACP runtime process.
    async fn shutdown_runtime(session: &mut GeminiSessionRuntime) {
        session.transport.close_stdin();
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

/// Active Gemini ACP session runtime.
struct GeminiSessionRuntime {
    child: tokio::process::Child,
    state: GeminiRuntimeState,
    transport: GeminiStdioTransport,
}

impl GeminiSessionRuntime {
    /// Returns whether the runtime matches one incoming turn request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.folder == request.folder && self.state.model == request.model
    }

    /// Returns whether runtime startup restored prior provider context.
    fn restored_context(&self) -> bool {
        self.state.restored_context
    }

    /// Returns the active provider-native Gemini ACP `sessionId`, or `None`
    /// when the runtime has not yet started a session.
    fn provider_conversation_id(&self) -> Option<String> {
        if self.state.session_id.is_empty() {
            None
        } else {
            Some(self.state.session_id.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use agent_client_protocol::{InitializeResponse, ProtocolVersion};
    use mockall::Sequence;
    use serde_json::Value;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::infra::agent::app_server::gemini::{
        MockGeminiRuntimeTransport, lifecycle, policy, stream_parser, usage,
    };

    /// Captures the dynamic request id from a written payload.
    fn remember_request_id(id_store: &Arc<Mutex<Option<String>>>, payload: &Value) {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if let Ok(mut guard) = id_store.lock() {
            *guard = id;
        }
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
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialize"))
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
            .returning(move |_| {
                let response_id = request_id
                    .lock()
                    .expect("request mutex should lock")
                    .clone()
                    .expect("initialize id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({
                        "id": response_id,
                        "result": serde_json::to_value(InitializeResponse::new(ProtocolVersion::LATEST))
                            .expect("initialize response should serialize")
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
        let result = lifecycle::initialize_runtime(&mut transport).await;

        // Assert
        result.expect("initialize should succeed");
    }

    #[tokio::test]
    async fn bootstrap_runtime_session_initializes_then_creates_session() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let init_id = Arc::new(Mutex::new(None));
        let session_new_id = Arc::new(Mutex::new(None));
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();

        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialize"))
            .returning({
                let init_id = Arc::clone(&init_id);

                move |payload| {
                    remember_request_id(&init_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                let response_id = init_id
                    .lock()
                    .expect("init mutex should lock")
                    .clone()
                    .expect("initialize id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({
                        "id": response_id,
                        "result": serde_json::to_value(InitializeResponse::new(ProtocolVersion::LATEST))
                            .expect("initialize response should serialize")
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
            .returning({
                let session_new_id = Arc::clone(&session_new_id);

                move |payload| {
                    remember_request_id(&session_new_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                let response_id = session_new_id
                    .lock()
                    .expect("session mutex should lock")
                    .clone()
                    .expect("session/new id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({
                        "id": response_id,
                        "result": {"sessionId": "session-1"}
                    })
                    .to_string())
                })
            });

        // Act
        let session_id = lifecycle::bootstrap_runtime_session(&mut transport, folder.path()).await;

        // Assert
        assert_eq!(session_id.expect("bootstrap should succeed"), "session-1");
    }

    #[tokio::test]
    async fn run_turn_with_runtime_handles_permission_progress_chunk_and_completion() {
        // Arrange
        let prompt_id = Arc::new(Mutex::new(None));
        let mut transport = MockGeminiRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        expect_permission_progress_chunk_and_completion(&mut transport, &mut sequence, prompt_id);

        // Act
        let result = lifecycle::run_turn_with_runtime(
            &mut transport,
            "session-1",
            "Implement the task",
            stream_tx,
        )
        .await;

        // Assert
        let (assistant_message, input_tokens, output_tokens) =
            result.expect("turn should complete");
        assert_eq!(assistant_message, "Chunk text");
        assert_eq!(input_tokens, 11);
        assert_eq!(output_tokens, 4);
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

    /// Expects a Gemini prompt flow with permission approval, tool progress,
    /// content delta, and final completion.
    fn expect_permission_progress_chunk_and_completion(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
        prompt_id: Arc<Mutex<Option<String>>>,
    ) {
        expect_prompt_request(transport, sequence, &prompt_id);
        expect_permission_request_and_response(transport, sequence);
        expect_tool_progress_update(transport, sequence);
        expect_message_chunk_update(transport, sequence);
        expect_prompt_completion(transport, sequence, prompt_id);
    }

    /// Expects the initial Gemini prompt request.
    fn expect_prompt_request(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
        prompt_id: &Arc<Mutex<Option<String>>>,
    ) {
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(|payload| {
                payload.get("method").and_then(Value::as_str) == Some("session/prompt")
            })
            .returning({
                let prompt_id = Arc::clone(prompt_id);

                move |payload| {
                    remember_request_id(&prompt_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
    }

    /// Expects permission request output and the matching approval response.
    fn expect_permission_request_and_response(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
    ) {
        transport
            .expect_next_stdout()
            .in_sequence(sequence)
            .times(1)
            .return_once(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
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
                        })
                        .to_string(),
                    ))
                })
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(|payload| payload.get("id") == Some(&Value::String("permission-1".to_string())))
            .returning(|_| Box::pin(async { Ok(()) }));
    }

    /// Expects one Gemini tool-progress update.
    fn expect_tool_progress_update(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .return_once(|| {
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

    /// Expects one assistant message chunk update.
    fn expect_message_chunk_update(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .return_once(|| {
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

    /// Expects the final Gemini prompt completion response.
    fn expect_prompt_completion(
        transport: &mut MockGeminiRuntimeTransport,
        sequence: &mut Sequence,
        prompt_id: Arc<Mutex<Option<String>>>,
    ) {
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .return_once(move || {
                let response_id = prompt_id
                    .lock()
                    .expect("prompt mutex should lock")
                    .clone()
                    .expect("prompt id should be recorded");

                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({
                            "id": response_id,
                            "result": {
                                "usage": {
                                    "inputTokens": 11,
                                    "outputTokens": 4
                                },
                                "response": "Chunk text"
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    #[test]
    fn parse_prompt_completion_response_reads_output_content_parts() {
        // Arrange
        let response_value = serde_json::json!({
            "result": {
                "usage": {
                    "inputTokens": 9,
                    "outputTokens": 3
                },
                "output": [{
                    "content": [{
                        "text": "Part one"
                    }, {
                        "text": " and part two"
                    }]
                }]
            }
        });

        // Act
        let prompt_completion = usage::parse_prompt_completion_response(&response_value);

        // Assert
        let prompt_completion = prompt_completion.expect("completion should parse");
        assert_eq!(
            prompt_completion.assistant_message,
            Some("Part one and part two".to_string())
        );
        assert_eq!(prompt_completion.input_tokens, 9);
        assert_eq!(prompt_completion.output_tokens, 3);
    }

    #[test]
    fn build_permission_response_prefers_allow_always_over_allow_once() {
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
                }, {
                    "optionId": "allow-always",
                    "kind": "allow_always"
                }]
            }
        });

        // Act
        let permission_response = policy::build_permission_response(&response_value, "session-1")
            .expect("permission response should be built");

        // Assert
        assert_eq!(
            permission_response
                .get("result")
                .and_then(|result| result.get("outcome"))
                .and_then(|outcome| outcome.get("optionId"))
                .and_then(Value::as_str),
            Some("allow-always")
        );
    }

    #[test]
    fn select_preferred_assistant_message_prefers_structured_completion_payload() {
        // Arrange
        let streamed_message = "partial non-json";
        let completion_message =
            Some(r#"{"answer":"Final","questions":[],"follow_up_tasks":[],"summary":null}"#);

        // Act
        let selected =
            stream_parser::select_preferred_assistant_message(streamed_message, completion_message);

        // Assert
        assert_eq!(
            selected,
            r#"{"answer":"Final","questions":[],"follow_up_tasks":[],"summary":null}"#
        );
    }
}
