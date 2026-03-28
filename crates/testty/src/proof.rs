//! Proof pipeline for generating self-documenting test output.
//!
//! The proof module collects labeled captures during scenario execution
//! and renders them through swappable [`backend::ProofBackend`]
//! implementations. [`report::ProofReport`] is the central collector that all
//! backends consume.

pub mod backend;
pub mod frame_text;
pub mod gif;
pub mod html;
pub mod report;
pub mod strip;
