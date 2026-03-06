//! Session workflow module router used by `core.rs`.
//!
//! This module re-exports core session types/constants for child workflow
//! modules and exposes workflow submodules for orchestration wiring.

pub(super) use super::core::{
    COMMIT_MESSAGE, Clock, SESSION_REFRESH_INTERVAL, SessionManager, SessionTaskService,
    session_branch, session_folder,
};

pub(super) mod access;
pub(super) mod lifecycle;
pub(super) mod load;
pub(super) mod merge;
pub(super) mod refresh;
pub(super) mod review;
pub(super) mod task;
pub(super) mod worker;
