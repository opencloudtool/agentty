//! App module router.
//!
//! This parent module intentionally exposes child modules and re-exports app
//! orchestration types and functions.

mod assist;
mod core;
mod merge_queue;
mod project;
mod service;
pub(crate) mod session;
pub mod session_state;
pub(crate) mod setting;
pub(crate) mod tab;
mod task;

#[cfg(test)]
pub(crate) use core::MockSyncMainRunner;
pub use core::{AGENTTY_WT_DIR, App, agentty_home};
pub(crate) use core::{AppEvent, SessionStatsUsage};

pub use project::ProjectManager;
pub use service::AppServices;
pub use session::SessionManager;
#[cfg(test)]
pub(crate) use session::{SyncMainOutcome, SyncSessionStartError};
pub use session_state::SessionState;
pub use setting::SettingsManager;
pub use tab::{Tab, TabManager};
