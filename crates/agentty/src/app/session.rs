//! Session module router.
//!
//! This parent module intentionally exposes child modules and re-exports
//! session orchestration types and helper APIs.

mod core;
mod error;
mod resource;
mod state;
mod workflow;

pub use core::SessionManager;
pub(crate) use core::{
    Clock, RunAgentAssistTaskInput, SESSION_REFRESH_INTERVAL, SessionCreationKind,
    SessionCreationSettings, SessionDefaults, SessionTaskService, StatusTransition,
    SyncMainOutcome, SyncSessionStartError, TurnAppliedState, remote_branch_name_from_upstream_ref,
    session_branch, session_folder, unix_timestamp_from_system_time,
};

pub use error::SessionError;
pub(crate) use state::SessionGitStatus;
pub use state::SessionState;
pub(crate) use workflow::load::{
    SessionLoadInput, load_session_transcript, migrate_active_sessions_off_retired_models,
    migrate_session_off_retired_model,
};
pub(crate) use workflow::refresh::SyncReviewRequestOutcome;
