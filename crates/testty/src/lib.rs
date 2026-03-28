//! Rust-native TUI end-to-end testing framework.
//!
//! Drives a real TUI binary in a PTY, captures location-aware terminal state
//! with `vt100`, generates VHS tapes for visual screenshots, and provides
//! an assertion API for text, style, color, and region checks.

pub mod artifact;
pub mod assertion;
pub mod calibration;
pub mod diff;
pub mod frame;
pub mod journey;
pub mod locator;
pub mod overlay;
pub mod proof;
pub mod recipe;
pub mod region;
pub mod renderer;
pub mod scenario;
pub mod session;
pub mod snapshot;
pub mod step;
pub mod vhs;
