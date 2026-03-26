//! Structured response protocol subsystem router.
//!
//! This parent module intentionally stays router-only:
//! it exposes the child modules and re-exports the public protocol API.

mod model;
mod parse;
mod schema;

pub use model::ProtocolRequestProfile;
pub(crate) use model::{AgentResponse, AgentResponseSummary, QuestionItem};
pub(crate) use parse::{
    format_protocol_parse_debug_details, normalize_stream_assistant_chunk, normalize_turn_response,
    parse_agent_response_strict,
};
pub(crate) use schema::{
    agent_response_json_schema_json, agent_response_output_schema,
    agent_response_output_schema_json,
};
