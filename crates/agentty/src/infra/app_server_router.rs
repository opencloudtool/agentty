//! Provider router for app-server backed session execution.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::domain::agent::AgentKind;
use crate::infra::agent;
use crate::infra::app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse,
};

/// Production router that dispatches app-server turns by model provider.
pub struct RoutingAppServerClient {
    codex_client: Arc<dyn AppServerClient>,
    gemini_client: Arc<dyn AppServerClient>,
}

impl RoutingAppServerClient {
    /// Creates a router backed by production Codex and Gemini clients.
    pub fn new() -> Self {
        Self::new_with_clients(
            agent::create_app_server_client(AgentKind::Codex, None)
                .expect("Codex should provide an app-server client"),
            agent::create_app_server_client(AgentKind::Gemini, None)
                .expect("Gemini should provide an app-server client"),
        )
    }

    /// Creates a router with injected provider clients.
    pub fn new_with_clients(
        codex_client: Arc<dyn AppServerClient>,
        gemini_client: Arc<dyn AppServerClient>,
    ) -> Self {
        Self {
            codex_client,
            gemini_client,
        }
    }
}

impl Default for RoutingAppServerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AppServerClient for RoutingAppServerClient {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        let codex_client = Arc::clone(&self.codex_client);
        let gemini_client = Arc::clone(&self.gemini_client);
        let model = request.model.clone();

        Box::pin(async move {
            let provider_kind = agent::provider_kind_for_model(&model).map_err(|error| {
                AppServerError::Provider(format!("App-server routing failed for {error}"))
            })?;

            match provider_kind {
                AgentKind::Codex => codex_client.run_turn(request, stream_tx).await,
                AgentKind::Gemini => gemini_client.run_turn(request, stream_tx).await,
                AgentKind::Claude => Err(AppServerError::Provider(
                    "Claude does not support app-server session execution".to_string(),
                )),
            }
        })
    }

    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()> {
        let codex_client = Arc::clone(&self.codex_client);
        let gemini_client = Arc::clone(&self.gemini_client);

        Box::pin(async move {
            codex_client.shutdown_session(session_id.clone()).await;
            gemini_client.shutdown_session(session_id).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::{AgentModel, ReasoningLevel};
    use crate::infra::app_server::MockAppServerClient;

    #[tokio::test]
    async fn run_turn_routes_codex_models_to_codex_client() {
        // Arrange
        let mut codex_client = MockAppServerClient::new();
        codex_client.expect_run_turn().times(1).returning(|_, _| {
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: "codex".to_string(),
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 2,
                    pid: Some(111),
                    provider_conversation_id: Some("thread-codex".to_string()),
                })
            })
        });
        let mut gemini_client = MockAppServerClient::new();
        gemini_client.expect_run_turn().times(0);
        let app_server_client = RoutingAppServerClient::new_with_clients(
            Arc::new(codex_client),
            Arc::new(gemini_client),
        );
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
        let request = AppServerTurnRequest {
            folder: std::env::temp_dir(),
            live_session_output: None,
            model: AgentModel::Gpt53Codex.as_str().to_string(),
            prompt: "prompt".into(),
            request_kind: crate::infra::channel::AgentRequestKind::SessionStart,
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let response = app_server_client
            .run_turn(request, stream_tx)
            .await
            .expect("run_turn should succeed");

        // Assert
        assert_eq!(response.assistant_message, "codex");
    }

    #[tokio::test]
    async fn run_turn_routes_gemini_models_to_gemini_client() {
        // Arrange
        let mut codex_client = MockAppServerClient::new();
        codex_client.expect_run_turn().times(0);
        let mut gemini_client = MockAppServerClient::new();
        gemini_client.expect_run_turn().times(1).returning(|_, _| {
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: "gemini".to_string(),
                    context_reset: false,
                    input_tokens: 3,
                    output_tokens: 4,
                    pid: Some(222),
                    provider_conversation_id: Some("thread-gemini".to_string()),
                })
            })
        });
        let app_server_client = RoutingAppServerClient::new_with_clients(
            Arc::new(codex_client),
            Arc::new(gemini_client),
        );
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
        let request = AppServerTurnRequest {
            folder: std::env::temp_dir(),
            live_session_output: None,
            model: AgentModel::Gemini3FlashPreview.as_str().to_string(),
            prompt: "prompt".into(),
            request_kind: crate::infra::channel::AgentRequestKind::SessionStart,
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let response = app_server_client
            .run_turn(request, stream_tx)
            .await
            .expect("run_turn should succeed");

        // Assert
        assert_eq!(response.assistant_message, "gemini");
    }

    #[tokio::test]
    async fn run_turn_returns_error_for_unknown_model() {
        // Arrange
        let mut codex_client = MockAppServerClient::new();
        codex_client.expect_run_turn().times(0);
        let mut gemini_client = MockAppServerClient::new();
        gemini_client.expect_run_turn().times(0);
        let app_server_client = RoutingAppServerClient::new_with_clients(
            Arc::new(codex_client),
            Arc::new(gemini_client),
        );
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
        let request = AppServerTurnRequest {
            folder: std::env::temp_dir(),
            live_session_output: None,
            model: "unknown-model".to_string(),
            prompt: "prompt".into(),
            request_kind: crate::infra::channel::AgentRequestKind::SessionStart,
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let result = app_server_client.run_turn(request, stream_tx).await;
        let error = result.expect_err("should fail for unknown model");

        // Assert
        assert!(
            error.to_string().contains("unknown model"),
            "error should mention unknown model"
        );
    }

    #[tokio::test]
    async fn shutdown_session_propagates_to_all_provider_clients() {
        // Arrange
        let mut codex_client = MockAppServerClient::new();
        codex_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));
        let mut gemini_client = MockAppServerClient::new();
        gemini_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));
        let app_server_client = RoutingAppServerClient::new_with_clients(
            Arc::new(codex_client),
            Arc::new(gemini_client),
        );

        // Act
        app_server_client
            .shutdown_session("session-1".to_string())
            .await;

        // Assert
    }
}
