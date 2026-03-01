//! Backward-compatible re-exports for UI utilities.
//!
//! New code should import from focused modules:
//! - `ui::layout`
//! - `ui::diff_util`
//! - `ui::text_util`
//! - `ui::activity_heatmap`

pub use crate::ui::activity_heatmap::{
    activity_day_key, activity_day_key_local, activity_day_key_with_offset,
    build_activity_heatmap_grid, build_heatmap_month_row, build_visible_heatmap_month_row,
    current_day_key_local, current_day_key_utc, heatmap_intensity_level, heatmap_max_count,
    heatmap_month_markers, visible_heatmap_week_count,
};
pub use crate::ui::diff_util::{
    DiffLine, DiffLineKind, diff_line_change_totals, max_diff_line_number, parse_diff_lines,
    parse_hunk_header, wrap_diff_content,
};
pub use crate::ui::layout::{
    CHAT_INPUT_MAX_VISIBLE_LINES, calculate_input_height, calculate_input_viewport,
    centered_horizontal_layout, compute_input_layout, first_table_column_width,
    move_input_cursor_down, move_input_cursor_up,
};
pub use crate::ui::text_util::{
    format_duration_compact, format_token_count, truncate_with_ellipsis, wrap_lines,
    wrap_styled_line,
};
