//! Provider router for app-server backed session execution.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::domain::agent::{AgentKind, AgentModel};
use crate::infra::app_server::{
    AppServerClient, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse,
};
use crate::infra::codex_app_server::RealCodexAppServerClient;
use crate::infra::gemini_acp::RealGeminiAcpClient;

/// Production router that dispatches app-server turns by model provider.
pub struct RoutingAppServerClient {
    codex_client: Arc<dyn AppServerClient>,
    gemini_client: Arc<dyn AppServerClient>,
}

impl RoutingAppServerClient {
    /// Creates a router backed by production Codex and Gemini clients.
    pub fn new() -> Self {
        Self::new_with_clients(
            Arc::new(RealCodexAppServerClient::new()),
            Arc::new(RealGeminiAcpClient::new()),
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
    ) -> AppServerFuture<Result<AppServerTurnResponse, String>> {
        let codex_client = Arc::clone(&self.codex_client);
        let gemini_client = Arc::clone(&self.gemini_client);
        let model = request.model.clone();

        Box::pin(async move {
            let model = model.parse::<AgentModel>().map_err(|error| {
                format!("App-server routing failed for unknown model `{model}`: {error}")
            })?;
            match model.kind() {
                AgentKind::Codex => codex_client.run_turn(request, stream_tx).await,
                AgentKind::Gemini => gemini_client.run_turn(request, stream_tx).await,
                AgentKind::Claude => {
                    Err("Claude does not support app-server session execution".to_string())
                }
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
            live_session_output: None,
            folder: std::env::temp_dir(),
            model: AgentModel::Gpt53Codex.as_str().to_string(),
            prompt: "prompt".to_string(),
            session_id: "session-1".to_string(),
            session_output: None,
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
                })
            })
        });
        let app_server_client = RoutingAppServerClient::new_with_clients(
            Arc::new(codex_client),
            Arc::new(gemini_client),
        );
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
        let request = AppServerTurnRequest {
            live_session_output: None,
            folder: std::env::temp_dir(),
            model: AgentModel::Gemini3FlashPreview.as_str().to_string(),
            prompt: "prompt".to_string(),
            session_id: "session-1".to_string(),
            session_output: None,
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
            live_session_output: None,
            folder: std::env::temp_dir(),
            model: "unknown-model".to_string(),
            prompt: "prompt".to_string(),
            session_id: "session-1".to_string(),
            session_output: None,
        };

        // Act
        let result = app_server_client.run_turn(request, stream_tx).await;
        let error = result.err().unwrap_or_default();

        // Assert
        assert!(
            error.contains("unknown model"),
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
