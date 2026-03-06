//! Runtime module router.
//!
//! This parent module intentionally exposes child modules and re-exports
//! runtime entry APIs.

mod core;
mod event;
mod key_handler;
pub mod mode;
mod terminal;

pub use core::run;
pub(crate) use core::{EventResult, TuiTerminal};
