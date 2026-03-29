//! App-server RPC [`AgentChannel`] adapter.
//!
//! Delegates turn execution to [`AppServerClient`] and bridges
//! [`AppServerStreamEvent`]s to the unified [`TurnEvent`] stream.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::domain::agent::AgentKind;
use crate::infra::agent;
use crate::infra::app_server::{AppServerClient, AppServerStreamEvent, AppServerTurnRequest};
use crate::infra::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};

/// [`AgentChannel`] adapter backed by a persistent app-server session.
///
/// Turn execution is delegated to [`AppServerClient::run_turn`].
/// [`AppServerStreamEvent`]s emitted by the provider are bridged to
/// [`TurnEvent::ThoughtDelta`] values when transient loader text should be
/// updated.
pub struct AppServerAgentChannel {
    /// Provider-specific app-server client.
    client: Arc<dyn AppServerClient>,
    /// Provider kind routed through this channel instance.
    kind: AgentKind,
}

impl AppServerAgentChannel {
    /// Creates a new app-server channel backed by the given client.
    pub fn new(client: Arc<dyn AppServerClient>, kind: AgentKind) -> Self {
        Self { client, kind }
    }
}

impl AgentChannel for AppServerAgentChannel {
    /// Returns a [`SessionRef`] immediately; the app-server session is
    /// initialised lazily on the first turn.
    fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> AgentFuture<Result<SessionRef, AgentError>> {
        let session_id = req.session_id;

        Box::pin(async move { Ok(SessionRef { session_id }) })
    }

    /// Runs one app-server turn and bridges stream events to [`TurnEvent`]s.
    ///
    /// Assistant stream chunks are never appended directly to the transcript.
    /// Instead, Codex thought-style deltas (`phase: thinking/plan`) and
    /// provider progress updates are bridged to [`TurnEvent::ThoughtDelta`] so
    /// the UI loader can reflect transient state while the final persisted
    /// output still comes only from the parsed [`TurnResult`].
    ///
    /// # Errors
    /// Returns [`AgentError`] when [`AppServerClient::run_turn`] fails.
    fn run_turn(
        &self,
        session_id: String,
        req: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        let client = Arc::clone(&self.client);
        let kind = self.kind;
        Box::pin(async move {
            let request = AppServerTurnRequest {
                folder: req.folder,
                live_session_output: req.live_session_output,
                model: req.model,
                prompt: req.prompt,
                request_kind: req.request_kind,
                provider_conversation_id: req.provider_conversation_id,
                reasoning_level: req.reasoning_level,
                session_id,
            };
            let protocol_profile = request.request_kind.protocol_profile();
            let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<AppServerStreamEvent>();

            let bridge_handle = {
                let events = events.clone();

                tokio::spawn(async move {
                    while let Some(event) = stream_rx.recv().await {
                        match event {
                            AppServerStreamEvent::AssistantMessage {
                                message,
                                phase,
                                is_delta,
                            } => {
                                let trimmed = message.trim_end();
                                if trimmed.trim().is_empty() {
                                    continue;
                                }

                                if agent::is_app_server_thought_chunk(
                                    kind,
                                    is_delta,
                                    phase.as_deref(),
                                ) {
                                    // Fire-and-forget: receiver may be dropped during shutdown.
                                    let _ =
                                        events.send(TurnEvent::ThoughtDelta(trimmed.to_string()));

                                    continue;
                                }
                            }
                            AppServerStreamEvent::ProgressUpdate(progress) => {
                                let trimmed = progress.trim();
                                if trimmed.is_empty() {
                                    continue;
                                }

                                // Fire-and-forget: receiver may be dropped during shutdown.
                                let _ = events.send(TurnEvent::ThoughtDelta(trimmed.to_string()));
                            }
                        }
                    }
                })
            };

            let turn_result = client.run_turn(request, stream_tx).await;
            // Task join: panic in the spawned task is not recoverable here.
            let _ = bridge_handle.await;

            match turn_result {
                Ok(response) => {
                    // Fire-and-forget: receiver may be dropped during shutdown.
                    let _ = events.send(TurnEvent::PidUpdate(response.pid));
                    let assistant_message = agent::parse_turn_response(
                        kind,
                        &response.assistant_message,
                        protocol_profile,
                    )
                    .map_err(AgentError::Backend)?;

                    Ok(TurnResult {
                        assistant_message,
                        context_reset: response.context_reset,
                        input_tokens: response.input_tokens,
                        output_tokens: response.output_tokens,
                        provider_conversation_id: response.provider_conversation_id,
                    })
                }
                Err(error) => Err(AgentError::AppServer(error)),
            }
        })
    }

