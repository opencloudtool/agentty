//! Text locators for finding and describing UI elements in the terminal grid.
//!
//! A [`MatchedSpan`] describes a contiguous run of styled text found at a
//! specific position in the terminal. Locators combine text search with
//! style and color filtering to identify TUI "controls" such as tabs,
//! buttons, and highlighted labels.

use crate::frame::{CellColor, CellStyle};
use crate::region::Region;

/// A matched span of text found in the terminal grid.
///
/// Contains the text content, its bounding rectangle, and the style and
/// color information extracted from the first cell. Spans are always
/// single-row since terminal text does not wrap across rows for matching
/// purposes.
#[derive(Debug, Clone)]
pub struct MatchedSpan {
    /// The text content of the matched span.
    pub text: String,
    /// The bounding rectangle in terminal cell coordinates.
    pub rect: Region,
    /// Foreground color of the first cell, or `None` for terminal default.
    pub foreground: Option<CellColor>,
    /// Background color of the first cell, or `None` for terminal default.
    pub background: Option<CellColor>,
    /// Style flags of the first cell.
    pub style: CellStyle,
}

impl MatchedSpan {
    /// Check whether this span has a specific foreground color.
    pub fn has_fg(&self, color: &CellColor) -> bool {
        self.foreground.as_ref() == Some(color)
    }

    /// Check whether this span has a specific background color.
    pub fn has_bg(&self, color: &CellColor) -> bool {
        self.background.as_ref() == Some(color)
    }

    /// Check whether this span is rendered bold.
    pub fn is_bold(&self) -> bool {
        self.style.bold()
    }

    /// Check whether this span is rendered with inverse colors.
    pub fn is_inverse(&self) -> bool {
        self.style.inverse()
    }

    /// Check whether this span appears visually highlighted.
    ///
    /// A span is considered highlighted if it is bold, inverse, or has a
    /// non-default background color.
    pub fn is_highlighted(&self) -> bool {
        self.style.bold() || self.style.inverse() || self.background.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_span() -> MatchedSpan {
        MatchedSpan {
            text: "Tab".to_string(),
            rect: Region::new(5, 0, 3, 1),
            foreground: Some(CellColor::white()),
            background: Some(CellColor::new(0, 0, 128)),
            style: CellStyle::from_raw(0x01),
        }
    }

    #[test]
    fn has_fg_matches_exact_color() {
        // Arrange
        let span = sample_span();

        // Act / Assert
        assert!(span.has_fg(&CellColor::white()));
        assert!(!span.has_fg(&CellColor::black()));
    }

    #[test]
    fn has_bg_matches_exact_color() {
        // Arrange
        let span = sample_span();

        // Act / Assert
        assert!(span.has_bg(&CellColor::new(0, 0, 128)));
        assert!(!span.has_bg(&CellColor::white()));
    }

    #[test]
    fn is_highlighted_detects_bold() {
        // Arrange
        let span = sample_span();

        // Act / Assert
        assert!(span.is_highlighted());
        assert!(span.is_bold());
    }

    #[test]
    fn is_highlighted_detects_background_color() {
        // Arrange
        let span = MatchedSpan {
            text: "item".to_string(),
            rect: Region::new(0, 0, 4, 1),
            foreground: None,
            background: Some(CellColor::new(50, 50, 50)),
            style: CellStyle::default(),
        };

        // Act / Assert
        assert!(span.is_highlighted());
    }

    #[test]
    fn not_highlighted_when_plain() {
        // Arrange
        let span = MatchedSpan {
            text: "plain".to_string(),
            rect: Region::new(0, 0, 5, 1),
            foreground: None,
            background: None,
            style: CellStyle::default(),
        };

        // Act / Assert
        assert!(!span.is_highlighted());
    }
}
