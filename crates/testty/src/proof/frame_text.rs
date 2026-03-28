//! Frame-text proof backend.
//!
//! [`FrameTextBackend`] renders a [`ProofReport`](super::report::ProofReport)
//! as annotated plain text, producing the same output as
//! [`ProofReport::to_annotated_text()`](super::report::ProofReport::to_annotated_text)
//! but routed through the [`ProofBackend`](super::backend::ProofBackend) trait.

use std::path::Path;

use super::backend::ProofBackend;
use super::report::{ProofError, ProofReport};

/// Renders a proof report as annotated plain-text frame dumps.
///
/// This is the simplest backend, writing each captured frame as a
/// bordered text block with step labels, descriptions, and assertion
/// markers. The output is identical to [`ProofReport::to_annotated_text()`].
pub struct FrameTextBackend;

impl ProofBackend for FrameTextBackend {
    /// Write the annotated text proof to the given output path.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError::Io`] if writing the file fails.
    fn render(&self, report: &ProofReport, output: &Path) -> Result<(), ProofError> {
        let text = report.to_annotated_text();
        std::fs::write(output, text)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::TerminalFrame;

    #[test]
    fn frame_text_backend_produces_same_output_as_to_annotated_text() {
        // Arrange
        let frame = TerminalFrame::new(40, 5, b"Hello proof");
        let mut report = ProofReport::new("consistency_check");
        report.add_capture("init", "Initial state", &frame);
        report.add_capture("done", "Final state", &frame);

        let expected = report.to_annotated_text();
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("proof.txt");

        // Act
        let backend = FrameTextBackend;
        backend
            .render(&report, &output_path)
            .expect("render should succeed");

        // Assert
        let written = std::fs::read_to_string(&output_path).expect("failed to read output");
        assert_eq!(written, expected);
    }

    #[test]
    fn frame_text_backend_writes_valid_file() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Test content");
        let mut report = ProofReport::new("file_test");
        report.add_capture("snap", "Snapshot", &frame);
        report.add_assertion("snap", true, "content visible");

        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("proof_output.txt");

        // Act
        let backend = FrameTextBackend;
        backend
            .render(&report, &output_path)
            .expect("render should succeed");

        // Assert
        let content = std::fs::read_to_string(&output_path).expect("failed to read");
        assert!(content.contains("Proof Report: file_test"));
        assert!(content.contains("[PASS] content visible"));
        assert!(content.contains("Test content"));
    }

    #[test]
    fn save_dispatches_through_backend_trait() {
        // Arrange
        let frame = TerminalFrame::new(40, 5, b"dispatch test");
        let mut report = ProofReport::new("dispatch");
        report.add_capture("step", "Dispatch step", &frame);

        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("dispatched.txt");

        // Act
        let backend = FrameTextBackend;
        report
            .save(&backend, &output_path)
            .expect("save should succeed");

        // Assert
        let content = std::fs::read_to_string(&output_path).expect("failed to read");
        assert!(content.contains("dispatch test"));
    }
}
