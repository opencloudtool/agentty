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
    SessionTaskService, SyncMainOutcome, SyncSessionStartError, session_branch,
};

pub use error::SessionError;
