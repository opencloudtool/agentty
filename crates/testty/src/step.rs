//! Scenario step definitions for TUI test automation.
//!
//! Each [`Step`] represents a single action in a test scenario, such as
//! typing text, pressing a key, waiting for output, or capturing a frame.
//! Steps are designed to be compiled into both PTY executor actions and
//! VHS tape commands from a single authored scenario.

use std::time::Duration;

/// A single action in a test scenario.
///
/// Steps describe user interactions and wait conditions in a
/// platform-neutral way. The PTY executor runs them directly against
/// the terminal, while the VHS compiler translates them into tape syntax.
#[derive(Debug, Clone)]
pub enum Step {
    /// Type text into the terminal.
    WriteText(String),

    /// Press a named key (e.g., `"Enter"`, `"Tab"`, `"Escape"`, `"Up"`).
    PressKey(String),

    /// Sleep for a fixed duration.
    Sleep(Duration),

    /// Wait until the specified text appears in the terminal output.
    WaitForText {
        /// The text to search for.
        needle: String,
        /// Maximum time to wait in milliseconds.
        timeout_ms: u32,
    },

    /// Wait until the terminal frame stops changing.
    WaitForStableFrame {
        /// Duration of stability required in milliseconds.
        stable_ms: u32,
        /// Maximum time to wait in milliseconds.
        timeout_ms: u32,
    },

    /// Capture the current terminal state for assertions.
    Capture,
}

impl Step {
    /// Create a step that types the given text.
    pub fn write_text(text: impl Into<String>) -> Self {
        Self::WriteText(text.into())
    }

    /// Create a step that presses a named key.
    pub fn press_key(key: impl Into<String>) -> Self {
        Self::PressKey(key.into())
    }

    /// Create a step that sleeps for the given duration.
    pub fn sleep(duration: Duration) -> Self {
        Self::Sleep(duration)
    }

    /// Create a step that sleeps for the given number of milliseconds.
    pub fn sleep_ms(ms: u64) -> Self {
        Self::Sleep(Duration::from_millis(ms))
    }

    /// Create a step that waits for text to appear.
    pub fn wait_for_text(needle: impl Into<String>, timeout_ms: u32) -> Self {
        Self::WaitForText {
            needle: needle.into(),
            timeout_ms,
        }
    }

    /// Create a step that waits for the frame to stabilize.
    pub fn wait_for_stable_frame(stable_ms: u32, timeout_ms: u32) -> Self {
        Self::WaitForStableFrame {
            stable_ms,
            timeout_ms,
        }
    }

    /// Create a capture step.
    pub fn capture() -> Self {
        Self::Capture
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_text_stores_content() {
        // Arrange / Act
        let step = Step::write_text("hello");

        // Assert
        let Step::WriteText(text) = step else {
            unreachable!("Expected WriteText variant");
        };
        assert_eq!(text, "hello");
    }

    #[test]
    fn sleep_ms_converts_to_duration() {
        // Arrange / Act
        let step = Step::sleep_ms(500);

        // Assert
        let Step::Sleep(duration) = step else {
            unreachable!("Expected Sleep variant");
        };
        assert_eq!(duration, Duration::from_millis(500));
    }

    #[test]
    fn wait_for_text_stores_needle_and_timeout() {
        // Arrange / Act
        let step = Step::wait_for_text("Loading", 5000);

        // Assert
        let Step::WaitForText { needle, timeout_ms } = step else {
            unreachable!("Expected WaitForText variant");
        };
        assert_eq!(needle, "Loading");
        assert_eq!(timeout_ms, 5000);
    }
}
