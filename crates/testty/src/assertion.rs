//! Matcher APIs for terminal frame assertions.
//!
//! Provides structured assertion functions that operate on a
//! [`TerminalFrame`] and produce detailed failure messages including
//! matched rects, expected regions, actual regions, and relevant colors.

use std::fmt::Write;

use crate::frame::{CellColor, TerminalFrame};
use crate::locator::MatchedSpan;
use crate::region::Region;

/// Assert that `needle` appears at least once within `region`.
///
/// # Panics
///
/// Panics with a structured message if the text is not found in the region.
pub fn assert_text_in_region(frame: &TerminalFrame, needle: &str, region: &Region) {
    let matches = frame.find_text_in_region(needle, region);

    assert!(
        !matches.is_empty(),
        "{}",
        format_text_not_found(frame, needle, *region)
    );
}

/// Assert that `needle` does not appear anywhere in the terminal frame.
///
/// # Panics
///
/// Panics with a structured message if the text is found.
pub fn assert_not_visible(frame: &TerminalFrame, needle: &str) {
    let matches = frame.find_text(needle);

    assert!(
        matches.is_empty(),
        "Expected text '{needle}' to NOT be visible, but found {} occurrence(s):\n{}",
        matches.len(),
        format_span_list(&matches)
    );
}

/// Assert that `needle` appears exactly `expected_count` times in the frame.
///
/// # Panics
///
/// Panics if the count does not match.
pub fn assert_match_count(frame: &TerminalFrame, needle: &str, expected_count: usize) {
    let matches = frame.find_text(needle);

    assert_eq!(
        matches.len(),
        expected_count,
        "Expected '{needle}' to appear {expected_count} time(s), but found {}:\n{}",
        matches.len(),
        format_span_list(&matches)
    );
}

/// Assert that the first match of `needle` has the specified foreground color.
///
/// # Panics
///
/// Panics if the text is not found or the color does not match.
pub fn assert_text_has_fg_color(frame: &TerminalFrame, needle: &str, expected_color: &CellColor) {
    let matches = frame.find_text(needle);

    assert!(
        !matches.is_empty(),
        "Cannot check color: text '{needle}' not found in frame"
    );

    let span = &matches[0];

    assert!(
        span.has_fg(expected_color),
        "Text '{needle}' at ({}, {}) has foreground {:?}, expected {:?}",
        span.rect.col,
        span.rect.row,
        span.foreground,
        expected_color
    );
}

/// Assert that the first match of `needle` has the specified background color.
///
/// # Panics
///
/// Panics if the text is not found or the color does not match.
pub fn assert_text_has_bg_color(frame: &TerminalFrame, needle: &str, expected_color: &CellColor) {
    let matches = frame.find_text(needle);

    assert!(
        !matches.is_empty(),
        "Cannot check color: text '{needle}' not found in frame"
    );

    let span = &matches[0];

    assert!(
        span.has_bg(expected_color),
        "Text '{needle}' at ({}, {}) has background {:?}, expected {:?}",
        span.rect.col,
        span.rect.row,
        span.background,
        expected_color
    );
}

/// Assert that the first match of `needle` appears highlighted.
///
/// A span is highlighted if it is bold, inverse, or has a non-default
/// background color.
///
/// # Panics
///
/// Panics if the text is not found or is not highlighted.
pub fn assert_span_is_highlighted(frame: &TerminalFrame, needle: &str) {
    let matches = frame.find_text(needle);

    assert!(
        !matches.is_empty(),
        "Cannot check highlight: text '{needle}' not found in frame"
    );

    let span = &matches[0];

    assert!(
        span.is_highlighted(),
        "Text '{needle}' at ({}, {}) is not highlighted. Style: {:?}, fg: {:?}, bg: {:?}",
        span.rect.col,
        span.rect.row,
        span.style,
        span.foreground,
        span.background
    );
}

