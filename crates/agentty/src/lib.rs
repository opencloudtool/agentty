use std::time::Duration;

/// Suspends execution for a bounded duration.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait Sleeper: Send + Sync {
    /// Blocks for `duration`.
    fn sleep(&self, duration: Duration);
}

/// Production sleeper backed by `std::thread::sleep`.
pub(crate) struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub mod app;
pub mod domain;
pub mod infra;
pub mod ui;

pub mod runtime;

// Re-exports for backward compatibility and convenience
pub use domain::agent;
pub use infra::{db, git, version};
pub use ui::icon;
