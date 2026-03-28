//! VHS tape compiler for generating visual screenshot tapes from scenarios.
//!
//! Compiles a [`Scenario`] into VHS tape syntax so the same test journey
//! that runs semantically in a PTY also produces a visual screenshot via
//! the `vhs` tool. The tape includes environment setup, binary launch,
//! interaction steps, and screenshot capture.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::scenario::Scenario;
use crate::step::Step;

/// Default VHS terminal width in pixels.
const DEFAULT_VHS_WIDTH: u16 = 1200;

/// Default VHS terminal height in pixels.
const DEFAULT_VHS_HEIGHT: u16 = 600;

/// Default VHS font size in points.
const DEFAULT_VHS_FONT_SIZE: u16 = 14;

/// Maximum number of VHS execution retries.
const MAX_VHS_RETRIES: u8 = 3;

/// A compiled VHS tape ready for writing and execution.
///
/// Generated from a [`Scenario`] with environment and binary configuration.
/// The tape uses VHS commands (`Set`, `Hide`, `Show`, `Type`, `Sleep`,
/// `Wait+Screen`, `Wait+Line`, `Screenshot`) to reproduce the scenario
/// journey and capture a PNG screenshot.
pub struct VhsTape {
    /// The rendered tape content as VHS syntax.
    content: String,
    /// Path where the screenshot will be saved.
    screenshot_path: PathBuf,
}

impl VhsTape {
    /// Compile a scenario into a VHS tape.
    ///
    /// The tape sets up the environment, launches the binary, executes
    /// the scenario steps, and captures a screenshot at each `Capture`
    /// step.
    pub fn from_scenario(
        scenario: &Scenario,
        binary_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
    ) -> Self {
        let content = compile_tape(scenario, binary_path, screenshot_path, env_vars);

        Self {
            content,
            screenshot_path: screenshot_path.to_path_buf(),
        }
    }

    /// Return the rendered tape content as a string.
    pub fn render(&self) -> &str {
        &self.content
    }

    /// Write the tape to a file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the file fails.
    pub fn write_to(&self, tape_path: &Path) -> Result<(), std::io::Error> {
        std::fs::write(tape_path, &self.content)
    }

    /// Execute the tape using the `vhs` CLI and return the screenshot path.
    ///
    /// Retries up to [`MAX_VHS_RETRIES`] times if the screenshot is not
    /// produced.
    ///
    /// # Errors
    ///
    /// Returns an error if VHS is not installed, execution fails, or the
    /// screenshot is not produced after retries.
    pub fn execute(&self, tape_path: &Path) -> Result<PathBuf, VhsError> {
        check_vhs_installed()?;
        self.write_to(tape_path)
            .map_err(|err| VhsError::IoError(err.to_string()))?;

        let mut last_error = String::new();

        for attempt in 1..=MAX_VHS_RETRIES {
            // Best-effort cleanup: screenshot file may already be removed.
            let _ = std::fs::remove_file(&self.screenshot_path);

            let output = Command::new("vhs")
                .arg(tape_path)
                .output()
                .map_err(|err| VhsError::ExecutionFailed(err.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);

                return Err(VhsError::ExecutionFailed(format!(
                    "VHS exited with error: {stderr}"
                )));
            }

            if self.screenshot_path.exists() {
                return Ok(self.screenshot_path.clone());
            }

            last_error = format!(
                "Attempt {attempt}/{MAX_VHS_RETRIES}: screenshot not produced at {}",
                self.screenshot_path.display()
            );
        }

        Err(VhsError::ScreenshotNotProduced(last_error))
    }

    /// Return the path where the screenshot will be saved.
    pub fn screenshot_path(&self) -> &Path {
        &self.screenshot_path
    }
}

/// Errors from VHS tape operations.
#[derive(Debug, thiserror::Error)]
pub enum VhsError {
    /// VHS is not installed or not on PATH.
    #[error("VHS not installed: {0}")]
    NotInstalled(String),

    /// VHS execution failed.
    #[error("VHS execution failed: {0}")]
    ExecutionFailed(String),

    /// VHS ran but did not produce a screenshot.
    #[error("Screenshot not produced: {0}")]
    ScreenshotNotProduced(String),

    /// I/O error writing or reading files.
    #[error("I/O error: {0}")]
    IoError(String),
}

