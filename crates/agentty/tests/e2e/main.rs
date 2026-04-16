//! TUI end-to-end tests for the agentty binary.
//!
//! Uses the `testty` framework to drive the real binary in a PTY and
//! capture terminal frames for semantic assertions.
//!
//! # Running
//!
//! ```sh
//! cargo test -p agentty --test e2e
//! ```

mod common;
mod confirmation;
mod navigation;
mod project;
mod session;
mod setting;
mod stat;
