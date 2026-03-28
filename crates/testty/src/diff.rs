//! Frame diffing engine for comparing terminal states.
//!
//! [`FrameDiff`] computes cell-level differences between two
//! [`TerminalFrame`] snapshots, groups changes into readable regions,
//! and generates human-readable summaries. Diffs are automatically
//! computed between consecutive captures in a
//! [`ProofReport`](crate::proof::report::ProofReport).

use std::fmt::Write;

use crate::frame::TerminalFrame;
use crate::region::Region;

/// Describes what changed in a single terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellChange {
    /// No change between before and after.
    Unchanged,
    /// The cell's text content changed.
    TextChanged,
    /// The cell's style or color changed but text stayed the same.
    StyleChanged,
    /// Both text and style/color changed.
    BothChanged,
}

/// A contiguous span of changed cells on a single row.
#[derive(Debug, Clone)]
pub struct ChangedRegion {
    /// The bounding rectangle of the changed cells.
    pub region: Region,
    /// The type of change observed in this span.
    pub change_type: CellChange,
}

/// Cell-level diff between two terminal frames.
///
/// Compute with [`FrameDiff::compute()`], then inspect individual cells,
/// extract changed regions, or generate human-readable summaries.
#[derive(Debug, Clone)]
pub struct FrameDiff {
    /// Grid of per-cell change types, stored row-major.
    changes: Vec<Vec<CellChange>>,
    /// Number of columns in the diff grid.
    cols: u16,
    /// Number of rows in the diff grid.
    rows: u16,
}

impl FrameDiff {
    /// Return the number of columns in the diff grid.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Return the number of rows in the diff grid.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Compute a cell-level diff between two terminal frames.
    ///
    /// Compares text, foreground color, background color, and style for
    /// each cell. The grid dimensions cover the union of both frames;
    /// cells outside either frame's bounds are treated as changed.
    pub fn compute(before: &TerminalFrame, after: &TerminalFrame) -> Self {
        let cols = before.cols().max(after.cols());
        let rows = before.rows().max(after.rows());
        let mut changes = Vec::with_capacity(usize::from(rows));

        for row in 0..rows {
            let mut row_changes = Vec::with_capacity(usize::from(cols));

            for col in 0..cols {
                let change = compare_cell(before, after, row, col);
                row_changes.push(change);
            }

            changes.push(row_changes);
        }

        Self {
            changes,
            cols,
            rows,
        }
    }

    /// Return the change type for a specific cell.
    ///
    /// Returns `None` if the coordinates are out of bounds.
    pub fn cell_change(&self, row: u16, col: u16) -> Option<CellChange> {
        self.changes
            .get(usize::from(row))
            .and_then(|row_changes| row_changes.get(usize::from(col)).copied())
    }

    /// Whether the two frames are identical (no changes detected).
    pub fn is_identical(&self) -> bool {
        self.changes
            .iter()
            .all(|row| row.iter().all(|change| *change == CellChange::Unchanged))
    }

    /// Group adjacent changed cells on the same row into [`ChangedRegion`]
    /// spans.
    pub fn changed_regions(&self) -> Vec<ChangedRegion> {
        let mut regions = Vec::new();

        for (row_index, row_changes) in self.changes.iter().enumerate() {
            let row = u16::try_from(row_index).unwrap_or(0);
            let mut span_start: Option<(u16, CellChange)> = None;

            for (col_index, &change) in row_changes.iter().enumerate() {
                let col = u16::try_from(col_index).unwrap_or(0);

                if change == CellChange::Unchanged {
                    if let Some((start_col, change_type)) = span_start.take() {
                        regions.push(ChangedRegion {
                            region: Region::new(start_col, row, col - start_col, 1),
                            change_type,
                        });
                    }
                } else if let Some((_, ref mut current_type)) = span_start {
                    // Merge adjacent changes; upgrade to BothChanged if types differ.
                    if *current_type != change {
                        *current_type = CellChange::BothChanged;
                    }
                } else {
                    span_start = Some((col, change));
                }
            }

            // Close any trailing span.
            if let Some((start_col, change_type)) = span_start {
                regions.push(ChangedRegion {
                    region: Region::new(start_col, row, self.cols - start_col, 1),
                    change_type,
                });
            }
        }

        regions
    }

    /// Generate human-readable change descriptions.
    ///
    /// Each entry describes one changed region with its row, column range,
    /// and change type (e.g., "row 0, cols 1-9: text changed").
    pub fn summary(&self) -> Vec<String> {
        self.changed_regions()
            .iter()
            .map(|changed_region| {
                let row = changed_region.region.row;
                let start_col = changed_region.region.col;
                let end_col = start_col + changed_region.region.width - 1;
                let change_label = match changed_region.change_type {
                    CellChange::TextChanged => "text changed",
                    CellChange::StyleChanged => "style changed",
                    CellChange::BothChanged => "text and style changed",
                    CellChange::Unchanged => "unchanged",
                };

                let mut description = String::new();
                if start_col == end_col {
                    let _ = write!(description, "row {row}, col {start_col}: {change_label}");
                } else {
                    let _ = write!(
                        description,
                        "row {row}, cols {start_col}-{end_col}: {change_label}"
                    );
                }

                description
            })
            .collect()
    }
}