/// Compile a scenario into VHS tape syntax.
fn compile_tape(
    scenario: &Scenario,
    binary_path: &Path,
    screenshot_path: &Path,
    env_vars: &[(&str, &str)],
) -> String {
    let mut tape = String::new();
    let gif_path = screenshot_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}.gif", scenario.name));

    // Infallible: all `writeln!` calls below write to a String, which cannot fail.
    // Header settings.
    let _ = writeln!(tape, "Set Shell \"bash\"");
    let _ = writeln!(tape, "Set FontSize {DEFAULT_VHS_FONT_SIZE}");
    let _ = writeln!(tape, "Set Width {DEFAULT_VHS_WIDTH}");
    let _ = writeln!(tape, "Set Height {DEFAULT_VHS_HEIGHT}");
    let _ = writeln!(tape, "Set Padding 0");
    let _ = writeln!(tape, "Set TypingSpeed 0");
    let _ = writeln!(tape);
    let _ = writeln!(
        tape,
        "Output \"{}\"",
        escape_vhs_double_quote(&gif_path.display().to_string())
    );
    let _ = writeln!(tape);

    // Hidden setup: export environment variables.
    let _ = writeln!(tape, "Hide");
    for (key, value) in env_vars {
        let escaped_value = escape_shell_single_quote(value);
        let export_cmd = format!("export {key}='{escaped_value}'");
        let _ = writeln!(tape, "Type \"{}\"", escape_vhs_double_quote(&export_cmd));
        let _ = writeln!(tape, "Enter");
        let _ = writeln!(tape, "Sleep 200ms");
    }

    // Launch the binary (shell-quoted for paths with spaces/metacharacters).
    let _ = writeln!(tape, "Show");
    let _ = writeln!(tape);
    let escaped_binary = escape_shell_single_quote(&binary_path.display().to_string());
    let _ = writeln!(
        tape,
        "Type \"{}\"",
        escape_vhs_double_quote(&format!("'{escaped_binary}'"))
    );
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape);

    // Compile scenario steps.
    for step in &scenario.steps {
        compile_step(&mut tape, step, screenshot_path);
    }

    // Hidden teardown.
    let _ = writeln!(tape);
    let _ = writeln!(tape, "Hide");
    let _ = writeln!(tape, "Type \"q\"");
    let _ = writeln!(tape, "Sleep 1s");

    tape
}

/// Compile a single step into VHS tape commands.
fn compile_step(tape: &mut String, step: &Step, screenshot_path: &Path) {
    // Infallible: all `writeln!` calls below write to a String, which cannot fail.
    match step {
        Step::WriteText(text) => {
            let _ = writeln!(tape, "Type \"{}\"", escape_vhs_double_quote(text));
        }
        Step::PressKey(key) => {
            let vhs_key = key_to_vhs_command(key);
            let _ = writeln!(tape, "{vhs_key}");
        }
        Step::Sleep(duration) => {
            let ms = duration.as_millis();
            if ms >= 1000 && ms % 1000 == 0 {
                let _ = writeln!(tape, "Sleep {}s", ms / 1000);
            } else {
                let _ = writeln!(tape, "Sleep {ms}ms");
            }
        }
        Step::WaitForText { needle, timeout_ms } => {
            let timeout_secs = f64::from(*timeout_ms) / 1000.0;
            let _ = writeln!(
                tape,
                "Wait+Screen \"{}\" {timeout_secs:.1}s",
                escape_vhs_double_quote(needle)
            );
        }
        Step::WaitForStableFrame {
            stable_ms,
            timeout_ms: _,
        } => {
            // VHS does not have a direct "wait for stable" command.
            // Approximate by sleeping for the stable duration.
            let _ = writeln!(tape, "Sleep {stable_ms}ms");
        }
        Step::Capture | Step::CaptureLabeled { .. } => {
            let _ = writeln!(
                tape,
                "Screenshot \"{}\"",
                escape_vhs_double_quote(&screenshot_path.display().to_string())
            );
        }
    }
}

/// Convert a key name to the corresponding VHS command.
fn key_to_vhs_command(key: &str) -> String {
    match key.to_lowercase().as_str() {
        "enter" | "return" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        "escape" | "esc" => "Escape".to_string(),
        "backspace" => "Backspace".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        "right" => "Right".to_string(),
        "left" => "Left".to_string(),
        "space" => "Space".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        other => {
            if let Some(character) = other.strip_prefix("ctrl+") {
                format!("Ctrl+{}", character.to_uppercase())
            } else {
                format!("Type \"{}\"", escape_vhs_double_quote(other))
            }
        }
    }
}

