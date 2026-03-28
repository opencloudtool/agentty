//! Proof report collector and annotated text output.
//!
//! [`ProofReport`] accumulates [`ProofCapture`] entries during scenario
//! execution. Each capture records a terminal frame snapshot alongside its
//! label, description, dimensions, and optional assertion results.

use std::fmt::Write;
use std::path::Path;

use super::backend::ProofBackend;
use crate::diff::FrameDiff;
use crate::frame::TerminalFrame;

/// A single labeled capture collected during scenario execution.
///
/// Each capture preserves the full terminal frame data (text, colors,
/// and styles), dimensions, label, description, and optional assertion
/// results for proof rendering.
#[derive(Debug, Clone)]
pub struct ProofCapture {
    /// Short identifier for this capture step.
    pub label: String,
    /// Human-readable description of what this capture documents.
    pub description: String,
    /// Full terminal text at the moment of capture (plain text, no escapes).
    pub frame_text: String,
    /// ANSI-formatted bytes that reproduce the full frame state including
    /// colors and styles. Pass to [`TerminalFrame::new()`] with [`cols`]
    /// and [`rows`] to reconstruct a frame with full cell metadata.
    pub frame_bytes: Vec<u8>,
    /// Number of terminal columns at capture time.
    pub cols: u16,
    /// Number of terminal rows at capture time.
    pub rows: u16,
    /// Optional list of assertion results (pass/fail with description).
    pub assertions: Vec<AssertionResult>,
}

/// The outcome of a single assertion evaluated against a captured frame.
#[derive(Debug, Clone)]
pub struct AssertionResult {
    /// Whether the assertion passed.
    pub passed: bool,
    /// Human-readable description of the assertion.
    pub description: String,
}

/// Errors that can occur during proof report generation.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    /// An I/O operation failed during proof output.
    #[error("proof I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A formatting error occurred during proof rendering.
    #[error("proof format error: {0}")]
    Format(String),
}

/// Collector for labeled captures produced during scenario execution.
///
/// Build a `ProofReport` by calling [`add_capture()`](ProofReport::add_capture)
/// for each labeled step, then render the report through a
/// [`ProofBackend`](super::backend::ProofBackend) or directly via
/// [`to_annotated_text()`](ProofReport::to_annotated_text).
#[derive(Debug, Clone)]
pub struct ProofReport {
    /// Human-readable name of the scenario that produced this report.
    pub scenario_name: String,
    /// Ordered list of captures collected during execution.
    pub captures: Vec<ProofCapture>,
    /// Diffs between consecutive captures, indexed by `(i, i+1)` pair.
    ///
    /// `diffs[i]` is the diff from `captures[i]` to `captures[i+1]`.
    /// The length is always `captures.len().saturating_sub(1)`.
    pub diffs: Vec<FrameDiff>,
}

impl ProofReport {
    /// Create an empty proof report for the given scenario name.
    pub fn new(scenario_name: impl Into<String>) -> Self {
        Self {
            scenario_name: scenario_name.into(),
            captures: Vec::new(),
            diffs: Vec::new(),
        }
    }

    /// Add a labeled capture from a terminal frame.
    ///
    /// If a previous capture exists, a [`FrameDiff`] between the previous
    /// and current frame is automatically computed and stored.
    pub fn add_capture(
        &mut self,
        label: impl Into<String>,
        description: impl Into<String>,
        frame: &TerminalFrame,
    ) {
        // Compute diff from previous capture's reconstructed frame.
        if let Some(previous) = self.captures.last() {
            let previous_frame =
                TerminalFrame::new(previous.cols, previous.rows, &previous.frame_bytes);
            self.diffs.push(FrameDiff::compute(&previous_frame, frame));
        }

        self.captures.push(ProofCapture {
            label: label.into(),
            description: description.into(),
            frame_text: frame.all_text(),
            frame_bytes: frame.contents_formatted(),
            cols: frame.cols(),
            rows: frame.rows(),
            assertions: Vec::new(),
        });
    }

    /// Attach an assertion result to the capture with the given label.
    ///
    /// Returns `true` if the label was found and the assertion was
    /// attached, `false` if no capture matches the label.
    pub fn add_assertion(
        &mut self,
        label: &str,
        passed: bool,
        description: impl Into<String>,
    ) -> bool {
        if let Some(capture) = self
            .captures
            .iter_mut()
            .find(|capture| capture.label == label)
        {
            capture.assertions.push(AssertionResult {
                passed,
                description: description.into(),
            });

            return true;
        }

        false
    }

