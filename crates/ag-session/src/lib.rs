//! Frontend-neutral session management API and shared session models.
//!
//! Host applications implement [`SessionBackend`] to connect the stable
//! programmatic API to their persistence, agent, Git, and forge workflows.
//! Callers such as future orchestrator sessions use [`SessionService`] without
//! depending on terminal UI state.

mod error;
mod message;
mod model;
mod orchestration;
mod personality;
mod project;
mod question;
mod review;
mod service;
mod setting;
mod transcript_notice;

pub use error::SessionError;
pub use message::{
    SessionMessage, SessionMessageKind, SessionMessageKindParseError, SessionTranscript,
    normalized_message_content, stored_message_content,
};
pub use model::{
    ForgeKind, PermissionMode, ResponseStyle, ReviewRequest, ReviewRequestState,
    ReviewRequestSummary, Session, SessionId, SessionRole, SessionSettings, SessionStatus,
    SpeedMode, activity_day_key_with_offset, session_branch,
};
pub use orchestration::{
    IntegrationApproach, MAX_AUTOMATED_REVIEW_ITERATIONS, OrchestrationPlanTask,
    OrchestrationPolicy, OrchestrationScheduleDecision, OrchestrationStatus, OrchestrationTaskKind,
    OrchestrationTaskStatus, validate_subtasks,
};
pub use personality::{
    PERSONALITY_PROMPT_MAX_BYTES, Personality, PersonalityParseError, PersonalitySummary,
    parse_agent_definition, parse_agent_summary,
};
pub use project::{
    Project, ProjectListItem, mru_project_order, ordered_project_items, project_name_from_path,
};
pub use question::{QuestionItem, default_option_index};
pub use review::{
    FocusedReviewStatus, build_apply_review_prompt, has_actionable_review_suggestions,
    review_suggestions,
};
pub use service::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CoordinatorMessageVisibility,
    CreateSessionMode, CreateSessionRequest, QuestionAnswer, SessionBackend, SessionService,
};
pub use setting::{
    DEFAULT_AUTO_APPROVE_ORCHESTRATION_RESEARCH, DEFAULT_ORCHESTRATION_PARALLELISM,
    MAX_ORCHESTRATION_PARALLELISM, SettingName,
};
pub use transcript_notice::TranscriptNotice;
