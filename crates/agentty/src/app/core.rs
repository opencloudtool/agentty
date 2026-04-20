//! App core module router.
//!
//! This parent module intentionally stays router-only and re-exports the
//! `App` facade plus focused child modules for state, startup, draw, reducer,
//! and roadmap behavior.

mod draw;
mod events;
mod new;
mod roadmap;
mod state;

pub(crate) use events::AppEvent;
pub use state::{AGENTTY_WT_DIR, App, UpdateStatus, agentty_home};
#[cfg(test)]
pub(crate) use state::{AppClients, MockSyncMainRunner};
pub(crate) use state::{SessionStatsUsage, SyncReviewRequestTaskResult};
