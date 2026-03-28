//! Scenario builder for composing test scenarios from steps.
//!
//! A [`Scenario`] is an ordered sequence of [`Step`] actions that describe
//! a complete user journey through a TUI application. Scenarios are authored
//! in Rust and can be compiled into both PTY executor actions (for semantic
//! assertions) and VHS tape files (for visual screenshot capture).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::journey::Journey;
use crate::proof::report::ProofReport;
use crate::session::{PtySession, PtySessionBuilder, PtySessionError};
use crate::step::Step;
use crate::vhs::VhsTape;

/// A test scenario describing a user journey through a TUI application.
///
/// Built using a fluent API, then executed against either the PTY executor
/// or compiled into a VHS tape.
#[must_use]
pub struct Scenario {
    /// Human-readable name for this scenario (used in artifact file names).
    pub name: String,
    /// Ordered sequence of steps to execute.
    pub steps: Vec<Step>,
}

impl Scenario {
    /// Create a new empty scenario with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
        }
    }

    /// Append a step to the scenario and return `self` for chaining.
    pub fn step(mut self, step: Step) -> Self {
        self.steps.push(step);

        self
    }

    /// Type text into the terminal.
    pub fn write_text(self, text: impl Into<String>) -> Self {
        self.step(Step::write_text(text))
    }

    /// Press a named key.
    pub fn press_key(self, key: impl Into<String>) -> Self {
        self.step(Step::press_key(key))
    }

    /// Sleep for a duration.
    pub fn sleep(self, duration: Duration) -> Self {
        self.step(Step::sleep(duration))
    }

    /// Sleep for a number of milliseconds.
    pub fn sleep_ms(self, ms: u64) -> Self {
        self.step(Step::sleep_ms(ms))
    }

    /// Wait for text to appear in the terminal.
    pub fn wait_for_text(self, needle: impl Into<String>, timeout_ms: u32) -> Self {
        self.step(Step::wait_for_text(needle, timeout_ms))
    }

    /// Wait for the terminal frame to stabilize.
    pub fn wait_for_stable_frame(self, stable_ms: u32, timeout_ms: u32) -> Self {
        self.step(Step::wait_for_stable_frame(stable_ms, timeout_ms))
    }

    /// Capture the current terminal state.
    pub fn capture(self) -> Self {
        self.step(Step::capture())
    }

    /// Capture the current terminal state with a label and description.
    ///
    /// Labeled captures are collected into a
    /// [`crate::proof::report::ProofReport`] when running with
    /// `run_with_proof()`.
    pub fn capture_labeled(self, label: impl Into<String>, description: impl Into<String>) -> Self {
        self.step(Step::capture_labeled(label, description))
    }

    /// Append all steps from a journey to this scenario.
    ///
    /// Enables declarative test building by composing reusable
    /// building blocks.
    pub fn compose(mut self, journey: &Journey) -> Self {
        self.steps.extend(journey.steps.iter().cloned());

        self
    }

    /// Execute this scenario in a PTY session and return the final frame.
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails.
    pub fn execute_in_pty(
        &self,
        session: &mut PtySession,
    ) -> Result<crate::frame::TerminalFrame, PtySessionError> {
        session.execute_steps(&self.steps)
    }

    /// Execute this scenario in a PTY session with proof collection.
    ///
    /// Returns both the final frame and a [`ProofReport`] containing all
    /// labeled captures encountered during execution.
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails.
    pub fn execute_in_pty_with_proof(
        &self,
        session: &mut PtySession,
    ) -> Result<(crate::frame::TerminalFrame, ProofReport), PtySessionError> {
        let mut report = ProofReport::new(&self.name);
        let mut last_frame = None;

        for step in &self.steps {
            match step {
                Step::CaptureLabeled { label, description } => {
                    let frame = session.capture_frame();
                    report.add_capture(label, description, &frame);
                    last_frame = Some(frame);
                }
                _ => {
                    last_frame = Some(session.execute_steps(std::slice::from_ref(step))?);
                }
            }
        }

        let final_frame = last_frame.unwrap_or_else(|| session.capture_frame());

        Ok((final_frame, report))
    }

    /// Execute this scenario against a binary, creating a new PTY session
    /// with the given builder configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if spawning or execution fails.
    pub fn run(
        &self,
        builder: PtySessionBuilder,
    ) -> Result<crate::frame::TerminalFrame, PtySessionError> {
        let mut session = builder.spawn()?;

        self.execute_in_pty(&mut session)
    }

    /// Execute this scenario with proof collection, creating a new PTY
    /// session with the given builder configuration.
    ///
    /// Returns both the final frame and a [`ProofReport`] containing all
    /// labeled captures.
    ///
    /// # Errors
    ///
    /// Returns an error if spawning or execution fails.
    pub fn run_with_proof(
        &self,
        builder: PtySessionBuilder,
    ) -> Result<(crate::frame::TerminalFrame, ProofReport), PtySessionError> {
        let mut session = builder.spawn()?;

        self.execute_in_pty_with_proof(&mut session)
    }

    /// Compile this scenario into a VHS tape.
    ///
    /// The tape can be written to disk and executed with `vhs` to produce
    /// a screenshot of the same journey.
    pub fn to_vhs_tape(
        &self,
        binary_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
    ) -> VhsTape {
        VhsTape::from_scenario(self, binary_path, screenshot_path, env_vars)
    }

    /// Compile this scenario into a VHS tape and write it to a file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the tape file fails.
    pub fn write_vhs_tape(
        &self,
        binary_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
        tape_path: &Path,
    ) -> Result<PathBuf, std::io::Error> {
        let tape = self.to_vhs_tape(binary_path, screenshot_path, env_vars);
        tape.write_to(tape_path)?;

        Ok(tape_path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_builder_chains_steps() {
        // Arrange / Act
        let scenario = Scenario::new("test")
            .write_text("hello")
            .press_key("Enter")
            .sleep_ms(100)
            .capture();

        // Assert
        assert_eq!(scenario.name, "test");
        assert_eq!(scenario.steps.len(), 4);
    }

    #[test]
    fn scenario_compiles_to_vhs_tape() {
        // Arrange
        let scenario = Scenario::new("startup").sleep_ms(500).capture();

        // Act
        let tape = scenario.to_vhs_tape(
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/screenshot.png"),
            &[],
        );
        let content = tape.render();

        // Assert
        assert!(content.contains("Screenshot"));
        assert!(content.contains("Sleep"));
    }

    #[test]
    fn scenario_capture_labeled_adds_step() {
        // Arrange / Act
        let scenario = Scenario::new("labeled")
            .capture_labeled("init", "Initial state")
            .capture_labeled("done", "Final state");

        // Assert
        assert_eq!(scenario.steps.len(), 2);
    }

    #[test]
    fn scenario_compose_appends_journey_steps() {
        // Arrange
        let startup = Journey::wait_for_startup(300, 5000);
        let navigate = Journey::navigate_with_key("Tab", "Sessions", 3000);

        // Act
        let scenario = Scenario::new("composed")
            .compose(&startup)
            .compose(&navigate)
            .capture();

        // Assert — 1 from startup + 2 from navigate + 1 capture = 4.
        assert_eq!(scenario.steps.len(), 4);
    }

    #[test]
    fn scenario_compose_preserves_existing_steps() {
        // Arrange
        let journey = Journey::type_and_confirm("hello");

        // Act
        let scenario = Scenario::new("mixed")
            .sleep_ms(100)
            .compose(&journey)
            .capture();

        // Assert — 1 sleep + 2 from journey + 1 capture = 4.
        assert_eq!(scenario.steps.len(), 4);
    }
}
