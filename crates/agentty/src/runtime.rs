//! Runtime module router.
//!
//! This parent module intentionally exposes child modules and re-exports
//! runtime entry APIs.

mod clipboard_image;
mod core;
mod event;
mod key_handler;
pub mod mode;
mod terminal;
mod timing;

pub(crate) use core::{EventResult, TuiTerminal, backend_err};
pub use core::{run, run_with_backend};

pub(crate) use timing::FRAME_INTERVAL;
