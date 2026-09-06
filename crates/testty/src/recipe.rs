//! Agent-friendly recipe helpers for common TUI assertions.
//!
//! Provides a small set of composable, high-level helpers that wrap raw
//! locators and region checks. These helpers are designed so that AI agents
//! and contributors can write feature-oriented regression tests without
//! rebuilding locator and color logic from scratch.
//!
//! # Layered API
//!
//! Each recipe is exposed in two layers, mirroring the
//! [`crate::assertion`] module:
//!
//! - **`match_*` recipes** return [`MatchResult`] so callers can compose,
//!   retry, accumulate failures via [`crate::assertion::SoftAssertions`], or
//!   surface the failure into a [`crate::proof::report::ProofReport`] without
//!   unwinding.
//! - **`expect_*` recipes** are thin panic adapters that delegate to the
//!   matching `match_*` and panic with the structured failure message. They are
//!   kept for source compatibility with tests that expect panic-on-failure
//!   semantics.
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
use crate::assertion::{AssertionFailure, Expected, MatchResult};
use crate::frame::TerminalFrame;
use crate::region::Region;

/// Match that a tab with the given label exists in the header row and
/// appears highlighted (selected).
///
/// First proves the label exists in the header row via
/// [`assertion::match_text_in_region`], then inspects the highlight state
/// of that header occurrence specifically. The highlight check is scoped
/// to the header region so that a duplicate of the same label rendered
/// elsewhere in the frame cannot mask or fabricate a tab-state failure.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// label is missing from the header row or the header occurrence is not
/// highlighted.
pub fn match_selected_tab(frame: &TerminalFrame, label: &str) -> MatchResult {
    let header = Region::top_row(frame.cols());
    assertion::match_text_in_region(frame, label, &header)?;

    let header_matches = frame.find_text_in_region(label, &header);
    let span = &header_matches[0];

    if !span.is_highlighted() {
        let message = format!(
            "Text '{label}' at ({}, {}) is not highlighted. Style: {:?}, fg: {:?}, bg: {:?}",
            span.rect.col, span.rect.row, span.style, span.foreground, span.background
        );

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::Highlighted {
                needle: label.to_string(),
            },
            region: Some(header),
            matched_spans: header_matches,
            frame_excerpt: frame.all_text(),
        }));
    }

    Ok(())
}

/// Assert that a tab with the given label exists in the header row
/// and appears highlighted (selected).
///
/// Panic adapter for [`match_selected_tab`].
///
/// # Panics
///
/// Panics if the tab label is not found in the top row or is not highlighted.
pub fn expect_selected_tab(frame: &TerminalFrame, label: &str) {
    let result = match_selected_tab(frame, label);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a tab with the given label exists in the header row but is
/// NOT highlighted (not selected).
///
/// First proves the label exists in the header row via
/// [`assertion::match_text_in_region`], then inspects the highlight state
/// of that header occurrence specifically. The highlight check is scoped
/// to the header region so that a duplicate of the same label rendered
/// elsewhere in the frame cannot mask or fabricate a tab-state failure.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// label is missing from the header row or the header occurrence is
/// highlighted.
pub fn match_unselected_tab(frame: &TerminalFrame, label: &str) -> MatchResult {
    let header = Region::top_row(frame.cols());
    assertion::match_text_in_region(frame, label, &header)?;

    let header_matches = frame.find_text_in_region(label, &header);
    let span = &header_matches[0];

    if span.is_highlighted() {
        let message = format!(
            "Text '{label}' at ({}, {}) is highlighted but should not be. Style: {:?}, fg: {:?}, \
             bg: {:?}",
            span.rect.col, span.rect.row, span.style, span.foreground, span.background
        );

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::NotHighlighted {
                needle: label.to_string(),
            },
            region: Some(header),
            matched_spans: header_matches,
            frame_excerpt: frame.all_text(),
        }));
    }

    Ok(())
}

/// Assert that a tab with the given label exists in the header row
/// but is NOT highlighted (not selected).
///
/// Panic adapter for [`match_unselected_tab`].
///
/// # Panics
///
/// Panics if the tab label is not found or is highlighted.
pub fn expect_unselected_tab(frame: &TerminalFrame, label: &str) {
    let result = match_unselected_tab(frame, label);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that an instruction or help text is visible in the frame.
///
/// Checks the full terminal grid since instructions may appear in
/// different areas depending on the application state.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// instruction text is not present anywhere in the frame.
pub fn match_instruction_visible(frame: &TerminalFrame, instruction: &str) -> MatchResult {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, instruction, &full)
}

