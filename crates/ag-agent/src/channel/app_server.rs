//! App-server RPC [`AgentChannel`] adapter.
//!
//! Delegates turn execution to [`AppServerClient`] and bridges
//! [`AppServerStreamEvent`]s to the unified [`TurnEvent`] stream.

use std::sync::Arc;

use ag_protocol::{
    AgentResponse, ProtocolRequestProfile, build_protocol_repair_prompt_for_profile,
};
use tokio::sync::mpsc;

use crate::agent;
use crate::app_server::{AppServerClient, AppServerStreamEvent, AppServerTurnRequest};
use crate::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};
use crate::model::agent::AgentKind;

/// [`AgentChannel`] adapter backed by a persistent app-server session.
///
/// Turn execution is delegated to [`AppServerClient::run_turn`].
/// [`AppServerStreamEvent`]s emitted by the provider are bridged to
/// [`TurnEvent::ThoughtDelta`] values when transient loader text should be
/// updated.
pub(crate) struct AppServerAgentChannel {
    /// Provider-specific app-server client.
    client: Arc<dyn AppServerClient>,
    /// Provider kind routed through this channel instance.
    kind: AgentKind,
}

impl AppServerAgentChannel {
    /// Creates a new app-server channel backed by the given client.
    pub(crate) fn new(client: Arc<dyn AppServerClient>, kind: AgentKind) -> Self {
        Self { client, kind }
    }

