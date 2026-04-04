//! App module router.
//!
//! This parent module intentionally exposes child modules and re-exports app
//! orchestration types and functions.

mod assist;
mod branch_publish;
mod core;
mod error;
mod merge_queue;
mod project;
mod reducer;
mod review;
mod service;
pub(crate) mod session;
pub mod session_state;
pub(crate) mod setting;
mod startup;
pub(crate) mod tab;
mod task;

pub use core::{AGENTTY_WT_DIR, App, UpdateStatus, agentty_home};
#[cfg(test)]
pub(crate) use core::{AppClients, MockSyncMainRunner};
pub(crate) use core::{AppEvent, SessionStatsUsage};

pub use error::AppError;
pub use project::ProjectManager;
pub(crate) use review::{
    ReviewCacheEntry, diff_content_hash, is_review_loading_status_message, review_loading_message,
};
pub use service::AppServices;
pub use session::{SessionError, SessionManager};
#[cfg(test)]
pub(crate) use session::{SyncMainOutcome, SyncSessionStartError};
pub use session_state::SessionState;
pub use setting::SettingsManager;
pub use tab::{Tab, TabManager};
