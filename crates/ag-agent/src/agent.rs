//! Agent backend wiring split into provider-specific submodules.
//!
//! This module feeds the curated crate-root API while keeping provider
//! command builders, parsers, and transport policy descriptors private.

mod antigravity;
pub(crate) mod app_server;
mod availability;
mod backend;
mod claude;
pub(crate) mod cli;
mod codex;
mod gemini;
mod instruction;
mod prompt;
mod provider;
pub(crate) mod replay;
mod response_parser;
mod submission;

pub use availability::{
    AgentAvailabilityProbe, RealAgentAvailabilityProbe, StaticAgentAvailabilityProbe,
    executable_name,
};
#[cfg(any(test, feature = "test-utils"))]
pub use backend::MockAgentBackend;
pub use backend::{AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest};
pub use instruction::normalize_instruction_conversation_id;
pub(crate) use instruction::{InstructionDeliveryMode, plan_app_server_instruction_delivery};
pub use prompt::diff_fence;
pub(crate) use prompt::{
    PromptPreparationRequest, apply_response_style_prompt, prepare_prompt_text,
};
pub(crate) use provider::{
    build_command_stdin_payload, is_app_server_thought_chunk, parse_response,
    parse_stream_output_line, parse_turn_response, protocol_schema_instruction_mode,
};
pub use provider::{create_app_server_client, create_backend, transport_mode};
pub use replay::cleanup_session_worktree_artifacts;
pub(crate) use response_parser::{
    ParsedResponse, compact_codex_progress_message, is_codex_completion_status_message,
};
#[cfg(any(test, feature = "test-utils"))]
pub use submission::MockOneShotClient;
pub use submission::{
    OneShotClient, OneShotError, OneShotRequest, OneShotSubmission, RealOneShotClient,
};