    /// Bridges normal-turn PID and transient loader updates until the runtime
    /// drops its stream sender.
    fn bridge_turn_stream(
        kind: AgentKind,
        mut stream_rx: mpsc::UnboundedReceiver<AppServerStreamEvent>,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = stream_rx.recv().await {
                match event {
                    AppServerStreamEvent::PidUpdate(pid) => {
                        let _ = events.send(TurnEvent::PidUpdate(pid));
                    }
                    AppServerStreamEvent::AssistantMessage {
                        message,
                        phase,
                        is_delta,
                    } => {
                        let trimmed = message.trim_end();
                        if trimmed.trim().is_empty() {
                            continue;
                        }

                        if agent::is_app_server_thought_chunk(kind, is_delta, phase.as_deref()) {
                            // Fire-and-forget: receiver may be dropped during shutdown.
                            let _ = events.send(TurnEvent::ThoughtDelta(trimmed.to_string()));
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
    /// Every terminal error clears the tracked PID, including failed repair.
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
        let error_events = events.clone();
        let turn = async move {
            let mut req = req;
            req.prompt = agent::apply_response_style_prompt(
                req.prompt,
                req.request_kind.protocol_profile(),
                req.response_style,
            )
            .map_err(|error| AgentError::Backend(error.to_string()))?;
            let continuation = req.continuation.into_parts();
            let request = AppServerTurnRequest {
                folder: req.folder,
                live_transcript: continuation.live_transcript,
                main_checkout_root: req.main_checkout_root,
                model: req.model,
                permission_mode: req.permission_mode,
                personality: req.personality,
                prompt: req.prompt,
                request_kind: req.request_kind,
                replay_transcript: continuation.replay_transcript,
                provider_conversation_id: continuation.provider_conversation_id,
                persisted_instruction_conversation_id: continuation
                    .persisted_instruction_conversation_id,
                reasoning_level: req.reasoning_level,
                session_id,
                speed_mode: req.speed_mode,
            };
            let protocol_profile = request.request_kind.protocol_profile();
            let repair_request = request.clone();
            let (stream_tx, stream_rx) = mpsc::unbounded_channel::<AppServerStreamEvent>();

            let bridge_handle = Self::bridge_turn_stream(kind, stream_rx, events.clone());

            let turn_result = client.run_turn(request, stream_tx).await;
            // Task join: panic in the spawned task is not recoverable here.
            let _ = bridge_handle.await;

            match turn_result {
                Ok(response) => {
                    // Fire-and-forget: receiver may be dropped during shutdown.
                    let _ = events.send(TurnEvent::PidUpdate(response.pid));
                    let parsed = parse_or_repair_app_server_response(
                        kind,
                        &response,
                        protocol_profile,
                        repair_request,
                        &client,
                        &events,
                    )
                    .await?;

                    Ok(TurnResult {
                        assistant_message: parsed.assistant_message,
                        context_reset: response.context_reset,
                        input_tokens: response.input_tokens + parsed.repair_input_tokens,
                        output_tokens: response.output_tokens + parsed.repair_output_tokens,
                        provider_conversation_id: parsed.provider_conversation_id,
                    })
                }
                Err(error) => Err(AgentError::AppServer(error)),
            }
        };

        Box::pin(async move {
            let result = turn.await;
            if result.is_err() {
                let _ = error_events.send(TurnEvent::PidUpdate(None));
            }

            result
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

/// Aggregated result from parsing an app-server turn response, including
/// metadata from a repair turn when one was needed.
struct AppServerParsedTurnResult {
    /// Parsed agent response from the successful attempt.
    assistant_message: AgentResponse,
    /// Provider conversation id from the latest successful attempt,
    /// falling back to the original response when the repair turn does
    /// not produce one.
    provider_conversation_id: Option<String>,
    /// Additional input tokens consumed by a repair turn (zero when no
    /// repair was needed).
    repair_input_tokens: u64,
    /// Additional output tokens consumed by a repair turn (zero when no
    /// repair was needed).
    repair_output_tokens: u64,
}

/// Parses one app-server turn response strictly, falling back to a single
/// protocol-repair retry when the initial parse fails.
///
/// The repair prompt is sent as a follow-up turn on the same session so the
/// agent retains the original conversation context. When repair succeeds,
/// the returned metadata reflects the repair turn's provider conversation id
/// and token usage so the caller can propagate them correctly.
///
/// When repair is attempted, a concise [`TurnEvent::ThoughtDelta`] is emitted
/// so the user can see that schema repair is in progress. The parse error is
/// deliberately excluded: thought updates render as live loader lines, and the
/// error carries provider diagnostics that must not reach the UI.
/// Repair streams forward PID changes while withholding provider diagnostics;
/// the final repair response replaces the tracked PID before parsing its
/// output.
async fn parse_or_repair_app_server_response(
    kind: AgentKind,
    response: &crate::app_server::AppServerTurnResponse,
    protocol_profile: ProtocolRequestProfile,
    repair_request: AppServerTurnRequest,
    client: &Arc<dyn AppServerClient>,
    events: &mpsc::UnboundedSender<TurnEvent>,
) -> Result<AppServerParsedTurnResult, AgentError> {
    let parse_error =
        match agent::parse_turn_response(kind, &response.assistant_message, protocol_profile) {
            Ok(parsed) => {
                return Ok(AppServerParsedTurnResult {
                    assistant_message: parsed,
                    provider_conversation_id: response.provider_conversation_id.clone(),
                    repair_input_tokens: 0,
                    repair_output_tokens: 0,
                });
            }
            Err(error) => error,
        };

    let _ = events.send(TurnEvent::ThoughtDelta(format!(
        "Protocol parse error; retrying schema repair for {kind}."
    )));

    let repair_prompt = build_protocol_repair_prompt_for_profile(
        protocol_profile,
        &parse_error,
        &response.assistant_message,
    );

    let repair_provider_conversation_id = response
        .provider_conversation_id
        .clone()
        .or_else(|| repair_request.provider_conversation_id.clone());

    let repair_turn_request = AppServerTurnRequest {
        folder: repair_request.folder,
        live_transcript: None,
        main_checkout_root: repair_request.main_checkout_root,
        model: repair_request.model,
        permission_mode: repair_request.permission_mode,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: ag_protocol::TurnPrompt::from_agent_data(repair_prompt),
        request_kind: repair_request.request_kind,
        replay_transcript: None,
        provider_conversation_id: repair_provider_conversation_id,
        persisted_instruction_conversation_id: None,
        reasoning_level: repair_request.reasoning_level,
        session_id: repair_request.session_id,
        speed_mode: repair_request.speed_mode,
    };
    let (repair_stream_tx, mut repair_stream_rx) = mpsc::unbounded_channel();
    let repair_bridge = {
        let events = events.clone();

        tokio::spawn(async move {
            while let Some(event) = repair_stream_rx.recv().await {
                if let AppServerStreamEvent::PidUpdate(pid) = event {
                    let _ = events.send(TurnEvent::PidUpdate(pid));
                }
            }
        })
    };
    let repair_result = client.run_turn(repair_turn_request, repair_stream_tx).await;
    let _ = repair_bridge.await;
    let repair_result = repair_result.map_err(|error| {
        AgentError::Backend(format!(
            "{parse_error}\nprotocol repair transport failed: {error}"
        ))
    })?;
    let _ = events.send(TurnEvent::PidUpdate(repair_result.pid));

    let parsed =
        agent::parse_turn_response(kind, &repair_result.assistant_message, protocol_profile)
            .map_err(|error| {
                AgentError::Backend(format!(
                    "{parse_error}\nprotocol repair retry also failed: {error}"
                ))
            })?;

    Ok(AppServerParsedTurnResult {
        assistant_message: parsed,
        provider_conversation_id: repair_result
            .provider_conversation_id
            .or(response.provider_conversation_id.clone()),
        repair_input_tokens: repair_result.input_tokens,
        repair_output_tokens: repair_result.output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use ag_protocol::TurnPromptAttachment;
    use tokio::sync::mpsc;

    use super::*;
    use crate::app_server::{AppServerTurnResponse, MockAppServerClient};
    use crate::channel::AgentRequestKind;
    use crate::model::agent::ReasoningLevel;

    fn make_turn_request() -> TurnRequest {
        TurnRequest {
            continuation: crate::channel::TurnContinuation::fresh(),
            folder: PathBuf::from("/tmp"),
            main_checkout_root: Some(PathBuf::from("/tmp/main")),
            model: "gpt-5.6-sol".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do something".into(),
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            response_style: crate::ResponseStyle::default(),
            speed_mode: crate::model::session::SpeedMode::default(),
        }
    }

    #[tokio::test]
    async fn forwards_runtime_pid_and_clears_it_after_non_retained_turn() {
        // Arrange
        let mut client = MockAppServerClient::new();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let mut finish_rx = Some(finish_rx);
        client
            .expect_run_turn()
            .times(1)
            .returning(move |_, stream_tx| {
                let finish_rx = finish_rx.take().expect("single turn");
                Box::pin(async move {
                    let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(123)));
                    finish_rx.await.expect("release turn");

                    Ok(make_ok_response(r#"{"answer":"ok","questions":[]}"#))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(client), AgentKind::Gemini);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let turn = tokio::spawn(async move {
            channel
                .run_turn("session".to_string(), make_turn_request(), events_tx)
                .await
        });
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events_rx.recv())
            .await
            .expect("PID arrives during turn");

        // Assert
        assert!(matches!(event, Some(TurnEvent::PidUpdate(Some(123)))));
        assert!(!turn.is_finished());
        finish_tx.send(()).expect("finish turn");
        turn.await.expect("join turn").expect("successful turn");
        assert!(matches!(
            events_rx.recv().await,
            Some(TurnEvent::PidUpdate(None))
        ));
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

    fn collect_pid_updates(events: &mut mpsc::UnboundedReceiver<TurnEvent>) -> Vec<Option<u32>> {
        let mut pids = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let TurnEvent::PidUpdate(pid) = event {
                pids.push(pid);
            }
        }

        pids
    }

    #[tokio::test]
    async fn repair_forwards_live_pid_and_publishes_retained_or_cleared_response_pid() {
        for final_pid in [Some(456), None] {
            // Arrange
            let initial_pid = final_pid.map(|_| 123);
            let mut client = MockAppServerClient::new();
            let mut sequence = mockall::Sequence::new();
            client
                .expect_run_turn()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(move |_, _| {
                    Box::pin(async move {
                        let mut response = make_ok_response("invalid original response");
                        response.pid = initial_pid;

                        Ok(response)
                    })
                });
            let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
            let mut finish_rx = Some(finish_rx);
            client
                .expect_run_turn()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(move |_, stream_tx| {
                    let finish_rx = finish_rx.take().expect("single repair turn");
                    Box::pin(async move {
                        let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(456)));
                        let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                            "private repair diagnostics".to_string(),
                        ));
                        finish_rx.await.expect("release repair");
                        let mut response =
                            make_ok_response(r#"{"answer":"repaired","questions":[]}"#);
                        response.pid = final_pid;

                        Ok(response)
                    })
                });
            let channel = AppServerAgentChannel::new(Arc::new(client), AgentKind::Codex);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();

            // Act
            let turn = tokio::spawn(async move {
                channel
                    .run_turn("session".to_string(), make_turn_request(), events_tx)
                    .await
            });
            let mut pids = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while let Some(event) = events_rx.recv().await {
                    if let TurnEvent::PidUpdate(pid) = event {
                        pids.push(pid);
                        if pid == Some(456) {
                            break;
                        }
                    }
                }
            })
            .await
            .expect("repair PID arrives while turn is running");

            // Assert
            assert_eq!(pids, vec![initial_pid, Some(456)]);
            assert!(!turn.is_finished());
            finish_tx.send(()).expect("finish repair");
            let result = turn
                .await
                .expect("join turn")
                .expect("repaired turn succeeds");
            assert_eq!(result.assistant_message.to_display_text(), "repaired");
            assert!(
                matches!(events_rx.recv().await, Some(TurnEvent::PidUpdate(pid)) if pid == final_pid)
            );
            assert!(events_rx.recv().await.is_none());
        }
    }

    #[tokio::test]
    async fn repair_transport_failure_clears_latest_runtime_pid() {
        // Arrange
        let mut client = MockAppServerClient::new();
        let mut sequence = mockall::Sequence::new();
        client
            .expect_run_turn()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| {
                Box::pin(async {
                    let mut response = make_ok_response("invalid original response");
                    response.pid = Some(123);

                    Ok(response)
                })
            });
        client
            .expect_run_turn()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, stream_tx| {
                Box::pin(async move {
                    let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(456)));

                    Err(crate::app_server::AppServerError::Provider(
                        "repair runtime failed".to_string(),
                    ))
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let error = channel
            .run_turn("session".to_string(), make_turn_request(), events_tx)
            .await
            .expect_err("repair transport fails");

        // Assert
        assert!(
            error
                .to_string()
                .contains("protocol repair transport failed")
        );
        assert_eq!(
            collect_pid_updates(&mut events_rx),
            vec![Some(123), Some(456), None]
        );
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
                        r#"{"answer":"Hello world","questions":[]}"#,
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
        assert_ne!(events, [] as [crate::channel::contract::TurnEvent; 0]);
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
                        r#"{"answer":"Full paragraph","questions":[]}"#,
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
        assert_ne!(events, [] as [crate::channel::contract::TurnEvent; 0]);
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
                    message: r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}]}"#.to_string(),
                    phase: None,
                    is_delta: false,
                });

                Box::pin(async {
                    Ok(make_ok_response(
                        r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}]}"#,
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
        assert_ne!(events, [] as [crate::channel::contract::TurnEvent; 0]);
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

                Box::pin(async { Ok(make_ok_response(r#"{"answer":"Done.","questions":[]}"#)) })
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

                Box::pin(async { Ok(make_ok_response(r#"{"answer":"Done.","questions":[]}"#)) })
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
    /// Verifies nonempty `ProgressUpdate` events drive the transient loader
    /// while blank updates leave it unchanged.
    async fn test_run_turn_routes_progress_update_events_to_thought_delta() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(" \n ".to_string()));
                let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                    "Running tool".to_string(),
                ));

                Box::pin(async { Ok(make_ok_response(r#"{"answer":"","questions":[]}"#)) })
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

                Box::pin(async { Ok(make_ok_response(r#"{"answer":"","questions":[]}"#)) })
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
                        r#"{"answer":"Final answer.","questions":[]}"#,
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
    /// Verifies app-server providers suppress streamed assistant chunks and
    /// rely on the final parsed payload.
    async fn test_run_turn_app_server_suppresses_streamed_assistant_messages() {
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
                        r#"{"answer":"Final structured output.","questions":[]}"#,
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
        assert_eq!(
            result.assistant_message.to_display_text(),
            "Final structured output."
        );
        while let Ok(event) = events_rx.try_recv() {
            assert!(
                !matches!(event, TurnEvent::ThoughtDelta(_)),
                "no ThoughtDelta should be emitted for plain assistant deltas, got: {event:?}"
            );
        }
    }

    #[tokio::test]
    /// Verifies app-server turns surface invalid structured output after both
    /// the original parse and the protocol-repair retry fail.
    async fn test_run_turn_returns_error_for_invalid_structured_output() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .times(2)
            .returning(|request, stream_tx| {
                assert_eq!(request.request_kind, AgentRequestKind::SessionStart);
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(42)));

                Box::pin(async {
                    let mut response = make_ok_response("plain non-json response");
                    response.pid = Some(42);

                    Ok(response)
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let error = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect_err("invalid structured output should fail");

        // Assert
        let error_message = error.to_string();
        assert!(error_message.contains("did not match the required JSON schema"));
        assert!(!error_message.contains("plain non-json response"));
        assert_eq!(collect_pid_updates(&mut events_rx).last(), Some(&None));
    }

    #[tokio::test]
    /// Verifies app-server turns recover valid output when the initial parse
    /// fails but the protocol-repair retry returns valid protocol JSON.
    async fn test_run_turn_recovers_valid_output_via_protocol_repair() {
        // Arrange
        let call_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut mock_client = MockAppServerClient::new();
        mock_client.expect_run_turn().times(2).returning({
            let counter = Arc::clone(&call_counter);

            move |_request, _stream_tx| {
                let call_number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                if call_number == 0 {
                    Box::pin(async { Ok(make_ok_response("plain non-json response")) })
                } else {
                    Box::pin(async {
                        Ok(make_ok_response(
                            r#"{"answer":"Repaired response","questions":[]}"#,
                        ))
                    })
                }
            }
        });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect("repair retry should succeed");

        // Assert
        assert_eq!(
            result.assistant_message.to_display_text(),
            "Repaired response"
        );
    }

    #[tokio::test]
    /// Verifies app-server turns pass pasted image prompt payloads through to
    /// the underlying app-server client.
    async fn test_run_turn_allows_image_attachments() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .times(1)
            .returning(|request, _stream_tx| {
                assert_eq!(request.prompt.attachments.len(), 1);

                Box::pin(async { Ok(make_ok_response(r#"{"answer":"codex ok","questions":[]}"#)) })
            });
        let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
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
        assert_eq!(result.assistant_message.to_display_text(), "codex ok");
    }

    #[tokio::test]
    /// Verifies Codex turns surface invalid plain-text output after both the
    /// original parse and the protocol-repair retry fail.
    async fn test_run_turn_codex_rejects_plain_text_after_repair_retry() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .times(2)
            .returning(|_request, _stream_tx| {
                Box::pin(async { Ok(make_ok_response("plain-text-payload")) })
            });
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
        assert!(!error_message.contains("plain-text-payload"));
    }

    #[tokio::test]
    /// Verifies client turn failure propagates as `Err(AgentError)`.
    async fn test_run_turn_client_failure_returns_agent_error() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(42)));
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(43)));

                Box::pin(async {
                    Err(crate::app_server::AppServerError::Provider(
                        "server timeout".to_string(),
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
        let error_message = result
            .expect_err("expected Err on server timeout")
            .to_string();
        assert!(error_message.contains("server timeout"));
        assert_eq!(
            collect_pid_updates(&mut events_rx),
            vec![Some(42), Some(43), None]
        );
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
                        assistant_message: r#"{"answer":"Result","questions":[]}"#.to_string(),
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
                        assistant_message: r#"{"answer":"ok","questions":[]}"#.to_string(),
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
        request.continuation = crate::channel::TurnContinuation::provider(
            None,
            None,
            Some("thread-abc".to_string()),
            None,
        );

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
