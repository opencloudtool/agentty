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

pub use core::run;
pub(crate) use core::{EventResult, TuiTerminal};

pub(crate) use timing::FRAME_INTERVAL;
