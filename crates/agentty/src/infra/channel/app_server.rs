//! App-server RPC [`AgentChannel`] adapter.
//!
//! Delegates turn execution to [`AppServerClient`] and bridges
//! [`AppServerStreamEvent`]s to the unified [`TurnEvent`] stream.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::domain::agent::AgentKind;
use crate::infra::agent;
use crate::infra::agent::protocol::{
    build_protocol_repair_prompt, normalize_stream_assistant_chunk, parse_agent_response,
    parse_agent_response_strict,
};
use crate::infra::app_server::{
    AppServerClient, AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnMode,
    TurnRequest, TurnResult,
};

/// Maximum number of repair turns for strict structured-protocol providers.
const MAX_STRUCTURED_OUTPUT_REPAIR_ATTEMPTS: usize = 3;

/// [`AgentChannel`] adapter backed by a persistent app-server session.
///
/// Turn execution is delegated to [`AppServerClient::run_turn`].
/// [`AppServerStreamEvent`]s emitted by the provider are bridged to
/// [`TurnEvent::AssistantDelta`] and [`TurnEvent::Progress`] as they arrive.
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
    /// [`AppServerStreamEvent::AssistantMessage`] events are routed to either
    /// [`TurnEvent::AssistantDelta`] or [`TurnEvent::ThoughtDelta`] for
    /// non-strict providers. Strict providers suppress assistant stream chunks
    /// and rely on final payload parsing to avoid leaking malformed first-pass
    /// output when a repair retry is needed.
    ///
    /// Codex thought-style deltas (`phase: thinking/plan`) are emitted as
    /// [`TurnEvent::ThoughtDelta`]. Other assistant chunks are normalized via
    /// [`normalize_stream_assistant_chunk`] and emitted as
    /// [`TurnEvent::AssistantDelta`].
    ///
    /// [`AppServerStreamEvent::ProgressUpdate`] events are forwarded as
    /// [`TurnEvent::Progress`].
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
        let should_stream_assistant_messages = !requires_strict_structured_output(kind);

        Box::pin(async move {
            let session_output = match req.mode {
                TurnMode::Start => None,
                TurnMode::Resume { session_output } => session_output,
            };

            let request = AppServerTurnRequest {
                reasoning_level: req.reasoning_level,
                live_session_output: req.live_session_output,
                folder: req.folder,
                model: req.model,
                prompt: req.prompt,
                provider_conversation_id: req.provider_conversation_id,
                session_id,
                session_output,
            };
            let request_for_repair = request.clone();

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
                                if !should_stream_assistant_messages {
                                    continue;
                                }

                                let trimmed = message.trim_end();
                                if trimmed.trim().is_empty() {
                                    continue;
                                }

                                if is_streamed_thought_message(kind, is_delta, phase.as_deref()) {
                                    let _ =
                                        events.send(TurnEvent::ThoughtDelta(trimmed.to_string()));

                                    continue;
                                }

                                let formatted = if is_delta {
                                    normalize_delta_assistant_message(trimmed)
                                } else {
                                    format_non_delta_assistant_message(trimmed)
                                };
                                let Some(formatted) = formatted else {
                                    continue;
                                };
                                if formatted.trim().is_empty() {
                                    continue;
                                }

                                let _ = events.send(TurnEvent::AssistantDelta(formatted));
                            }
                            AppServerStreamEvent::ProgressUpdate(progress) => {
                                let _ = events.send(TurnEvent::Progress(progress));
                            }
                        }
                    }
                })
            };

            let turn_result = client.run_turn(request, stream_tx).await;
            let _ = bridge_handle.await;

            match turn_result {
                Ok(response) => {
                    let _ = events.send(TurnEvent::PidUpdate(response.pid));
                    let assistant_message = parse_or_repair_structured_response(
                        &client,
                        kind,
                        &request_for_repair,
                        &response,
                    )
                    .await?;

                    Ok(TurnResult {
                        assistant_message,
                        context_reset: response.context_reset,
                        input_tokens: response.input_tokens,
                        output_tokens: response.output_tokens,
                        provider_conversation_id: response.provider_conversation_id,
                    })
                }
                Err(error) => Err(AgentError(error)),
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

/// Normalizes one streamed delta assistant message.
///
/// For protocol JSON fragments, returns [`None`] so partial JSON is not
/// appended to session output.
fn normalize_delta_assistant_message(message: &str) -> Option<String> {
    normalize_stream_assistant_chunk(message)
}

/// Normalizes one complete non-delta assistant message.
///
/// The app-server emits non-delta assistant messages as complete chunks. This
/// helper parses protocol wrappers and returns only `answer` display text,
/// preserving paragraph spacing.
///
/// Returns [`None`] when the payload is only a suppressed protocol fragment.
fn format_non_delta_assistant_message(message: &str) -> Option<String> {
    let normalized = normalize_stream_assistant_chunk(message)?;
    let normalized = normalized.trim_end();

    Some(format!("{normalized}\n\n"))
}

/// Returns whether one streamed assistant chunk should be treated as thought
/// text.
///
/// Codex app-server emits thought/planning deltas with `phase` values such as
/// `thinking` and `plan`. These are surfaced as [`TurnEvent::ThoughtDelta`] so
/// worker transcript output only contains final assistant answers.
fn is_streamed_thought_message(kind: AgentKind, is_delta: bool, phase: Option<&str>) -> bool {
    if kind != AgentKind::Codex || !is_delta {
        return false;
    }

    phase.is_some_and(is_codex_thought_phase_label)
}

/// Returns whether one Codex phase label denotes thought/planning text.
///
/// Phase matching is case-insensitive so provider variants such as `Thinking`
/// and `PLAN` continue to route to [`TurnEvent::ThoughtDelta`].
fn is_codex_thought_phase_label(phase: &str) -> bool {
    let normalized_phase = phase.trim();

    normalized_phase.eq_ignore_ascii_case("thinking")
        || normalized_phase.eq_ignore_ascii_case("plan")
        || normalized_phase.eq_ignore_ascii_case("reasoning")
        || normalized_phase.eq_ignore_ascii_case("thought")
}

/// Parses one final assistant payload, optionally repairing malformed
/// structured output for strict providers.
async fn parse_or_repair_structured_response(
    client: &Arc<dyn AppServerClient>,
    kind: AgentKind,
    original_request: &AppServerTurnRequest,
    response: &AppServerTurnResponse,
) -> Result<agent::AgentResponse, AgentError> {
    if !requires_strict_structured_output(kind) {
        return Ok(parse_agent_response(&response.assistant_message));
    }

    if let Ok(parsed_response) = parse_agent_response_strict(&response.assistant_message) {
        return Ok(parsed_response);
    }

    let mut last_response_text = response.assistant_message.clone();
    let mut provider_conversation_id = response.provider_conversation_id.clone();
    let mut last_error = None;

    for _attempt in 0..MAX_STRUCTURED_OUTPUT_REPAIR_ATTEMPTS {
        let repair_request = build_repair_request(
            original_request,
            &last_response_text,
            provider_conversation_id.as_deref(),
        );
        let (repair_stream_tx, _repair_stream_rx) = mpsc::unbounded_channel();
        let repair_response = client
            .run_turn(repair_request, repair_stream_tx)
            .await
            .map_err(AgentError)?;
        provider_conversation_id = repair_response.provider_conversation_id;
        last_response_text = repair_response.assistant_message;

        match parse_agent_response_strict(&last_response_text) {
            Ok(parsed_response) => return Ok(parsed_response),
            Err(error) => last_error = Some(error),
        }
    }

    let parse_error =
        last_error.unwrap_or(crate::infra::agent::protocol::AgentResponseParseError::InvalidFormat);

    Err(AgentError(format!(
        "Agent output did not match the required JSON schema after \
         {MAX_STRUCTURED_OUTPUT_REPAIR_ATTEMPTS} repair attempts: {parse_error}"
    )))
}

/// Returns whether the provider should fail closed on invalid structured
/// output instead of falling back to plain text.
fn requires_strict_structured_output(kind: AgentKind) -> bool {
    matches!(kind, AgentKind::Claude | AgentKind::Gemini)
}

/// Builds one app-server repair turn request from the original request.
fn build_repair_request(
    original_request: &AppServerTurnRequest,
    assistant_message: &str,
    provider_conversation_id: Option<&str>,
) -> AppServerTurnRequest {
    AppServerTurnRequest {
        reasoning_level: original_request.reasoning_level,
        folder: original_request.folder.clone(),
        live_session_output: None,
        model: original_request.model.clone(),
        prompt: build_protocol_repair_prompt(assistant_message),
        provider_conversation_id: provider_conversation_id
            .map(ToString::to_string)
            .or_else(|| original_request.provider_conversation_id.clone()),
        session_id: original_request.session_id.clone(),
        session_output: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::mpsc;

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::infra::app_server::{AppServerTurnResponse, MockAppServerClient};
    use crate::infra::channel::TurnMode;

    fn make_turn_request() -> TurnRequest {
        TurnRequest {
            reasoning_level: ReasoningLevel::default(),
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "gemini-3-flash-preview".to_string(),
            mode: TurnMode::Start,
            prompt: "Do something".to_string(),
            provider_conversation_id: None,
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
    /// Verifies `AssistantMessage` with `is_delta: true` is forwarded as an
    /// `AssistantDelta` event with the message text unchanged.
    async fn test_run_turn_bridges_delta_assistant_message_unchanged() {
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

                Box::pin(async { Ok(make_ok_response("Hello world")) })
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
        assert_eq!(event, TurnEvent::AssistantDelta("Hello world".to_string()));
    }

    #[tokio::test]
    /// Verifies Codex non-delta assistant chunks are forwarded as
    /// `AssistantDelta` events.
    async fn test_run_turn_codex_forwards_non_delta_assistant_messages() {
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

                Box::pin(async { Ok(make_ok_response("Full paragraph")) })
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
            TurnEvent::AssistantDelta("Full paragraph\n\n".to_string())
        );
    }

    #[tokio::test]
    /// Verifies Codex non-delta structured JSON chunks are normalized and
    /// forwarded as `AssistantDelta`.
    async fn test_run_turn_codex_forwards_non_delta_structured_json_streaming() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: r#"{"messages":[{"type":"answer","text":"Done."},{"type":"question","text":"Need clarification."}]}"#.to_string(),
                    phase: None,
                    is_delta: false,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"messages":[{"type":"answer","text":"Done."},{"type":"question","text":"Need clarification."}]}"#,
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
        assert_eq!(event, TurnEvent::AssistantDelta("Done.\n\n".to_string()));
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
                        r#"{"messages":[{"type":"answer","text":"Done."}]}"#,
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
    /// delta routing.
    async fn test_run_turn_routes_codex_thinking_delta_with_uppercase_phase_to_thought_event() {
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
                        r#"{"messages":[{"type":"answer","text":"Done."}]}"#,
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
    /// Verifies `ProgressUpdate` events are forwarded as `Progress` events.
    async fn test_run_turn_bridges_progress_update_as_progress_event() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                    "Running tool".to_string(),
                ));

                Box::pin(async { Ok(make_ok_response("")) })
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
        assert_eq!(event, TurnEvent::Progress("Running tool".to_string()));
    }

    #[tokio::test]
    /// Verifies whitespace-only `AssistantMessage` is not forwarded as an
    /// `AssistantDelta` event.
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

                Box::pin(async { Ok(make_ok_response("")) })
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
                !matches!(event, TurnEvent::AssistantDelta(_)),
                "no AssistantDelta should be emitted for whitespace-only messages, got: {event:?}"
            );
        }
    }

    #[tokio::test]
    /// Verifies delta protocol JSON fragments are not forwarded as
    /// `AssistantDelta` events.
    async fn test_run_turn_skips_delta_protocol_json_fragments() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: r#"{"messages":[{"type":"answer","#.to_string(),
                    phase: None,
                    is_delta: true,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"messages":[{"type":"answer","text":"Final answer."}]}"#,
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
                !matches!(event, TurnEvent::AssistantDelta(_)),
                "no AssistantDelta should be emitted for protocol fragments, got: {event:?}"
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
                        r#"{"messages":[{"type":"answer","text":"Final structured output."}]}"#,
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
                !matches!(event, TurnEvent::AssistantDelta(_)),
                "no AssistantDelta should be emitted for strict providers, got: {event:?}"
            );
        }
    }

    #[tokio::test]
    /// Verifies Gemini turns run one repair retry when final output is not
    /// valid structured JSON.
    async fn test_run_turn_repairs_invalid_structured_output_for_gemini() {
        // Arrange
        let call_count = Arc::new(AtomicUsize::new(0));
        let mut mock_client = MockAppServerClient::new();
        mock_client.expect_run_turn().times(2).returning({
            let call_count = Arc::clone(&call_count);
            move |request, _stream_tx| {
                let call_index = call_count.fetch_add(1, Ordering::SeqCst);
                if call_index == 0 {
                    assert_eq!(request.prompt, "Do something");

                    Box::pin(async { Ok(make_ok_response("plain non-json response")) })
                } else {
                    assert!(
                        request
                            .prompt
                            .contains("did not match the required JSON schema")
                    );
                    Box::pin(async {
                        Ok(make_ok_response(
                            r#"{"messages":[{"type":"answer","text":"Recovered output"}]}"#,
                        ))
                    })
                }
            }
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
            result.assistant_message.to_display_text(),
            "Recovered output"
        );
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    /// Verifies Codex turns do not run repair fallback when final output is
    /// plain text.
    async fn test_run_turn_codex_keeps_plain_text_without_repair_retry() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .times(1)
            .returning(|_request, _stream_tx| Box::pin(async { Ok(make_ok_response("plain")) }));
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(result.assistant_message.to_display_text(), "plain");
    }

    #[tokio::test]
    /// Verifies client turn failure propagates as `Err(AgentError)`.
    async fn test_run_turn_client_failure_returns_agent_error() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, _stream_tx| {
                Box::pin(async { Err("server timeout".to_string()) })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await;

        // Assert
        let error_message = result.expect_err("expected Err on server timeout").0;
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
                        assistant_message: "Result".to_string(),
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
                        assistant_message: "ok".to_string(),
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
