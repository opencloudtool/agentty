//! Session module router.
//!
//! This parent module intentionally exposes child modules and re-exports
//! session orchestration types and helper APIs.

mod core;
mod error;
mod workflow;

pub use core::SessionManager;
pub(crate) use core::{
    Clock, RealClock, RunAgentAssistTaskInput, SESSION_REFRESH_INTERVAL, SessionDefaults,
    SessionTaskService, SyncMainOutcome, SyncSessionStartError, TurnAppliedState, session_branch,
    unix_timestamp_from_system_time,
};

pub use error::SessionError;