/// Compare a single cell between two frames.
///
/// Uses [`TerminalFrame::cell_text()`] for zero-allocation text comparison
/// instead of the heavier [`TerminalFrame::text_in_region()`] path.
fn compare_cell(before: &TerminalFrame, after: &TerminalFrame, row: u16, col: u16) -> CellChange {
    let before_in_bounds = row < before.rows() && col < before.cols();
    let after_in_bounds = row < after.rows() && col < after.cols();

    // If the cell is outside either frame, treat as changed.
    if !before_in_bounds || !after_in_bounds {
        return CellChange::BothChanged;
    }

    let text_changed = before.cell_text(row, col) != after.cell_text(row, col);

    let style_changed = before.fg_color(row, col) != after.fg_color(row, col)
        || before.bg_color(row, col) != after.bg_color(row, col)
        || before.cell_style(row, col) != after.cell_style(row, col);

    match (text_changed, style_changed) {
        (false, false) => CellChange::Unchanged,
        (true, false) => CellChange::TextChanged,
        (false, true) => CellChange::StyleChanged,
        (true, true) => CellChange::BothChanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_frames_produce_no_changes() {
        // Arrange
        let data = b"Hello, World!";
        let frame_a = TerminalFrame::new(80, 24, data);
        let frame_b = TerminalFrame::new(80, 24, data);

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);

        // Assert
        assert!(diff.is_identical());
        assert!(diff.changed_regions().is_empty());
        assert!(diff.summary().is_empty());
    }

    #[test]
    fn single_cell_text_change_detected() {
        // Arrange
        let frame_a = TerminalFrame::new(80, 24, b"ABC");
        let frame_b = TerminalFrame::new(80, 24, b"AXC");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);

        // Assert
        assert!(!diff.is_identical());
        assert_eq!(diff.cell_change(0, 0), Some(CellChange::Unchanged));
        assert_eq!(diff.cell_change(0, 1), Some(CellChange::TextChanged));
        assert_eq!(diff.cell_change(0, 2), Some(CellChange::Unchanged));
    }

    #[test]
    fn adjacent_changes_merge_into_region() {
        // Arrange
        let frame_a = TerminalFrame::new(80, 24, b"AAAAAA");
        let frame_b = TerminalFrame::new(80, 24, b"ABBBBA");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);
        let regions = diff.changed_regions();

        // Assert — four adjacent changed cells should merge.
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].region.col, 1);
        assert_eq!(regions[0].region.width, 4);
        assert_eq!(regions[0].change_type, CellChange::TextChanged);
    }

    #[test]
    fn summary_formats_human_readable_text() {
        // Arrange
        let frame_a = TerminalFrame::new(80, 24, b"ABC");
        let frame_b = TerminalFrame::new(80, 24, b"AXC");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);
        let summary = diff.summary();

        // Assert
        assert_eq!(summary.len(), 1);
        assert!(summary[0].contains("row 0"));
        assert!(summary[0].contains("col 1"));
        assert!(summary[0].contains("text changed"));
    }

    #[test]
    fn style_change_detected() {
        // Arrange — same text but different style.
        let frame_a = TerminalFrame::new(80, 24, b"A");
        let frame_b = TerminalFrame::new(80, 24, b"\x1b[1mA\x1b[0m");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);

        // Assert
        assert!(!diff.is_identical());
        assert_eq!(diff.cell_change(0, 0), Some(CellChange::StyleChanged));
    }

    #[test]
    fn out_of_bounds_cell_returns_none() {
        // Arrange
        let frame = TerminalFrame::new(10, 5, b"Hi");
        let diff = FrameDiff::compute(&frame, &frame);

        // Act / Assert
        assert!(diff.cell_change(100, 100).is_none());
    }

    #[test]
    fn different_size_frames_mark_extra_cells() {
        // Arrange — after frame is wider.
        let frame_a = TerminalFrame::new(5, 1, b"Hello");
        let frame_b = TerminalFrame::new(10, 1, b"Hello");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);

        // Assert — first 5 cols unchanged, cols 5-9 are "out of bounds" = changed.
        assert_eq!(diff.cell_change(0, 0), Some(CellChange::Unchanged));
        assert_eq!(diff.cell_change(0, 5), Some(CellChange::BothChanged));
    }

    #[test]
    fn shrunk_frame_marks_removed_cols_as_changed() {
        // Arrange — after frame is narrower than before.
        let frame_a = TerminalFrame::new(10, 1, b"HelloWorld");
        let frame_b = TerminalFrame::new(5, 1, b"Hello");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);

        // Assert — grid covers the wider frame; removed cols are changed.
        assert_eq!(diff.cols(), 10);
        assert_eq!(diff.cell_change(0, 0), Some(CellChange::Unchanged));
        assert_eq!(diff.cell_change(0, 4), Some(CellChange::Unchanged));
        assert_eq!(diff.cell_change(0, 5), Some(CellChange::BothChanged));
        assert_eq!(diff.cell_change(0, 9), Some(CellChange::BothChanged));
    }

    #[test]
    fn shrunk_frame_marks_removed_rows_as_changed() {
        // Arrange — after frame has fewer rows.
        let frame_a = TerminalFrame::new(5, 3, b"A\nB\nC");
        let frame_b = TerminalFrame::new(5, 1, b"A");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);

        // Assert — grid covers all 3 rows; removed rows are changed.
        assert_eq!(diff.rows(), 3);
        assert!(!diff.is_identical());
        assert_eq!(diff.cell_change(1, 0), Some(CellChange::BothChanged));
        assert_eq!(diff.cell_change(2, 0), Some(CellChange::BothChanged));
    }

    #[test]
    fn summary_single_col_format() {
        // Arrange — change a single cell.
        let frame_a = TerminalFrame::new(80, 24, b"A");
        let frame_b = TerminalFrame::new(80, 24, b"B");

        // Act
        let diff = FrameDiff::compute(&frame_a, &frame_b);
        let summary = diff.summary();

        // Assert — single column should say "col X" not "cols X-X".
        assert_eq!(summary.len(), 1);
        assert!(summary[0].contains("col 0:"));
    }
}
