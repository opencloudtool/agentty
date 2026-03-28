//! Shared app-server module router.
//!
//! This parent module intentionally exposes child modules and re-exports the
//! public API for provider-neutral app-server contracts, prompt shaping,
//! runtime registries, and restart/retry orchestration.

pub mod contract;
pub mod error;
pub mod prompt;
pub mod registry;
pub mod retry;

#[cfg(test)]
pub use contract::MockAppServerClient;
pub use contract::{
    AppServerClient, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse,
};
pub use error::AppServerError;
pub use prompt::turn_prompt_for_runtime;
pub use registry::AppServerSessionRegistry;
pub use retry::{RuntimeInspector, run_turn_with_restart_retry};
