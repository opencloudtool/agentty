//! App-server RPC [`AgentChannel`] adapter.
//!
//! Delegates turn execution to [`AppServerClient`] and bridges
//! [`AppServerStreamEvent`]s to the unified [`TurnEvent`] stream.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::infra::app_server::{AppServerClient, AppServerStreamEvent, AppServerTurnRequest};
use crate::infra::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnMode,
    TurnRequest, TurnResult,
};

/// [`AgentChannel`] adapter backed by a persistent app-server session.
///
/// Turn execution is delegated to [`AppServerClient::run_turn`].
/// [`AppServerStreamEvent`]s emitted by the provider are bridged to
/// [`TurnEvent::AssistantDelta`] and [`TurnEvent::Progress`] as they arrive.
pub struct AppServerAgentChannel {
    /// Provider-specific app-server client.
    client: Arc<dyn AppServerClient>,
}

impl AppServerAgentChannel {
    /// Creates a new app-server channel backed by the given client.
    pub fn new(client: Arc<dyn AppServerClient>) -> Self {
        Self { client }
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
    /// [`AppServerStreamEvent::AssistantMessage`] events are forwarded as
    /// [`TurnEvent::AssistantDelta`] (formatting adjusted based on the
    /// `is_delta` flag). [`AppServerStreamEvent::ProgressUpdate`] events are
    /// forwarded as [`TurnEvent::Progress`].
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

        Box::pin(async move {
            let session_output = match req.mode {
                TurnMode::Start => None,
                TurnMode::Resume { session_output } => session_output,
            };

            let request = AppServerTurnRequest {
                live_session_output: req.live_session_output,
                folder: req.folder,
                model: req.model,
                prompt: req.prompt,
                provider_conversation_id: req.provider_conversation_id,
                session_id,
                session_output,
            };

            let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<AppServerStreamEvent>();

            let bridge_handle = {
                let events = events.clone();

                tokio::spawn(async move {
                    while let Some(event) = stream_rx.recv().await {
                        match event {
                            AppServerStreamEvent::AssistantMessage { message, is_delta } => {
                                let trimmed = message.trim_end();
                                if trimmed.trim().is_empty() {
                                    continue;
                                }

                                let formatted = if is_delta {
                                    message
                                } else {
                                    format!("{trimmed}\n\n")
                                };

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

                    Ok(TurnResult {
                        assistant_message: response.assistant_message,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::*;
    use crate::infra::app_server::{AppServerTurnResponse, MockAppServerClient};
    use crate::infra::channel::TurnMode;

    fn make_turn_request() -> TurnRequest {
        TurnRequest {
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
                    is_delta: true,
                });

                Box::pin(async { Ok(make_ok_response("Hello world")) })
            });
        let channel = AppServerAgentChannel {
            client: Arc::new(mock_client),
        };
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
    /// Verifies `AssistantMessage` with `is_delta: false` appends `\n\n` after
    /// trimmed text for paragraph spacing.
    async fn test_run_turn_bridges_non_delta_assistant_message_with_trailing_newlines() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    message: "Full paragraph   ".to_string(),
                    is_delta: false,
                });

                Box::pin(async { Ok(make_ok_response("Full paragraph")) })
            });
        let channel = AppServerAgentChannel {
            client: Arc::new(mock_client),
        };
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
        let channel = AppServerAgentChannel {
            client: Arc::new(mock_client),
        };
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
                    is_delta: true,
                });

                Box::pin(async { Ok(make_ok_response("")) })
            });
        let channel = AppServerAgentChannel {
            client: Arc::new(mock_client),
        };
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
    /// Verifies client turn failure propagates as `Err(AgentError)`.
    async fn test_run_turn_client_failure_returns_agent_error() {
        // Arrange
        let mut mock_client = MockAppServerClient::new();
        mock_client
            .expect_run_turn()
            .returning(|_request, _stream_tx| {
                Box::pin(async { Err("server timeout".to_string()) })
            });
        let channel = AppServerAgentChannel {
            client: Arc::new(mock_client),
        };
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
        let channel = AppServerAgentChannel {
            client: Arc::new(mock_client),
        };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();

        // Act
        let result = channel
            .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
            .await
            .expect("turn should succeed");

        // Assert
        assert_eq!(result.assistant_message, "Result");
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
        let channel = AppServerAgentChannel {
            client: Arc::new(mock_client),
        };
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let mut request = make_turn_request();
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
