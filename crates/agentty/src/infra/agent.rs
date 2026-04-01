//! Agent backend wiring split into provider-specific submodules.
//!
//! This module keeps the public API stable while delegating command building
//! and response parsing to focused files under `infra/agent/` using the
//! standard `agent.rs` + `agent/` module layout.

pub(crate) mod app_server;
mod availability;
mod backend;
mod claude;
pub(crate) mod cli;
mod codex;
mod gemini;
mod prompt;
pub(crate) mod protocol;
mod provider;
mod response_parser;
mod submission;

pub use availability::{AgentAvailabilityProbe, RealAgentAvailabilityProbe, executable_name};
pub use backend::{AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest};
pub(crate) use prompt::{PromptPreparationRequest, prepare_prompt_text};
pub(crate) use protocol::AgentResponse;
pub use protocol::ProtocolRequestProfile;
pub(crate) use provider::{
    build_command_stdin_payload, create_app_server_client, is_app_server_thought_chunk,
    parse_stream_output_line, parse_turn_response, provider_kind_for_model,
};
pub use provider::{create_backend, parse_response, transport_mode};
pub use response_parser::ParsedResponse;
pub(crate) use response_parser::{
    compact_codex_progress_message, is_codex_completion_status_message,
};
pub(crate) use submission::{
    OneShotRequest, OneShotSubmission, submit_one_shot, submit_one_shot_with_app_server_client,
    submit_one_shot_with_backend,
};

#[cfg(test)]
pub(crate) mod tests {
    //! Test-only exports for agent backend mocks.

    pub(crate) use super::backend::MockAgentBackend;
}
