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
    app_server_client: Arc<dyn AppServerClient>,
) -> Arc<dyn AgentChannel> {
    if agent::transport_mode(kind).uses_app_server() {
        Arc::new(AppServerAgentChannel::new(app_server_client, kind))
    } else {
        Arc::new(CliAgentChannel::new(kind))
    }
}