/// Assert that an instruction or help text is visible in the frame.
///
/// Panic adapter for [`match_instruction_visible`].
///
/// # Panics
///
/// Panics if the instruction text is not found.
pub fn expect_instruction_visible(frame: &TerminalFrame, instruction: &str) {
    let result = match_instruction_visible(frame, instruction);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a keybinding hint appears in the footer row.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// hint is missing from the footer row.
pub fn match_keybinding_hint(frame: &TerminalFrame, hint: &str) -> MatchResult {
    let footer = Region::footer(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, hint, &footer)
}

/// Assert that a keybinding hint appears in the footer row.
///
/// Panic adapter for [`match_keybinding_hint`].
///
/// # Panics
///
/// Panics if the hint text is not found in the footer.
pub fn expect_keybinding_hint(frame: &TerminalFrame, hint: &str) {
    let result = match_keybinding_hint(frame, hint);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a labeled action appears in the footer row.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// action label is missing from the footer row.
pub fn match_footer_action(frame: &TerminalFrame, action: &str) -> MatchResult {
    let footer = Region::footer(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, action, &footer)
}

/// Assert that a labeled action appears in the footer row.
///
/// Panic adapter for [`match_footer_action`].
///
/// # Panics
///
/// Panics if the action label is not found in the footer.
pub fn expect_footer_action(frame: &TerminalFrame, action: &str) {
    let result = match_footer_action(frame, action);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a dialog title appears in the terminal.
///
/// Searches the upper portion of the terminal (top 60%) since dialogs
/// typically render as centered overlays.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// title text is not present in the upper 60% of the frame.
pub fn match_dialog_title(frame: &TerminalFrame, title: &str) -> MatchResult {
    let upper = Region::percent(0, 0, 100, 60, frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, title, &upper)
}

/// Assert that a dialog title appears in the terminal.
///
/// Panic adapter for [`match_dialog_title`].
///
/// # Panics
///
/// Panics if the title text is not found.
pub fn expect_dialog_title(frame: &TerminalFrame, title: &str) {
    let result = match_dialog_title(frame, title);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a status message is visible anywhere in the frame.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// message text is not present anywhere in the frame.
pub fn match_status_message(frame: &TerminalFrame, message: &str) -> MatchResult {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, message, &full)
}

/// Assert that a status message is visible anywhere in the frame.
///
/// Panic adapter for [`match_status_message`].
///
/// # Panics
///
/// Panics if the status message is not found.
pub fn expect_status_message(frame: &TerminalFrame, message: &str) {
    let result = match_status_message(frame, message);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a specific text is NOT visible anywhere in the frame.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// text appears at least once in the frame.
pub fn match_not_visible(frame: &TerminalFrame, text: &str) -> MatchResult {
    assertion::match_not_visible(frame, text)
}

/// Assert that a specific text is NOT visible anywhere in the frame.
///
/// Panic adapter for [`match_not_visible`].
///
/// # Panics
///
/// Panics if the text is found.
pub fn expect_not_visible(frame: &TerminalFrame, text: &str) {
    let result = match_not_visible(frame, text);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
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

    #[test]
    fn match_selected_tab_returns_ok_for_bold_tab() {
        // Arrange — bold "Projects" in the first row.
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

        // Act
        let result = match_selected_tab(&frame, "Projects");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_selected_tab_returns_failure_when_not_highlighted() {
        // Arrange — plain text in the header.
        let frame = TerminalFrame::new(80, 24, b"Projects  Sessions");

        // Act
        let failure = match_selected_tab(&frame, "Projects").expect_err("should be Err");

        // Assert — composition surfaces the highlight check failure.
        assert!(matches!(
            &failure.expected,
            crate::assertion::Expected::Highlighted { needle, .. } if needle == "Projects"
        ));
    }

    #[test]
    fn match_selected_tab_returns_failure_when_label_missing_from_header() {
        // Arrange — label not present in the top row.
        let frame = TerminalFrame::new(80, 24, b"Other  Tabs");

        // Act
        let failure = match_selected_tab(&frame, "Projects").expect_err("should be Err");

        // Assert — composition stops at the region check before reaching
        // highlight.
        assert!(matches!(
            &failure.expected,
            crate::assertion::Expected::TextInRegion { needle, .. } if needle == "Projects"
        ));
    }

    #[test]
    fn match_unselected_tab_returns_ok_for_plain_tab() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

        // Act
        let result = match_unselected_tab(&frame, "Sessions");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_unselected_tab_returns_failure_when_highlighted() {
        // Arrange — bold tab fails the unselected check.
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

        // Act
        let failure = match_unselected_tab(&frame, "Projects").expect_err("should be Err");

        // Assert
        assert!(matches!(
            &failure.expected,
            crate::assertion::Expected::NotHighlighted { needle, .. } if needle == "Projects"
        ));
    }

    #[test]
    fn match_instruction_visible_returns_failure_when_absent() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello");

        // Act
        let failure = match_instruction_visible(&frame, "Press Enter").expect_err("should be Err");

        // Assert
        assert!(matches!(
            &failure.expected,
            crate::assertion::Expected::TextInRegion { needle, .. } if needle == "Press Enter"
        ));
    }

    #[test]
    fn match_keybinding_hint_returns_ok_when_in_footer() {
        // Arrange — write "Tab" on the last row.
        let mut data = Vec::new();
        for _ in 0..23 {
            data.extend_from_slice(b"\r\n");
        }
        data.extend_from_slice(b"Tab Enter q");
        let frame = TerminalFrame::new(80, 24, &data);

        // Act
        let result = match_keybinding_hint(&frame, "Tab");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_footer_action_returns_failure_when_missing() {
        // Arrange — empty footer.
        let frame = TerminalFrame::new(80, 24, b"Header text");

        // Act
        let failure = match_footer_action(&frame, "Quit").expect_err("should be Err");

        // Assert
        assert!(matches!(
            &failure.expected,
            crate::assertion::Expected::TextInRegion { needle, .. } if needle == "Quit"
        ));
    }

    #[test]
    fn match_dialog_title_returns_ok_when_in_upper_region() {
        // Arrange — title near the top of the frame.
        let frame = TerminalFrame::new(80, 24, b"\r\n\r\nConfirm Delete");

        // Act
        let result = match_dialog_title(&frame, "Confirm Delete");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_status_message_returns_ok_when_present() {
        // Arrange
        let mut data = Vec::new();
        for _ in 0..10 {
            data.extend_from_slice(b"\r\n");
        }
        data.extend_from_slice(b"Status: OK");
        let frame = TerminalFrame::new(80, 24, &data);

        // Act
        let result = match_status_message(&frame, "Status: OK");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_not_visible_returns_failure_when_present() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act
        let failure = match_not_visible(&frame, "Hello").expect_err("should be Err");

        // Assert
        assert!(matches!(
            &failure.expected,
            crate::assertion::Expected::NotVisible { needle, .. } if needle == "Hello"
        ));
    }

    #[test]
    fn match_selected_tab_inspects_header_occurrence_when_label_repeats_in_body() {
        // Arrange — bold "Projects" tab in the header row, plus a plain
        // (unhighlighted) "Projects" later in the body. The recipe must
        // judge the header occurrence specifically.
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"\x1b[1mProjects\x1b[0m  Sessions");
        for _ in 0..6 {
            data.extend_from_slice(b"\r\n");
        }
        data.extend_from_slice(b"Projects mentioned here is plain body text");
        let frame = TerminalFrame::new(80, 24, &data);

        // Act
        let result = match_selected_tab(&frame, "Projects");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_unselected_tab_inspects_header_occurrence_when_label_repeats_in_body() {
        // Arrange — plain "Sessions" tab in the header row, plus a bold
        // "Sessions" later in the body. The recipe must judge the header
        // occurrence specifically rather than the highlighted body match.
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"Projects  Sessions");
        for _ in 0..6 {
            data.extend_from_slice(b"\r\n");
        }
        data.extend_from_slice(b"\x1b[1mSessions\x1b[0m heading");
        let frame = TerminalFrame::new(80, 24, &data);

        // Act
        let result = match_unselected_tab(&frame, "Sessions");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_selected_tab_failure_is_scoped_to_header_region() {
        // Arrange — plain "Projects" in the header is the occurrence the
        // recipe must judge, even though a bold "Projects" appears later
        // in the body.
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"Projects  Sessions");
        for _ in 0..6 {
            data.extend_from_slice(b"\r\n");
        }
        data.extend_from_slice(b"\x1b[1mProjects\x1b[0m elsewhere");
        let frame = TerminalFrame::new(80, 24, &data);

        // Act
        let failure = match_selected_tab(&frame, "Projects").expect_err("should be Err");

        // Assert — failure carries the header region and the matched span
        // sits in row 0, proving the check is scoped to the header.
        let header = Region::top_row(frame.cols());
        assert_eq!(failure.region, Some(header));
        assert_eq!(failure.matched_spans.len(), 1);
        assert_eq!(failure.matched_spans[0].rect.row, 0);
    }

    #[test]
    fn match_unselected_tab_failure_is_scoped_to_header_region() {
        // Arrange — bold "Sessions" in the header is the occurrence the
        // recipe must reject, even though a plain "Sessions" appears
        // later in the body.
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"Projects  \x1b[1mSessions\x1b[0m");
        for _ in 0..6 {
            data.extend_from_slice(b"\r\n");
        }
        data.extend_from_slice(b"Sessions elsewhere");
        let frame = TerminalFrame::new(80, 24, &data);

        // Act
        let failure = match_unselected_tab(&frame, "Sessions").expect_err("should be Err");

        // Assert — failure carries the header region and the matched span
        // sits in row 0, proving the check is scoped to the header.
        let header = Region::top_row(frame.cols());
        assert_eq!(failure.region, Some(header));
        assert_eq!(failure.matched_spans.len(), 1);
        assert_eq!(failure.matched_spans[0].rect.row, 0);
    }

    #[test]
    fn match_recipes_compose_with_soft_assertions() {
        // Arrange — frame missing both the expected tab and the footer hint.
        let frame = TerminalFrame::new(80, 24, b"Other Header");

        // Act — accumulate every failure across composed recipes without
        // failing fast, then drain to suppress the drop-time panic.
        let mut soft = crate::assertion::SoftAssertions::new();
        soft.check(match_selected_tab(&frame, "Projects"));
        soft.check(match_keybinding_hint(&frame, "Quit"));
        let failures = soft.into_failures();

        // Assert — both recipe failures land in the accumulator.
        assert_eq!(failures.len(), 2);
    }
}