    /// Shuts down the underlying app-server session.
    fn shutdown_session(&self, session_id: String) -> AgentFuture<Result<(), AgentError>> {
        let client = Arc::clone(&self.client);

        Box::pin(async move {
            client.shutdown_session(session_id).await;

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::infra::app_server::{AppServerTurnResponse, MockAppServerClient};
    use crate::infra::channel::{AgentRequestKind, TurnPromptAttachment};

    fn make_turn_request() -> TurnRequest {
        TurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "gemini-3-flash-preview".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "Do something".into(),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
        }
    }

    fn make_ok_response(assistant_message: &str) -> AppServerTurnResponse {
        AppServerTurnResponse {
            assistant_message: assistant_message.to_string(),
            context_reset: false,
            input_tokens: 10,
            output_tokens: 5,
            pid: None,
            provider_conversation_id: None,
        }
    }

    #[tokio::test]
    /// Verifies non-thought assistant deltas are withheld from the unified
    /// event stream so transcript output is only appended from the final turn
    /// result.
    async fn test_run_turn_suppresses_non_thought_assistant_delta_streaming() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: "Hello world".to_string(),
                    phase: None,
                    is_delta: true,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Hello world","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        assert!(result.is_ok());
        let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| matches!(event, TurnEvent::PidUpdate(_))),
            "only pid events should be emitted, got: {events:?}"
        );
    }

    #[tokio::test]
    /// Verifies completed assistant chunks are also withheld from the unified
    /// event stream so the transcript only changes when the turn completes.
    async fn test_run_turn_suppresses_non_delta_assistant_messages() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: "Full paragraph   ".to_string(),
                    phase: None,
                    is_delta: false,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Full paragraph","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        assert!(result.is_ok());
        let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| matches!(event, TurnEvent::PidUpdate(_))),
            "only pid events should be emitted, got: {events:?}"
        );
    }

    #[tokio::test]
    /// Verifies structured assistant payload chunks are not emitted as live
    /// transcript output.
    async fn test_run_turn_suppresses_non_delta_structured_json_streaming() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}],"summary":null}"#.to_string(),
                    phase: None,
                    is_delta: false,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        assert!(result.is_ok());
        let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| matches!(event, TurnEvent::PidUpdate(_))),
            "only pid events should be emitted, got: {events:?}"
        );
    }

    #[tokio::test]
    /// Verifies Codex thought-phase deltas are routed to `ThoughtDelta`.
    async fn test_run_turn_routes_codex_thinking_delta_to_thought_event() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: "Inspecting files".to_string(),
                    phase: Some("thinking".to_string()),
                    is_delta: true,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Done.","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        assert!(result.is_ok());
        let event = events_rx.try_recv().expect("should have received an event");
        assert_eq!(
            event,
            TurnEvent::ThoughtDelta("Inspecting files".to_string())
        );
    }

    #[tokio::test]
    /// Verifies Codex thought-phase matching is case-insensitive for streamed
    /// thought routing.
    async fn test_run_turn_routes_uppercase_codex_thinking_delta_to_thought_event() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: "Inspecting files".to_string(),
                    phase: Some("Thinking".to_string()),
                    is_delta: true,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Done.","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        assert!(result.is_ok());
        let event = events_rx.try_recv().expect("should have received an event");
        assert_eq!(
            event,
            TurnEvent::ThoughtDelta("Inspecting files".to_string())
        );
    }

    #[tokio::test]
    /// Verifies `ProgressUpdate` events drive the transient thinking loader.
    async fn test_run_turn_routes_progress_update_events_to_thought_delta() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                    "Running tool".to_string(),
                ));

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        assert!(result.is_ok());
        let event = events_rx
            .try_recv()
            .expect("should have received a progress event");
        assert_eq!(event, TurnEvent::ThoughtDelta("Running tool".to_string()));
    }

    #[tokio::test]
    /// Verifies strict session-turn responses synthesize an empty summary when
    /// the provider returns `summary: null`.
    async fn test_run_turn_fills_missing_summary_for_session_turn() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, _stream_tx| {
                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Done.","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Gemini);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(
            result.assistant_message.summary,
            Some(agent::protocol::AgentResponseSummary {
                turn: String::new(),
                session: String::new(),
            })
        );
    }

    #[tokio::test]
    /// Verifies whitespace-only `AssistantMessage` does not emit a thinking
    /// update.
    async fn test_run_turn_skips_whitespace_only_assistant_messages() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: "   \n  ".to_string(),
                    phase: None,
                    is_delta: true,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        assert!(result.is_ok());
        while let Ok(event) = events_rx.try_recv() {
            assert!(
                !matches!(event, TurnEvent::ThoughtDelta(_)),
                "no ThoughtDelta should be emitted for whitespace-only messages, got: {event:?}"
            );
        }
    }

    #[tokio::test]
    /// Verifies delta protocol JSON fragments do not emit transient loader
    /// updates.
    async fn test_run_turn_skips_delta_protocol_json_fragments() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: r#"{"answer":"#.to_string(),
                    phase: None,
                    is_delta: true,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Final answer.","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(result.assistant_message.to_display_text(), "Final answer.");
        while let Ok(event) = events_rx.try_recv() {
            assert!(
                !matches!(event, TurnEvent::ThoughtDelta(_)),
                "no ThoughtDelta should be emitted for protocol fragments, got: {event:?}"
            );
        }
    }

    #[tokio::test]
    /// Verifies strict providers suppress streamed assistant chunks and rely on
    /// the final parsed payload.
    async fn test_run_turn_gemini_suppresses_streamed_assistant_messages() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: "streamed plain text".to_string(),
                    phase: None,
                    is_delta: true,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Final structured output.","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Gemini);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(
            result.assistant_message.to_display_text(),
            "Final structured output."
        );
        while let Ok(event) = events_rx.try_recv() {
            assert!(
                !matches!(event, TurnEvent::ThoughtDelta(_)),
                "no ThoughtDelta should be emitted for strict providers, got: {event:?}"
            );
        }
    }

    #[tokio::test]
    /// Verifies Gemini turns surface invalid structured output instead of
    /// starting a repair retry.
    async fn test_run_turn_returns_error_for_invalid_structured_output_for_gemini() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .times(1)
            .returning(|request, _stream_tx| {
                assert_eq!(request.prompt, "Do something");
                assert_eq!(request.request_kind, AgentRequestKind::SessionStart);

                Box::pin(async { Ok(make_ok_response("plain non-json response")) })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Gemini);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let error = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect_err("invalid structured output should fail");

        // Assert
        let error_message = error.to_string();
        assert!(error_message.contains("did not match the required JSON schema"));
        assert!(error_message.contains("response:\nplain non-json response"));
    }

    #[tokio::test]
    /// Verifies Gemini turns pass pasted image prompt payloads through to the
    /// underlying app-server client.
    async fn test_run_turn_allows_image_attachments_for_gemini() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .times(1)
            .returning(|request, _stream_tx| {
                assert_eq!(request.prompt.attachments.len(), 1);

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"gemini ok","questions":[],"summary":null}"#,
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Gemini);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let mut request = make_turn_request();
        request.prompt.attachments.push(TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/image.png"),
        });

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), request, events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(result.assistant_message.to_display_text(), "gemini ok");
    }

    #[tokio::test]
    /// Verifies Codex turns surface invalid plain-text output instead of
    /// accepting it as a final response.
    async fn test_run_turn_codex_rejects_plain_text_without_repair_retry() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .times(1)
            .returning(|_request, _stream_tx| Box::pin(async { Ok(make_ok_response("plain")) }));
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let error = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect_err("plain-text turn should fail");

        // Assert
        let error_message = error.to_string();
        assert!(error_message.contains("did not match the required JSON schema"));
        assert!(error_message.contains("response:\nplain"));
    }

    #[tokio::test]
    /// Verifies client turn failure propagates as `Err(AgentError)`.
    async fn test_run_turn_client_failure_returns_agent_error() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, _stream_tx| {
                Box::pin(async {
                    Err(crate::infra::app_server::AppServerError::Provider(
                        "server timeout".to_string(),
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        let error_message = result
            .expect_err("expected Err on server timeout")
            .to_string();
        assert!(error_message.contains("server timeout"));
    }

    #[tokio::test]
    /// Verifies `TurnResult` carries the correct token counts and context-reset
    /// flag from the underlying `AppServerTurnResponse`.
    async fn test_run_turn_returns_correct_token_counts_and_context_reset() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, _stream_tx| {
                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"Result","questions":[],"summary":null}"#
                            .to_string(),
                        context_reset: true,
                        input_tokens: 100,
                        output_tokens: 50,
                        pid: Some(1234),
                        provider_conversation_id: None,
                    })
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(result.assistant_message.to_display_text(), "Result");
        assert!(result.context_reset);
        assert_eq!(result.input_tokens, 100);
        assert_eq!(result.output_tokens, 50);
    }

    #[tokio::test]
    /// Verifies `provider_conversation_id` is forwarded from `TurnRequest` to
    /// the underlying `AppServerTurnRequest` and propagated back from the
    /// response into the returned `TurnResult`.
    async fn test_run_turn_passes_and_returns_provider_conversation_id() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|request, _stream_tx| {
                assert_eq!(
                    request.provider_conversation_id,
                    Some("thread-abc".to_string()),
                    "request should carry the provider conversation id"
                );
                assert_eq!(
                    request.reasoning_level,
                    ReasoningLevel::Medium,
                    "request should carry the codex reasoning level"
                );

                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"ok","questions":[],"summary":null}"#
                            .to_string(),
                        context_reset: false,
                        input_tokens: 1,
                        output_tokens: 1,
                        pid: Some(42),
                        provider_conversation_id: Some("thread-xyz".to_string()),
                    })
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let mut request = make_turn_request();
        request.reasoning_level = ReasoningLevel::Medium;
        request.provider_conversation_id = Some("thread-abc".to_string());

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), request, events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(
            result.provider_conversation_id,
            Some("thread-xyz".to_string()),
            "result should carry the provider conversation id from the response"
        );

        // Verify PID event was emitted from the response.
        let mut pid_event_seen = false;
        while let Ok(event) = events_rx.try_recv() {
            if matches!(event, TurnEvent::PidUpdate(Some(42))) {
                pid_event_seen = true;
            }
        }
        assert!(pid_event_seen, "should emit PidUpdate from response pid");
    }
}
