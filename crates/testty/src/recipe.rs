//! Agent-friendly recipe helpers for common TUI assertions.
//!
//! Provides a small set of composable, high-level helpers that wrap raw
//! locators and region checks. These helpers are designed so that AI agents
//! and contributors can write feature-oriented regression tests without
//! rebuilding locator and color logic from scratch.
//!
//! # Vocabulary
//!
//! - **Tab**: A labeled text span in the header row, where the selected tab
//!   appears highlighted (bold, inverse, or non-default background).
//! - **Instruction**: Visible help text or description in a designated area.
//! - **Keybinding hint**: A compact label like `Tab`, `Enter`, `q` in the
//!   footer area describing available actions.
//! - **Footer action**: A labeled action in the bottom row.
//! - **Dialog title**: A centered heading in a modal-like area.
//! - **Status message**: A transient notification in a specific region.

use crate::assertion;
use crate::frame::TerminalFrame;
use crate::region::Region;

/// Assert that a tab with the given label exists in the header row
/// and appears highlighted (selected).
///
/// # Panics
///
/// Panics if the tab label is not found in the top row or is not highlighted.
pub fn expect_selected_tab(frame: &TerminalFrame, label: &str) {
    let header = Region::top_row(frame.cols());
    assertion::assert_text_in_region(frame, label, &header);
    assertion::assert_span_is_highlighted(frame, label);
}

/// Assert that a tab with the given label exists in the header row
/// but is NOT highlighted (not selected).
///
/// # Panics
///
/// Panics if the tab label is not found or is highlighted.
pub fn expect_unselected_tab(frame: &TerminalFrame, label: &str) {
    let header = Region::top_row(frame.cols());
    assertion::assert_text_in_region(frame, label, &header);
    assertion::assert_span_is_not_highlighted(frame, label);
}

/// Assert that an instruction or help text is visible in the frame.
///
/// Checks the full terminal grid since instructions may appear in
/// different areas depending on the application state.
///
/// # Panics
///
/// Panics if the instruction text is not found.
pub fn expect_instruction_visible(frame: &TerminalFrame, instruction: &str) {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, instruction, &full);
}

/// Assert that a keybinding hint appears in the footer row.
///
/// # Panics
///
/// Panics if the hint text is not found in the footer.
pub fn expect_keybinding_hint(frame: &TerminalFrame, hint: &str) {
    let footer = Region::footer(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, hint, &footer);
}

/// Assert that a labeled action appears in the footer row.
///
/// # Panics
///
/// Panics if the action label is not found in the footer.
pub fn expect_footer_action(frame: &TerminalFrame, action: &str) {
    let footer = Region::footer(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, action, &footer);
}

/// Assert that a dialog title appears in the terminal.
///
/// Searches the upper portion of the terminal (top 60%) since dialogs
/// typically render as centered overlays.
///
/// # Panics
///
/// Panics if the title text is not found.
pub fn expect_dialog_title(frame: &TerminalFrame, title: &str) {
    let upper = Region::percent(0, 0, 100, 60, frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, title, &upper);
}

/// Assert that a status message is visible anywhere in the frame.
///
/// # Panics
///
/// Panics if the status message is not found.
pub fn expect_status_message(frame: &TerminalFrame, message: &str) {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, message, &full);
}

/// Assert that a specific text is NOT visible anywhere in the frame.
///
/// # Panics
///
/// Panics if the text is found.
pub fn expect_not_visible(frame: &TerminalFrame, text: &str) {
    assertion::assert_not_visible(frame, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expect_selected_tab_passes_for_bold_tab() {
        // Arrange — bold "Projects" in the first row.
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

        // Act / Assert
        expect_selected_tab(&frame, "Projects");
    }

    #[test]
    #[should_panic(expected = "is not highlighted")]
    fn expect_selected_tab_panics_when_not_highlighted() {
        // Arrange — plain text.
        let frame = TerminalFrame::new(80, 24, b"Projects  Sessions");

        // Act
        expect_selected_tab(&frame, "Projects");
    }

    #[test]
    fn expect_unselected_tab_passes_for_plain_tab() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

        // Act / Assert
        expect_unselected_tab(&frame, "Sessions");
    }

    #[test]
    fn expect_instruction_visible_finds_text() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\r\n\r\nPress Enter to continue");

        // Act / Assert
        expect_instruction_visible(&frame, "Press Enter");
    }

    #[test]
    fn expect_not_visible_passes_when_absent() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act / Assert
        expect_not_visible(&frame, "Goodbye");
    }

    #[test]
    fn expect_status_message_finds_anywhere() {
        // Arrange
        let mut data = Vec::new();
        for _ in 0..10 {
            data.extend_from_slice(b"\r\n");
        }
        data.extend_from_slice(b"Status: OK");
        let frame = TerminalFrame::new(80, 24, &data);

        // Act / Assert
        expect_status_message(&frame, "Status: OK");
    }
}
