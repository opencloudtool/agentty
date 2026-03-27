//! Scenario builder for composing test scenarios from steps.
//!
//! A [`Scenario`] is an ordered sequence of [`Step`] actions that describe
//! a complete user journey through a TUI application. Scenarios are authored
//! in Rust and can be compiled into both PTY executor actions (for semantic
//! assertions) and VHS tape files (for visual screenshot capture).

use std::path::{Path, PathBuf};
use std::time::Duration;

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
}
