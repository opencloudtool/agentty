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

#[cfg(test)]
pub use backend::MockAgentBackend;
pub(crate) use backend::build_resume_prompt;
pub use backend::{AgentBackend, create_backend};
pub(crate) use response_parser::parse_stream_output_line;
pub use response_parser::{ParsedResponse, parse_response};