    /// Render the report through a [`ProofBackend`] and write to `path`.
    ///
    /// This is the primary proof output method. Use
    /// [`to_annotated_text()`](Self::to_annotated_text) for in-memory text
    /// output.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or I/O fails.
    pub fn save(&self, backend: &dyn ProofBackend, path: &Path) -> Result<(), ProofError> {
        backend.render(self, path)
    }

    /// Render the report as annotated plain text.
    ///
    /// Each capture is shown as a bordered frame dump with step number,
    /// label, description, and assertion markers.
    pub fn to_annotated_text(&self) -> String {
        let mut output = String::new();

        write_header(&mut output, &self.scenario_name);

        for (index, capture) in self.captures.iter().enumerate() {
            let step_number = index + 1;
            write_capture_section(&mut output, step_number, capture);
        }

        write_footer(&mut output, self.captures.len());

        output
    }
}

/// Write the report header with scenario name.
fn write_header(output: &mut String, scenario_name: &str) {
    let title = format!("Proof Report: {scenario_name}");
    let border = "=".repeat(title.len().max(60));

    let _ = writeln!(output, "{border}");
    let _ = writeln!(output, "{title}");
    let _ = writeln!(output, "{border}");
    let _ = writeln!(output);
}

/// Write one capture section with bordered frame dump and assertions.
fn write_capture_section(output: &mut String, step_number: usize, capture: &ProofCapture) {
    let heading = format!(
        "Step {step_number}: [{}] {}",
        capture.label, capture.description
    );
    let separator = "-".repeat(heading.len().max(60));

    let _ = writeln!(output, "{separator}");
    let _ = writeln!(output, "{heading}");
    let _ = writeln!(output, "  Terminal: {}x{}", capture.cols, capture.rows);
    let _ = writeln!(output, "{separator}");
    let _ = writeln!(output);

    // Frame text with left border.
    let frame_border = format!("+{}+", "-".repeat(usize::from(capture.cols) + 2));
    let _ = writeln!(output, "{frame_border}");
    for line in capture.frame_text.lines() {
        let padded = format!("{line:<width$}", width = usize::from(capture.cols));
        let _ = writeln!(output, "| {padded} |");
    }
    let _ = writeln!(output, "{frame_border}");
    let _ = writeln!(output);

    // Assertion results.
    if !capture.assertions.is_empty() {
        let _ = writeln!(output, "  Assertions:");
        for assertion in &capture.assertions {
            let marker = if assertion.passed { "PASS" } else { "FAIL" };
            let _ = writeln!(output, "    [{marker}] {}", assertion.description);
        }
        let _ = writeln!(output);
    }
}

