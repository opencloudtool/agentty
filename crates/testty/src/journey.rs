//! Composable journey building blocks for declarative test authoring.
//!
//! A [`Journey`] is a named, reusable sequence of [`Step`] actions that
//! can be composed into scenarios. Instead of manually sequencing low-level
//! steps, agents and contributors build scenarios from pre-built journeys
//! like [`wait_for_startup()`](Journey::wait_for_startup) and
//! [`type_and_confirm()`](Journey::type_and_confirm).

use std::time::Duration;

use crate::step::Step;

/// A named, reusable sequence of steps for composing into scenarios.
///
/// Journeys provide high-level building blocks so test authors can
/// write declarative scenarios without memorizing exact keystrokes
/// and timing values.
#[must_use]
#[derive(Debug, Clone)]
pub struct Journey {
    /// Human-readable name for this journey (used in composed names).
    pub name: String,
    /// Optional description of what this journey accomplishes.
    pub description: Option<String>,
    /// The steps that make up this journey.
    pub steps: Vec<Step>,
}

impl Journey {
    /// Create a new empty journey with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            steps: Vec::new(),
        }
    }

    /// Set a description for this journey.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());

        self
    }

    /// Append a step to this journey.
    pub fn step(mut self, step: Step) -> Self {
        self.steps.push(step);

        self
    }

    /// Wait for the application to start up and stabilize.
    ///
    /// Waits for the terminal frame to stop changing for `stable_ms`
    /// milliseconds, with a maximum timeout of `timeout_ms`.
    pub fn wait_for_startup(stable_ms: u32, timeout_ms: u32) -> Self {
        Self::new("wait_for_startup")
            .with_description("Wait for application startup and frame stabilization")
            .step(Step::wait_for_stable_frame(stable_ms, timeout_ms))
    }

    /// Navigate by pressing a key and waiting for expected text to appear.
    ///
    /// Useful for tab switching, menu navigation, and view transitions.
    pub fn navigate_with_key(
        key: impl Into<String>,
        expected_text: impl Into<String>,
        timeout_ms: u32,
    ) -> Self {
        let key_name = key.into();
        let expected = expected_text.into();

        Self::new(format!("navigate_{key_name}"))
            .with_description(format!("Press '{key_name}' and wait for '{expected}'"))
            .step(Step::press_key(&key_name))
            .step(Step::wait_for_text(expected, timeout_ms))
    }

    /// Type text and press Enter to confirm.
    ///
    /// Useful for input fields, prompts, and command entry.
    pub fn type_and_confirm(text: impl Into<String>) -> Self {
        let input_text = text.into();

        Self::new("type_and_confirm")
            .with_description(format!("Type '{input_text}' and press Enter"))
            .step(Step::write_text(&input_text))
            .step(Step::press_key("Enter"))
    }

    /// Press a key and wait briefly for the UI to react.
    ///
    /// Useful for simple key actions that need a short delay.
    pub fn press_and_wait(key: impl Into<String>, wait_ms: u64) -> Self {
        let key_name = key.into();

        Self::new(format!("press_{key_name}"))
            .with_description(format!("Press '{key_name}' and wait {wait_ms}ms"))
            .step(Step::press_key(&key_name))
            .step(Step::sleep(Duration::from_millis(wait_ms)))
    }

    /// Capture a labeled snapshot with a description.
    ///
    /// Useful as a journey building block for proof-enabled scenarios.
    pub fn capture_labeled(label: impl Into<String>, description: impl Into<String>) -> Self {
        let label_text = label.into();
        let description_text = description.into();

        Self::new(format!("capture_{label_text}"))
            .with_description(format!("Capture: {description_text}"))
            .step(Step::capture_labeled(label_text, description_text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journey_new_creates_empty() {
        // Arrange / Act
        let journey = Journey::new("test_journey");

        // Assert
        assert_eq!(journey.name, "test_journey");
        assert!(journey.description.is_none());
        assert!(journey.steps.is_empty());
    }

    #[test]
    fn journey_with_description_sets_description() {
        // Arrange / Act
        let journey = Journey::new("described").with_description("Does something");

        // Assert
        assert_eq!(journey.description.as_deref(), Some("Does something"));
    }

    #[test]
    fn journey_step_appends() {
        // Arrange / Act
        let journey = Journey::new("custom")
            .step(Step::write_text("hello"))
            .step(Step::press_key("Enter"));

        // Assert
        assert_eq!(journey.steps.len(), 2);
    }

    #[test]
    fn wait_for_startup_produces_stable_frame_step() {
        // Arrange / Act
        let journey = Journey::wait_for_startup(300, 5000);

        // Assert
        assert_eq!(journey.name, "wait_for_startup");
        assert_eq!(journey.steps.len(), 1);
        assert!(matches!(
            &journey.steps[0],
            Step::WaitForStableFrame {
                stable_ms: 300,
                timeout_ms: 5000
            }
        ));
    }

    #[test]
    fn navigate_with_key_produces_press_then_wait() {
        // Arrange / Act
        let journey = Journey::navigate_with_key("Tab", "Sessions", 3000);

        // Assert
        assert_eq!(journey.name, "navigate_Tab");
        assert_eq!(journey.steps.len(), 2);
        assert!(matches!(&journey.steps[0], Step::PressKey(key) if key == "Tab"));
        assert!(
            matches!(&journey.steps[1], Step::WaitForText { needle, timeout_ms: 3000 } if needle == "Sessions")
        );
    }

    #[test]
    fn type_and_confirm_produces_write_then_enter() {
        // Arrange / Act
        let journey = Journey::type_and_confirm("hello world");

        // Assert
        assert_eq!(journey.name, "type_and_confirm");
        assert_eq!(journey.steps.len(), 2);
        assert!(matches!(&journey.steps[0], Step::WriteText(text) if text == "hello world"));
        assert!(matches!(&journey.steps[1], Step::PressKey(key) if key == "Enter"));
    }

    #[test]
    fn press_and_wait_produces_key_then_sleep() {
        // Arrange / Act
        let journey = Journey::press_and_wait("Escape", 200);

        // Assert
        assert_eq!(journey.name, "press_Escape");
        assert_eq!(journey.steps.len(), 2);
        assert!(matches!(&journey.steps[0], Step::PressKey(key) if key == "Escape"));
        assert!(
            matches!(&journey.steps[1], Step::Sleep(duration) if *duration == Duration::from_millis(200))
        );
    }

    #[test]
    fn capture_labeled_produces_capture_step() {
        // Arrange / Act
        let journey = Journey::capture_labeled("state", "Current state");

        // Assert
        assert_eq!(journey.name, "capture_state");
        assert_eq!(journey.steps.len(), 1);
        assert!(matches!(
            &journey.steps[0],
            Step::CaptureLabeled { label, description }
            if label == "state" && description == "Current state"
        ));
    }
}