/// Assert that the first match of `needle` is NOT highlighted.
///
/// # Panics
///
/// Panics if the text is not found or is highlighted.
pub fn assert_span_is_not_highlighted(frame: &TerminalFrame, needle: &str) {
    let matches = frame.find_text(needle);

    assert!(
        !matches.is_empty(),
        "Cannot check highlight: text '{needle}' not found in frame"
    );

    let span = &matches[0];

    assert!(
        !span.is_highlighted(),
        "Text '{needle}' at ({}, {}) is highlighted but should not be. Style: {:?}, fg: {:?}, bg: \
         {:?}",
        span.rect.col,
        span.rect.row,
        span.style,
        span.foreground,
        span.background
    );
}

/// Format a "text not found" error message with context.
fn format_text_not_found(frame: &TerminalFrame, needle: &str, region: Region) -> String {
    let all_matches = frame.find_text(needle);
    let region_text = frame.text_in_region(&region);
    let mut message = String::new();

    let _ = writeln!(
        message,
        "Text '{needle}' not found in region (col:{}, row:{}, {}x{})",
        region.col, region.row, region.width, region.height
    );

    if all_matches.is_empty() {
        let _ = writeln!(message, "  Text is not visible anywhere in the frame.");
    } else {
        let _ = writeln!(
            message,
            "  Text found {} time(s) outside the region:",
            all_matches.len()
        );
        for span in &all_matches {
            let _ = writeln!(
                message,
                "    - at (col:{}, row:{})",
                span.rect.col, span.rect.row
            );
        }
    }

    let _ = writeln!(message, "  Region content:\n{region_text}");

    message
}

/// Format a list of matched spans for error output.
fn format_span_list(spans: &[MatchedSpan]) -> String {
    let mut output = String::new();

    for span in spans {
        let _ = writeln!(
            output,
            "  - '{}' at (col:{}, row:{}) fg:{:?} bg:{:?} style:{:?}",
            span.text, span.rect.col, span.rect.row, span.foreground, span.background, span.style
        );
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_text_in_region_passes_when_found() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");
        let region = Region::new(0, 0, 80, 1);

        // Act / Assert — should not panic.
        assert_text_in_region(&frame, "Hello", &region);
    }

    #[test]
    #[should_panic(expected = "not found in region")]
    fn assert_text_in_region_panics_when_not_found() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");
        let region = Region::new(20, 0, 60, 1);

        // Act — should panic.
        assert_text_in_region(&frame, "Hello", &region);
    }

    #[test]
    fn assert_not_visible_passes_when_absent() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act / Assert
        assert_not_visible(&frame, "Goodbye");
    }

    #[test]
    #[should_panic(expected = "NOT be visible")]
    fn assert_not_visible_panics_when_present() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act
        assert_not_visible(&frame, "Hello");
    }

    #[test]
    fn assert_match_count_passes_with_correct_count() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"foo bar foo");

        // Act / Assert
        assert_match_count(&frame, "foo", 2);
    }

    #[test]
    #[should_panic(expected = "appear 1 time(s)")]
    fn assert_match_count_panics_with_wrong_count() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"foo bar foo");

        // Act
        assert_match_count(&frame, "foo", 1);
    }

    #[test]
    fn assert_span_is_highlighted_detects_bold() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mBold\x1b[0m");

        // Act / Assert
        assert_span_is_highlighted(&frame, "Bold");
    }

    #[test]
    fn assert_span_is_not_highlighted_for_plain_text() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"plain text");

        // Act / Assert
        assert_span_is_not_highlighted(&frame, "plain");
    }

    #[test]
    fn assert_text_has_fg_color_passes() {
        // Arrange — ANSI red foreground.
        let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

        // Act / Assert
        assert_text_has_fg_color(&frame, "Red", &CellColor::new(128, 0, 0));
    }

    #[test]
    #[should_panic(expected = "foreground")]
    fn assert_text_has_fg_color_panics_on_mismatch() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

        // Act
        assert_text_has_fg_color(&frame, "Red", &CellColor::new(0, 255, 0));
    }
}
