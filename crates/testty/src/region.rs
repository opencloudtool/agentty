//! Terminal region definitions for location-aware assertions.
//!
//! A [`Region`] describes a rectangular area of the terminal grid using
//! zero-based cell coordinates. Named constructors provide common anchors
//! such as [`top_row`](Region::top_row), [`footer`](Region::footer), and
//! percentage-based rectangles relative to the terminal dimensions.

/// A rectangular region in terminal cell coordinates.
///
/// All coordinates are zero-based. `col` and `row` specify the top-left
/// corner; `width` and `height` specify the extent in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Column of the top-left corner (zero-based).
    pub col: u16,
    /// Row of the top-left corner (zero-based).
    pub row: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

impl Region {
    /// Create a region from explicit cell coordinates.
    pub fn new(col: u16, row: u16, width: u16, height: u16) -> Self {
        Self {
            col,
            row,
            width,
            height,
        }
    }

    /// The entire terminal grid.
    pub fn full(cols: u16, rows: u16) -> Self {
        Self::new(0, 0, cols, rows)
    }

    /// The first row across the full width.
    pub fn top_row(cols: u16) -> Self {
        Self::new(0, 0, cols, 1)
    }

    /// The last row across the full width.
    pub fn footer(cols: u16, rows: u16) -> Self {
        Self::new(0, rows.saturating_sub(1), cols, 1)
    }

    /// The top-left quadrant.
    pub fn top_left(cols: u16, rows: u16) -> Self {
        Self::new(0, 0, cols / 2, rows / 2)
    }

    /// The top-right quadrant.
    pub fn top_right(cols: u16, rows: u16) -> Self {
        let half_cols = cols / 2;

        Self::new(half_cols, 0, cols - half_cols, rows / 2)
    }

    /// The left panel (full height, left half).
    pub fn left_panel(cols: u16, rows: u16) -> Self {
        Self::new(0, 0, cols / 2, rows)
    }

    /// The right panel (full height, right half).
    pub fn right_panel(cols: u16, rows: u16) -> Self {
        let half_cols = cols / 2;

        Self::new(half_cols, 0, cols - half_cols, rows)
    }

    /// A percentage-based region relative to terminal dimensions.
    ///
    /// Each parameter is a percentage (0–100). Values are clamped to the
    /// terminal bounds.
    pub fn percent(
        col_pct: u16,
        row_pct: u16,
        width_pct: u16,
        height_pct: u16,
        cols: u16,
        rows: u16,
    ) -> Self {
        let col = u16::try_from(u32::from(col_pct) * u32::from(cols) / 100).unwrap_or(u16::MAX);
        let row = u16::try_from(u32::from(row_pct) * u32::from(rows) / 100).unwrap_or(u16::MAX);
        let width = u16::try_from(u32::from(width_pct) * u32::from(cols) / 100).unwrap_or(u16::MAX);
        let height =
            u16::try_from(u32::from(height_pct) * u32::from(rows) / 100).unwrap_or(u16::MAX);

        Self::new(
            col.min(cols),
            row.min(rows),
            width.min(cols.saturating_sub(col)),
            height.min(rows.saturating_sub(row)),
        )
    }

    /// Check whether a cell coordinate falls inside this region.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.col
            && col < self.col.saturating_add(self.width)
            && row >= self.row
            && row < self.row.saturating_add(self.height)
    }

    /// Check whether `other` is fully contained within `self`.
    pub fn encloses(&self, other: &Region) -> bool {
        other.col >= self.col
            && other.row >= self.row
            && other.col.saturating_add(other.width) <= self.col.saturating_add(self.width)
            && other.row.saturating_add(other.height) <= self.row.saturating_add(self.height)
    }

    /// Return the rightmost column (exclusive).
    pub fn right(&self) -> u16 {
        self.col.saturating_add(self.width)
    }

    /// Return the bottom row (exclusive).
    pub fn bottom(&self) -> u16 {
        self.row.saturating_add(self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_region_covers_entire_grid() {
        // Arrange
        let cols = 80;
        let rows = 24;

        // Act
        let region = Region::full(cols, rows);

        // Assert
        assert_eq!(region, Region::new(0, 0, 80, 24));
    }

    #[test]
    fn top_row_spans_full_width_one_row() {
        // Arrange / Act
        let region = Region::top_row(120);

        // Assert
        assert_eq!(region.height, 1);
        assert_eq!(region.width, 120);
        assert_eq!(region.row, 0);
    }

    #[test]
    fn footer_is_last_row() {
        // Arrange / Act
        let region = Region::footer(80, 24);

        // Assert
        assert_eq!(region.row, 23);
        assert_eq!(region.height, 1);
    }

    #[test]
    fn contains_checks_bounds() {
        // Arrange
        let region = Region::new(10, 5, 20, 10);

        // Act / Assert
        assert!(region.contains(10, 5));
        assert!(region.contains(29, 14));
        assert!(!region.contains(9, 5));
        assert!(!region.contains(30, 5));
        assert!(!region.contains(10, 15));
    }

    #[test]
    fn encloses_checks_full_containment() {
        // Arrange
        let outer = Region::new(0, 0, 80, 24);
        let inner = Region::new(10, 5, 20, 10);
        let outside = Region::new(70, 20, 20, 10);

        // Act / Assert
        assert!(outer.encloses(&inner));
        assert!(!outer.encloses(&outside));
        assert!(!inner.encloses(&outer));
    }

    #[test]
    fn percent_region_computes_correctly() {
        // Arrange / Act
        let region = Region::percent(50, 0, 50, 100, 80, 24);

        // Assert
        assert_eq!(region.col, 40);
        assert_eq!(region.row, 0);
        assert_eq!(region.width, 40);
        assert_eq!(region.height, 24);
    }

    #[test]
    fn top_right_covers_right_half() {
        // Arrange / Act
        let region = Region::top_right(80, 24);

        // Assert
        assert_eq!(region.col, 40);
        assert_eq!(region.row, 0);
        assert_eq!(region.width, 40);
        assert_eq!(region.height, 12);
    }
}
