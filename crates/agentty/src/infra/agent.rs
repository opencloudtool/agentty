//! Agent backend wiring split into provider-specific submodules.
//!
//! This module keeps the public API stable while delegating command building
//! and response parsing to focused files under `infra/agent/` using the
//! standard `agent.rs` + `agent/` module layout.

mod backend;
mod claude;
mod codex;
mod gemini;
mod prompt;
pub(crate) mod protocol;
mod response_parser;
mod submission;

pub use backend::{
    AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest, create_backend,
    parse_response, transport_mode,
};
pub(crate) use backend::{
    build_command_stdin_payload, is_app_server_thought_chunk, parse_stream_output_line,
    parse_turn_response, should_stream_app_server_assistant_messages,
};
pub(crate) use prompt::{PromptPreparationRequest, prepare_prompt_text};
pub(crate) use protocol::AgentResponse;
pub use protocol::ProtocolRequestProfile;
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
