//! Provider-agnostic agent channel module.
//!
//! This parent module intentionally acts as a router only:
//! it exposes child modules and re-exports the public channel API.

pub mod app_server;
pub mod cli;
pub mod contract;
pub mod factory;

#[cfg(test)]
pub use contract::MockAgentChannel;
pub use contract::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnMode,
    TurnPrompt, TurnPromptAttachment, TurnRequest, TurnResult,
};
pub(crate) use contract::{TurnPromptContentPart, split_turn_prompt_content};
pub use factory::create_agent_channel;
