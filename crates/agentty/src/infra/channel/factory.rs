//! Channel factory for routing providers to transport adapters.

use std::sync::Arc;

use crate::domain::agent::AgentKind;
use crate::infra::agent;
use crate::infra::app_server::AppServerClient;
use crate::infra::channel::app_server::AppServerAgentChannel;
use crate::infra::channel::cli::CliAgentChannel;
use crate::infra::channel::contract::AgentChannel;

/// Creates the provider-specific channel for the given agent kind.
///
/// CLI providers (Claude) use [`CliAgentChannel`]; app-server providers
/// (Gemini, Codex) use [`AppServerAgentChannel`].
pub fn create_agent_channel(
    kind: AgentKind,
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
) -> Arc<dyn AgentChannel> {
    let backend = agent::create_backend(kind);
    let transport = backend.transport();

    if transport.uses_app_server() {
        let app_server_client = backend
            .app_server_client(app_server_client_override)
            .expect("app-server backend should provide an app-server client");

        Arc::new(AppServerAgentChannel::new(app_server_client, kind))
    } else {
        Arc::new(CliAgentChannel::with_backend(Arc::from(backend), kind))
    }
}