/// Write the report footer with capture count.
fn write_footer(output: &mut String, capture_count: usize) {
    let _ = writeln!(output, "Total captures: {capture_count}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_report_collects_captures() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello, World!");

        // Act
        let mut report = ProofReport::new("test_scenario");
        report.add_capture("startup", "Application launched", &frame);
        report.add_capture("after_input", "User typed text", &frame);

        // Assert
        assert_eq!(report.captures.len(), 2);
        assert_eq!(report.captures[0].label, "startup");
        assert_eq!(report.captures[1].label, "after_input");
        assert!(report.captures[0].frame_text.contains("Hello, World!"));
    }

    #[test]
    fn add_assertion_attaches_to_labeled_capture() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Test");
        let mut report = ProofReport::new("assert_test");
        report.add_capture("check", "Checking state", &frame);

        // Act
        report.add_assertion("check", true, "Text 'Test' is visible");
        report.add_assertion("check", false, "Color is blue");

        // Assert
        let capture = &report.captures[0];
        assert_eq!(capture.assertions.len(), 2);
        assert!(capture.assertions[0].passed);
        assert!(!capture.assertions[1].passed);
    }

    #[test]
    fn add_assertion_targets_specific_capture() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Test");
        let mut report = ProofReport::new("targeted_test");
        report.add_capture("first", "First", &frame);
        report.add_capture("second", "Second", &frame);

        // Act
        let found = report.add_assertion("first", true, "targeted assertion");

        // Assert — assertion lands on "first", not "second".
        assert!(found);
        assert_eq!(report.captures[0].assertions.len(), 1);
        assert_eq!(
            report.captures[0].assertions[0].description,
            "targeted assertion"
        );
        assert!(report.captures[1].assertions.is_empty());
    }

    #[test]
    fn add_assertion_returns_false_for_unknown_label() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Test");
        let mut report = ProofReport::new("miss_test");
        report.add_capture("existing", "Existing", &frame);

        // Act
        let found = report.add_assertion("nonexistent", true, "orphan");

        // Assert
        assert!(!found);
        assert!(report.captures[0].assertions.is_empty());
    }

    #[test]
    fn annotated_text_contains_scenario_name() {
        // Arrange
        let report = ProofReport::new("my_scenario");

        // Act
        let text = report.to_annotated_text();

        // Assert
        assert!(text.contains("Proof Report: my_scenario"));
    }

    #[test]
    fn annotated_text_contains_step_labels_and_descriptions() {
        // Arrange
        let frame = TerminalFrame::new(40, 5, b"content");
        let mut report = ProofReport::new("labeled_test");
        report.add_capture("init", "Initial state", &frame);
        report.add_capture("done", "Final state", &frame);

        // Act
        let text = report.to_annotated_text();

        // Assert
        assert!(text.contains("Step 1: [init] Initial state"));
        assert!(text.contains("Step 2: [done] Final state"));
        assert!(text.contains("Total captures: 2"));
    }

    #[test]
    fn annotated_text_contains_frame_content() {
        // Arrange
        let frame = TerminalFrame::new(40, 5, b"Hello proof");
        let mut report = ProofReport::new("frame_test");
        report.add_capture("snap", "Snapshot", &frame);

        // Act
        let text = report.to_annotated_text();

        // Assert
        assert!(text.contains("Hello proof"));
        assert!(text.contains("Terminal: 40x5"));
    }

    #[test]
    fn annotated_text_contains_assertion_markers() {
        // Arrange
        let frame = TerminalFrame::new(40, 5, b"Test");
        let mut report = ProofReport::new("assert_output");
        report.add_capture("check", "Verify state", &frame);
        report.add_assertion("check", true, "text visible");
        report.add_assertion("check", false, "color match");

        // Act
        let text = report.to_annotated_text();

        // Assert
        assert!(text.contains("[PASS] text visible"));
        assert!(text.contains("[FAIL] color match"));
    }

    #[test]
    fn proof_capture_stores_dimensions() {
        // Arrange
        let frame = TerminalFrame::new(120, 40, b"wide");

        // Act
        let mut report = ProofReport::new("dims");
        report.add_capture("wide_term", "Wide terminal", &frame);

        // Assert
        assert_eq!(report.captures[0].cols, 120);
        assert_eq!(report.captures[0].rows, 40);
    }

    #[test]
    fn auto_diff_computed_between_consecutive_captures() {
        // Arrange
        let frame_a = TerminalFrame::new(80, 24, b"Hello");
        let frame_b = TerminalFrame::new(80, 24, b"World");

        // Act
        let mut report = ProofReport::new("diff_test");
        report.add_capture("before", "Before change", &frame_a);
        report.add_capture("after", "After change", &frame_b);

        // Assert — one diff between the two captures.
        assert_eq!(report.diffs.len(), 1);
        assert!(!report.diffs[0].is_identical());
    }

    #[test]
    fn no_diff_for_single_capture() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Solo");

        // Act
        let mut report = ProofReport::new("single");
        report.add_capture("only", "Only capture", &frame);

        // Assert
        assert!(report.diffs.is_empty());
    }

    #[test]
    fn identical_captures_produce_identical_diff() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Same");

        // Act
        let mut report = ProofReport::new("same");
        report.add_capture("first", "First", &frame);
        report.add_capture("second", "Second", &frame);

        // Assert
        assert_eq!(report.diffs.len(), 1);
        assert!(report.diffs[0].is_identical());
    }

    #[test]
    fn frame_bytes_preserves_style_for_diff() {
        // Arrange — same text but different style (plain vs bold).
        let frame_a = TerminalFrame::new(80, 24, b"Hello");
        let frame_b = TerminalFrame::new(80, 24, b"\x1b[1mHello\x1b[0m");

        // Act
        let mut report = ProofReport::new("style_diff");
        report.add_capture("plain", "Plain text", &frame_a);
        report.add_capture("bold", "Bold text", &frame_b);

        // Assert — style change should be detected because frame_bytes
        // preserves ANSI formatting for accurate reconstruction.
        assert_eq!(report.diffs.len(), 1);
        assert!(!report.diffs[0].is_identical());
    }

    #[test]
    fn frame_bytes_stores_formatted_output() {
        // Arrange — colored text.
        let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

        // Act
        let mut report = ProofReport::new("bytes_test");
        report.add_capture("colored", "Red text", &frame);

        // Assert — frame_bytes should contain ANSI escape sequences.
        let capture = &report.captures[0];
        assert!(!capture.frame_bytes.is_empty());
        assert!(capture.frame_bytes.len() > capture.frame_text.len());
    }
}