/// Escape double quotes inside a string for use in VHS double-quoted
/// arguments (e.g., `Type "..."`, `Screenshot "..."`).
fn escape_vhs_double_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape single quotes inside a value for use in a POSIX single-quoted
/// shell string. The standard trick is to end the current single-quoted
/// segment, insert an escaped single quote, and restart a new segment:
/// `'` → `'\''`.
fn escape_shell_single_quote(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Verify that VHS is installed and available on `PATH`.
fn check_vhs_installed() -> Result<(), VhsError> {
    Command::new("vhs").arg("--version").output().map_err(|_| {
        VhsError::NotInstalled("VHS is not installed. Install with: brew install vhs".to_string())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_tape_includes_header_settings() {
        // Arrange
        let scenario = Scenario::new("test").sleep_ms(100).capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[],
        );

        // Assert
        assert!(tape.contains("Set Shell \"bash\""));
        assert!(tape.contains(&format!("Set FontSize {DEFAULT_VHS_FONT_SIZE}")));
        assert!(tape.contains(&format!("Set Width {DEFAULT_VHS_WIDTH}")));
        assert!(tape.contains("Set Padding 0"));
    }

    #[test]
    fn compile_tape_includes_env_vars() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[("AGENTTY_ROOT", "/tmp/root")],
        );

        // Assert
        assert!(tape.contains("export AGENTTY_ROOT='/tmp/root'"));
    }

    #[test]
    fn compile_tape_includes_screenshot() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[],
        );

        // Assert
        assert!(tape.contains("Screenshot \"/tmp/shot.png\""));
    }

    #[test]
    fn key_to_vhs_command_maps_common_keys() {
        // Arrange / Act / Assert
        assert_eq!(key_to_vhs_command("Enter"), "Enter");
        assert_eq!(key_to_vhs_command("tab"), "Tab");
        assert_eq!(key_to_vhs_command("escape"), "Escape");
        assert_eq!(key_to_vhs_command("up"), "Up");
        assert_eq!(key_to_vhs_command("ctrl+c"), "Ctrl+C");
    }

    #[test]
    fn compile_step_wait_for_text_uses_wait_screen() {
        // Arrange
        let step = Step::wait_for_text("Loading", 5000);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Wait+Screen \"Loading\" 5.0s"));
    }

    #[test]
    fn compile_step_sleep_uses_seconds_when_even() {
        // Arrange
        let step = Step::sleep_ms(3000);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Sleep 3s"));
    }

    #[test]
    fn compile_step_sleep_uses_milliseconds_when_fractional() {
        // Arrange
        let step = Step::sleep_ms(500);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Sleep 500ms"));
    }

    #[test]
    fn escape_vhs_double_quote_escapes_quotes_and_backslashes() {
        // Arrange / Act / Assert
        assert_eq!(escape_vhs_double_quote(r#"hello"world"#), r#"hello\"world"#);
        assert_eq!(escape_vhs_double_quote(r"back\slash"), r"back\\slash");
        assert_eq!(escape_vhs_double_quote("clean"), "clean");
    }

    #[test]
    fn escape_shell_single_quote_wraps_internal_quotes() {
        // Arrange / Act / Assert
        assert_eq!(escape_shell_single_quote("it's"), "it'\\''s");
        assert_eq!(escape_shell_single_quote("clean"), "clean");
    }

    #[test]
    fn compile_tape_escapes_env_value_with_single_quote() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[("KEY", "it's a value")],
        );

        // Assert — the single quote is shell-escaped to '\'' and the
        // backslash is then VHS-double-quote-escaped to '\\', giving '\\''
        // in the final tape string.
        assert!(tape.contains(r"it'\\''s a value"));
    }

    #[test]
    fn compile_tape_shell_quotes_binary_path() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[],
        );

        // Assert — binary path is wrapped in single quotes for the shell.
        assert!(tape.contains("Type \"'/usr/bin/echo'\""));
    }

    #[test]
    fn compile_tape_shell_quotes_binary_path_with_spaces() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/path with spaces/bin"),
            Path::new("/tmp/shot.png"),
            &[],
        );

        // Assert — spaces are safe inside single quotes.
        assert!(tape.contains("Type \"'/path with spaces/bin'\""));
    }
}
