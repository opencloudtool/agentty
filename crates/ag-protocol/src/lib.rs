//! Shared Agentty protocol models, schema helpers, and prompt payloads.
//!
//! This crate contains the transport-neutral wire contracts used between
//! Agentty frontends, session workflows, and provider adapters. It
//! intentionally avoids depending on the main `agentty` crate so future
//! frontends can reuse protocol parsing and turn-prompt serialization without
//! pulling in TUI state.

mod envelope;
mod model;
mod parse;
mod prompt;
mod question;
mod review;
mod schema;
mod subtask;
mod verification;

pub use envelope::{
    ProtocolSchemaInstructionMode, build_protocol_repair_prompt, prepend_protocol_instructions,
    prepend_protocol_refresh_reminder,
};
pub use model::{
    AgentResponse, AgentResponseParseError, ProtocolRequestProfile, ReviewCommentOutcome,
    ReviewCommentResolution,
};
pub use parse::{
    format_protocol_parse_debug_details, parse_agent_response_strict,
    parse_protocol_response_strict,
};
pub use prompt::{
    TurnPrompt, TurnPromptAttachment, TurnPromptContentPart, TurnPromptTextSource,
    render_prompt_text_for_agent, split_turn_prompt_content,
};
pub use question::QuestionItem;
pub use review::{FocusedReview, FocusedReviewSeverity, FocusedReviewSuggestion};
pub use schema::{
    SchemaRequiredPolicy, agent_response_json_schema_json, agent_response_output_schema,
    agent_response_output_schema_json, focused_review_json_schema_json,
    focused_review_output_schema, protocol_output_schema,
};
pub use subtask::{SubtaskItem, SubtaskKind};
pub use verification::{VerificationVerdict, VerificationVerdictItem};
