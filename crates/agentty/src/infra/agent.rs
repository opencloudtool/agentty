//! Agent backend wiring split into provider-specific submodules.
//!
//! This module keeps the public API stable while delegating command building
//! and response parsing to focused files under `infra/agent/`.

#[path = "agent/backend.rs"]
mod backend;
#[path = "agent/claude.rs"]
mod claude;
#[path = "agent/codex.rs"]
mod codex;
#[path = "agent/gemini.rs"]
mod gemini;
#[path = "agent/response_parser.rs"]
mod response_parser;

pub use backend::{
    AgentBackend, AgentBackendError, AgentCommandMode, AgentTransport, BuildCommandRequest,
    create_backend, parse_response, transport_mode,
};
pub(crate) use backend::{
    build_resume_prompt, parse_stream_output_line, prepend_repo_root_path_instructions,
};
pub use response_parser::ParsedResponse;
pub(crate) use response_parser::{
    compact_codex_progress_message, is_codex_completion_status_message,
};

#[cfg(test)]
pub(crate) mod tests {
    //! Test-only exports for agent backend mocks.

    pub(crate) use super::backend::MockAgentBackend;
}
