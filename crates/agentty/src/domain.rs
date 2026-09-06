//! Domain entities and pure business logic.

pub mod composer;
pub mod file_entry;
/// Editable text-input state and cursor operations.
pub mod input;
/// Orchestration and orchestration-task lifecycle states.
pub mod orchestration;
/// Workspace personality definitions and frontmatter parsing.
pub mod personality;
/// Persisted project entities and project-list ordering.
pub mod project;
pub mod question;
/// Live process-accounting projections.
pub mod resource;
pub mod review;
pub mod selection;
/// Session lifecycle entities, statistics, and identifiers.
pub mod session;
/// Canonical persisted session transcript messages.
pub mod session_message;
pub mod session_order;
pub mod setting;
pub mod theme;
pub(crate) mod transcript_notice;
pub(crate) mod transient_message;

/// Agent provider and model metadata used by the domain layer.
pub mod agent {
    pub use ag_agent::{
        AgentCliInfo, AgentCliVersion, AgentKind, AgentModel, AgentSelection,
        AgentSelectionMetadata, ReasoningLevel, ResponseStyle, SpeedMode,
        parse_persisted_session_agent_model, resolve_agent_kind_for_model,
        resolve_agent_selection_for_model, resolve_model_for_available_agent_kinds,
        resolve_prompt_model_agent_kind, selectable_models_for_agent_kinds,
    };
}

/// Agent permission mode metadata.
pub mod permission {
    pub use ag_agent::PermissionMode;
}

/// Canonical turn prompt payload types.
pub mod turn_prompt {
    pub use ag_protocol::{
        TurnPrompt, TurnPromptAttachment, TurnPromptContentPart, TurnPromptTextSource,
        render_prompt_text_for_agent, split_turn_prompt_content,
    };
}
