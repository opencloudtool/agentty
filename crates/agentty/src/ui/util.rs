use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use time::{OffsetDateTime, UtcOffset};

use crate::domain::session::DailyActivity;

const HEATMAP_DAY_COUNT: usize = 7;
const HEATMAP_DAY_COUNT_I64: i64 = 7;
const HEATMAP_WEEK_COUNT: usize = 53;
const HEATMAP_WEEK_COUNT_I64: i64 = 53;
const HEATMAP_MONTH_LABELS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const SECONDS_PER_DAY: i64 = 86_400;

/// Split an area into a centered content column with side gutters.
pub fn centered_horizontal_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(2),
            Constraint::Percentage(80),
            Constraint::Min(2),
        ])
        .split(area)
}

/// Calculate the full chat input widget height, including borders and prompt
/// padding.
pub fn calculate_input_height(width: u16, input: &str) -> u16 {
    let char_count = input.chars().count();
    let (_, _, cursor_y) = compute_input_layout(input, width, char_count);

    cursor_y + 3
}

/// Compute chat input lines and the cursor position for rendering.
///
/// The first line starts with the visible prompt prefix (` › `). Continuation
/// lines (from wrapping or explicit newlines) keep the same horizontal padding
/// as spaces, so text never appears under the prompt icon.
pub fn compute_input_layout(
    input: &str,
    width: u16,
    cursor: usize,
) -> (Vec<Line<'static>>, u16, u16) {
    let input_layout = compute_input_layout_data(input, width);
    let clamped_cursor = cursor.min(input_layout.cursor_positions.len().saturating_sub(1));
    let (cursor_x, cursor_y) = input_layout.cursor_positions[clamped_cursor];

    (
        input_layout.display_lines,
        u16::try_from(cursor_x).unwrap_or(u16::MAX),
        u16::try_from(cursor_y).unwrap_or(u16::MAX),
    )
}

/// Move the cursor one visual line up in the wrapped chat input layout.
pub fn move_input_cursor_up(input: &str, width: u16, cursor: usize) -> usize {
    move_input_cursor_vertical(input, width, cursor, VerticalDirection::Up)
}

/// Move the cursor one visual line down in the wrapped chat input layout.
pub fn move_input_cursor_down(input: &str, width: u16, cursor: usize) -> usize {
    move_input_cursor_vertical(input, width, cursor, VerticalDirection::Down)
}

/// Wrap plain text into terminal-width lines for output panes.
pub fn wrap_lines(text: &str, width: usize) -> Vec<Line<'_>> {
    let mut wrapped = Vec::new();
    for line in text.split('\n') {
        let mut current_line = String::new();
        let mut current_width = 0;

        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            wrapped.push(Line::from(""));
            continue;
        }

        for word in words {
            let word_len = word.chars().count();
            let space_len = usize::from(current_width != 0);

            if current_width + space_len + word_len > width && !current_line.is_empty() {
                wrapped.push(Line::from(current_line));
                current_line = String::new();
                current_width = 0;
            }

            if current_width > 0 {
                current_line.push(' ');
                current_width += 1;
            }
            current_line.push_str(word);
            current_width += word_len;
        }
        if !current_line.is_empty() {
            wrapped.push(Line::from(current_line));
        }
    }
    wrapped
}

/// Calculate the rendered width of the first table column.
///
/// This mirrors ratatui's table layout behavior, including highlight selection
/// space and column spacing.
pub fn first_table_column_width(
    table_width: u16,
    column_constraints: &[Constraint],
    column_spacing: u16,
    selection_width: u16,
) -> usize {
    if column_constraints.is_empty() {
        return 0;
    }

    let [_selection_area, columns_area] =
        Layout::horizontal([Constraint::Length(selection_width), Constraint::Fill(0)])
            .areas(Rect::new(0, 0, table_width, 1));
    let columns = Layout::horizontal(column_constraints.iter().copied())
        .spacing(column_spacing)
        .split(columns_area);

    columns
        .first()
        .map_or(0, |column| usize::from(column.width))
}

/// Truncate text to `max_width` and append `...` when it overflows.
pub fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    let text_width = text.chars().count();
    if text_width <= max_width {
        return text.to_string();
    }

    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let visible_width = max_width - 3;
    let truncated: String = text.chars().take(visible_width).collect();

    format!("{truncated}...")
}

fn move_input_cursor_vertical(
    input: &str,
    width: u16,
    cursor: usize,
    direction: VerticalDirection,
) -> usize {
    let input_layout = compute_input_layout_data(input, width);
    let clamped_cursor = cursor.min(input_layout.cursor_positions.len().saturating_sub(1));
    let (current_x, current_y) = input_layout.cursor_positions[clamped_cursor];

    let Some(target_y) = target_line_index(current_y, &input_layout.cursor_positions, direction)
    else {
        return clamped_cursor;
    };

    let target_line_width = input_layout
        .display_lines
        .get(target_y)
        .map_or(0, Line::width);
    let target_x = current_x.min(target_line_width);

    select_cursor_on_line(
        target_y,
        target_x,
        &input_layout.cursor_positions,
        clamped_cursor,
    )
}

fn compute_input_layout_data(input: &str, width: u16) -> InputLayout {
    let inner_width = width.saturating_sub(2) as usize;
    let prefix = " › ";
    let prefix_span = Span::styled(
        prefix,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let prefix_width = prefix_span.width();
    let continuation_padding = " ".repeat(prefix_width);

    let mut display_lines = Vec::new();
    let mut cursor_positions = Vec::with_capacity(input.chars().count() + 1);
    let mut current_line_spans = vec![prefix_span];
    let mut current_width = prefix_width;
    let mut line_index: usize = 0;

    for ch in input.chars() {
        if ch == '\n' {
            cursor_positions.push((current_width, line_index));
            display_lines.push(Line::from(std::mem::take(&mut current_line_spans)));
            current_line_spans = vec![Span::raw(continuation_padding.clone())];
            current_width = prefix_width;
            line_index += 1;

            continue;
        }

        let char_span = Span::raw(ch.to_string());
        let char_width = char_span.width();

        if current_width + char_width > inner_width {
            display_lines.push(Line::from(std::mem::take(&mut current_line_spans)));
            current_line_spans = vec![Span::raw(continuation_padding.clone())];
            current_width = prefix_width;
            line_index += 1;
        }

        cursor_positions.push((current_width, line_index));
        current_line_spans.push(char_span);
        current_width += char_width;
    }

    if current_width >= inner_width {
        cursor_positions.push((prefix_width, line_index + 1));
    } else {
        cursor_positions.push((current_width, line_index));
    }

    if !current_line_spans.is_empty() {
        display_lines.push(Line::from(current_line_spans));
    }

    if display_lines.is_empty() {
        display_lines.push(Line::from(""));
    }

    InputLayout {
        cursor_positions,
        display_lines,
    }
}

fn target_line_index(
    current_y: usize,
    cursor_positions: &[(usize, usize)],
    direction: VerticalDirection,
) -> Option<usize> {
    match direction {
        VerticalDirection::Up => current_y.checked_sub(1),
        VerticalDirection::Down => {
            let max_y = cursor_positions
                .iter()
                .map(|(_, cursor_y)| *cursor_y)
                .max()
                .unwrap_or(0);
            if current_y >= max_y {
                None
            } else {
                Some(current_y + 1)
            }
        }
    }
}

fn select_cursor_on_line(
    target_y: usize,
    target_x: usize,
    cursor_positions: &[(usize, usize)],
    fallback_cursor: usize,
) -> usize {
    let mut best_cursor_on_left: Option<(usize, usize)> = None;
    let mut nearest_cursor_on_right: Option<(usize, usize)> = None;

    for (cursor_index, (cursor_x, cursor_y)) in cursor_positions.iter().copied().enumerate() {
        if cursor_y != target_y {
            continue;
        }

        if cursor_x <= target_x {
            match best_cursor_on_left {
                Some((_, best_x)) if cursor_x < best_x => {}
                _ => {
                    best_cursor_on_left = Some((cursor_index, cursor_x));
                }
            }
        } else {
            match nearest_cursor_on_right {
                Some((_, nearest_x)) if cursor_x > nearest_x => {}
                _ => {
                    nearest_cursor_on_right = Some((cursor_index, cursor_x));
                }
            }
        }
    }

    best_cursor_on_left
        .or(nearest_cursor_on_right)
        .map_or(fallback_cursor, |(cursor_index, _)| cursor_index)
}

struct InputLayout {
    cursor_positions: Vec<(usize, usize)>,
    display_lines: Vec<Line<'static>>,
}

#[derive(Clone, Copy)]
enum VerticalDirection {
    Up,
    Down,
}

/// The kind of a line in a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    FileHeader,
    HunkHeader,
    Context,
    Addition,
    Deletion,
}

/// A parsed line from a unified diff, with optional old/new line numbers.
#[derive(Debug, PartialEq, Eq)]
pub struct DiffLine<'a> {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub content: &'a str,
}

/// Extract `(old_start, old_count, new_start, new_count)` from a hunk header
/// like `@@ -10,5 +20,7 @@`.
pub fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let line = line.strip_prefix("@@ -")?;
    let at_idx = line.find(" @@")?;
    let range_part = &line[..at_idx];
    let mut parts = range_part.split(" +");
    let old_range = parts.next()?;
    let new_range = parts.next()?;

    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;

    Some((old_start, old_count, new_start, new_count))
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    if let Some((start, count)) = range.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((range.parse().ok()?, 1))
    }
}

/// Parse a full unified diff into structured [`DiffLine`] entries with line
/// numbers.
pub fn parse_diff_lines(diff: &str) -> Vec<DiffLine<'_>> {
    let mut result = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for line in diff.lines() {
        if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            result.push(DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if line.starts_with("@@") {
            if let Some((old_start, _, new_start, _)) = parse_hunk_header(line) {
                old_line = old_start;
                new_line = new_start;
            }
            result.push(DiffLine {
                kind: DiffLineKind::HunkHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if let Some(rest) = line.strip_prefix('+') {
            result.push(DiffLine {
                kind: DiffLineKind::Addition,
                old_line: None,
                new_line: Some(new_line),
                content: rest,
            });
            new_line += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            result.push(DiffLine {
                kind: DiffLineKind::Deletion,
                old_line: Some(old_line),
                new_line: None,
                content: rest,
            });
            old_line += 1;
        } else {
            let content = line.strip_prefix(' ').unwrap_or(line);
            result.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_line),
                new_line: Some(new_line),
                content,
            });
            old_line += 1;
            new_line += 1;
        }
    }

    result
}

/// Find the maximum line number across all parsed diff lines for gutter width
/// calculation.
pub fn max_diff_line_number(lines: &[DiffLine<'_>]) -> u32 {
    lines
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .unwrap_or(0)
}

/// Split a diff content string into chunks that fit within `max_width`
/// characters. Returns at least one chunk (empty string if content is empty).
pub fn wrap_diff_content(content: &str, max_width: usize) -> Vec<&str> {
    if max_width == 0 {
        return vec![content];
    }

    let mut chunks = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if remaining.len() <= max_width {
            chunks.push(remaining);

            break;
        }

        let split_at = remaining
            .char_indices()
            .nth(max_width)
            .map_or(remaining.len(), |(idx, _)| idx);
        chunks.push(&remaining[..split_at]);
        remaining = &remaining[split_at..];
    }

    if chunks.is_empty() {
        chunks.push("");
    }

    chunks
}

/// Word-wraps a sequence of styled spans into multiple lines at the given
/// width.
///
/// Span styles are preserved across line breaks. A bold word that wraps to the
/// next line remains bold on that line.
pub fn wrap_styled_line(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(spans)];
    }

    let mut wrapped_lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;
    let mut needs_space = false;

    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();

        for word in content.split_whitespace() {
            let word_len = word.chars().count();
            let additional_space_width = usize::from(needs_space && current_width > 0);

            if current_width + additional_space_width + word_len > width
                && !current_spans.is_empty()
            {
                wrapped_lines.push(Line::from(std::mem::take(&mut current_spans)));
                current_width = 0;
                needs_space = false;
            }

            if needs_space && current_width > 0 {
                current_spans.push(Span::styled(" ".to_string(), style));
                current_width += 1;
            }

            current_spans.push(Span::styled(word.to_string(), style));
            current_width += word_len;
            needs_space = true;
        }
    }

    if !current_spans.is_empty() {
        wrapped_lines.push(Line::from(current_spans));
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Line::from(""));
    }

    wrapped_lines
}

/// Formats a token count for display: "500", "1.5k", "1.5M".
pub fn format_token_count(count: u64) -> String {
    if count >= 1_000_000 {
        return format_scaled_token_count(count, 1_000_000, "M");
    }
    if count >= 1_000 {
        return format_scaled_token_count(count, 1_000, "k");
    }

    count.to_string()
}

/// Formats elapsed seconds using a compact display suitable for dashboard
/// summaries.
pub fn format_duration_compact(duration_seconds: i64) -> String {
    if duration_seconds <= 0 {
        return "0m".to_string();
    }

    let duration_seconds = u64::try_from(duration_seconds).unwrap_or(0);
    let day_count = duration_seconds / 86_400;
    let hour_count = (duration_seconds % 86_400) / 3_600;
    let minute_count = (duration_seconds % 3_600) / 60;

    if day_count > 0 {
        return format!("{day_count}d {hour_count}h");
    }

    if hour_count > 0 {
        return format!("{hour_count}h {minute_count}m");
    }

    if minute_count > 0 {
        return format!("{minute_count}m");
    }

    "<1m".to_string()
}

/// Returns the current UTC day key as days since Unix epoch.
pub fn current_day_key_utc() -> i64 {
    let now_seconds = current_unix_timestamp_seconds();

    activity_day_key(now_seconds)
}

/// Returns the current local day key as days since Unix epoch.
pub fn current_day_key_local() -> i64 {
    let now_seconds = current_unix_timestamp_seconds();

    activity_day_key_local(now_seconds)
}

/// Converts Unix timestamp seconds to a UTC day key.
pub fn activity_day_key(timestamp_seconds: i64) -> i64 {
    timestamp_seconds.div_euclid(SECONDS_PER_DAY)
}

/// Converts Unix timestamp seconds to a local day key.
///
/// The local offset is resolved for the provided timestamp, so daylight-saving
/// transitions are applied automatically.
pub fn activity_day_key_local(timestamp_seconds: i64) -> i64 {
    let utc_offset_seconds = local_utc_offset_seconds(timestamp_seconds);

    activity_day_key_with_offset(timestamp_seconds, utc_offset_seconds)
}

/// Converts Unix timestamp seconds to a day key after applying a UTC offset.
pub fn activity_day_key_with_offset(timestamp_seconds: i64, utc_offset_seconds: i64) -> i64 {
    timestamp_seconds
        .saturating_add(utc_offset_seconds)
        .div_euclid(SECONDS_PER_DAY)
}

/// Builds a 53-week x 7-day heatmap grid from daily activity counts.
///
/// Rows are Monday through Sunday and columns are oldest to newest week.
pub fn build_activity_heatmap_grid(activity: &[DailyActivity], end_day_key: i64) -> Vec<Vec<u32>> {
    let mut grid = vec![vec![0_u32; HEATMAP_WEEK_COUNT]; HEATMAP_DAY_COUNT];
    let start_week_day_key = heatmap_start_week_day_key(end_day_key);
    let end_day_limit = start_week_day_key + (HEATMAP_WEEK_COUNT_I64 * HEATMAP_DAY_COUNT_I64) - 1;

    for daily_activity in activity {
        let day_key = daily_activity.day_key;
        if day_key < start_week_day_key || day_key > end_day_limit {
            continue;
        }

        let week_index =
            usize::try_from((day_key - start_week_day_key) / HEATMAP_DAY_COUNT_I64).unwrap_or(0);
        let weekday_index = weekday_index_monday(day_key);

        let day_cell = &mut grid[weekday_index][week_index];
        *day_cell = day_cell.saturating_add(daily_activity.session_count);
    }

    grid
}

/// Returns month label anchor points for each visible heatmap week.
///
/// Each tuple contains `(week_index, month_label)` where `week_index` is in
/// oldest-to-newest order across the 53-week heatmap.
pub fn heatmap_month_markers(end_day_key: i64) -> Vec<(usize, &'static str)> {
    let mut markers: Vec<(usize, &'static str)> = Vec::new();
    let start_week_day_key = heatmap_start_week_day_key(end_day_key);
    let mut previous_month_label: Option<&'static str> = None;

    for week_index in 0..HEATMAP_WEEK_COUNT {
        let week_start_day_key =
            start_week_day_key + (i64::try_from(week_index).unwrap_or(0) * HEATMAP_DAY_COUNT_I64);
        let month_label = month_label_from_day_key(week_start_day_key);
        if previous_month_label == Some(month_label) {
            continue;
        }

        markers.push((week_index, month_label));
        previous_month_label = Some(month_label);
    }

    markers
}

/// Builds the month-label row displayed above heatmap week columns.
///
/// `day_label_width` is the prefix width reserved for weekday labels and
/// `cell_width` is the width of each heatmap week column in characters.
pub fn build_heatmap_month_row(
    end_day_key: i64,
    day_label_width: usize,
    cell_width: usize,
) -> String {
    let total_width = day_label_width + (HEATMAP_WEEK_COUNT * cell_width);
    let mut row_characters = vec![' '; total_width];
    let mut last_label_end = 0_usize;

    for (week_index, month_label) in heatmap_month_markers(end_day_key) {
        let label_start = day_label_width + (week_index * cell_width);
        if label_start < last_label_end {
            continue;
        }

        for (label_offset, character) in month_label.chars().enumerate() {
            let write_index = label_start + label_offset;
            if write_index >= row_characters.len() {
                break;
            }

            row_characters[write_index] = character;
        }

        last_label_end = label_start + month_label.len();
    }

    row_characters.into_iter().collect()
}

/// Returns an activity intensity level from `0` to `4` for one heatmap cell.
pub fn heatmap_intensity_level(count: u32, max_count: u32) -> u8 {
    if count == 0 || max_count == 0 {
        return 0;
    }

    let scaled = (count.saturating_mul(4)).div_ceil(max_count);

    u8::try_from(scaled.min(4)).unwrap_or(4)
}

/// Returns the maximum daily activity count in a heatmap grid.
pub fn heatmap_max_count(grid: &[Vec<u32>]) -> u32 {
    grid.iter()
        .flat_map(|row| row.iter())
        .copied()
        .max()
        .unwrap_or(0)
}

fn format_scaled_token_count(count: u64, divisor: u64, suffix: &str) -> String {
    let scaled_tenths =
        ((u128::from(count) * 10) + (u128::from(divisor) / 2)) / u128::from(divisor);
    let whole = scaled_tenths / 10;
    let decimal = scaled_tenths % 10;

    format!("{whole}.{decimal}{suffix}")
}

fn current_unix_timestamp_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

fn local_utc_offset_seconds(timestamp_seconds: i64) -> i64 {
    let Ok(utc_timestamp) = OffsetDateTime::from_unix_timestamp(timestamp_seconds) else {
        return 0;
    };
    let Ok(local_offset) = UtcOffset::local_offset_at(utc_timestamp) else {
        return 0;
    };

    i64::from(local_offset.whole_seconds())
}

fn heatmap_start_week_day_key(end_day_key: i64) -> i64 {
    let end_week_start =
        end_day_key - i64::try_from(weekday_index_monday(end_day_key)).unwrap_or(0);

    end_week_start - ((HEATMAP_WEEK_COUNT_I64 - 1) * HEATMAP_DAY_COUNT_I64)
}

fn weekday_index_monday(day_key: i64) -> usize {
    let weekday_value = (day_key + 3).rem_euclid(HEATMAP_DAY_COUNT_I64);

    usize::try_from(weekday_value).unwrap_or(0)
}

fn month_label_from_day_key(day_key: i64) -> &'static str {
    let month_number = month_number_from_day_key(day_key);
    let month_index = usize::from(month_number.saturating_sub(1));

    HEATMAP_MONTH_LABELS
        .get(month_index)
        .copied()
        .unwrap_or("Jan")
}

fn month_number_from_day_key(day_key: i64) -> u8 {
    let (_year, month_number, _day) = civil_from_days(day_key);

    month_number
}

/// Converts a Unix day key (`1970-01-01` origin) into Gregorian
/// `(year, month, day)` values.
fn civil_from_days(day_key: i64) -> (i32, u8, u8) {
    let shifted_day_key = day_key + 719_468;
    let era = if shifted_day_key >= 0 {
        shifted_day_key
    } else {
        shifted_day_key - 146_096
    } / 146_097;
    let day_of_era = shifted_day_key - (era * 146_097);
    let year_of_era =
        (day_of_era - (day_of_era / 1_460) + (day_of_era / 36_524) - (day_of_era / 146_096)) / 365;
    let year = year_of_era + (era * 400);
    let day_of_year = day_of_era - (365 * year_of_era + (year_of_era / 4) - (year_of_era / 100));
    let month_partition = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_partition + 2) / 5 + 1;
    let month = month_partition + if month_partition < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);

    (
        i32::try_from(year).unwrap_or(1970),
        u8::try_from(month).unwrap_or(1),
        u8::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_input_height() {
        // Arrange & Act & Assert
        assert_eq!(calculate_input_height(20, ""), 3);
        assert_eq!(calculate_input_height(12, "1234567"), 4);
        assert_eq!(calculate_input_height(12, "12345678"), 4);
        assert_eq!(calculate_input_height(12, "12345671234567890"), 5);
    }

    #[test]
    fn test_compute_input_layout_empty() {
        // Arrange
        let input = "";
        let width = 20;

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_x, 3); // prefix " › "
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_single_line() {
        // Arrange
        let input = "test";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_x, 7); // 3 (prefix) + 4 (text)
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_exact_fit() {
        // Arrange
        let input = "1234567";
        let width = 12; // Inner width 10, Prefix 3, Available 7
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_wrap() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 4);
        assert_eq!(lines[1].to_string(), "   8");
        assert_eq!(cursor_x, 4);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_multiline_exact_fit() {
        // Arrange
        let input = "1234567".to_owned() + "1234567890";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(&input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 10);
        assert_eq!(lines[2].width(), 6);
        assert_eq!(cursor_x, 6);
        assert_eq!(cursor_y, 2);
    }

    #[test]
    fn test_compute_input_layout_cursor_at_start() {
        // Arrange
        let input = "hello";
        let width = 20;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

        // Assert — cursor sits right after prefix
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_cursor_in_middle() {
        // Arrange
        let input = "hello";
        let width = 20;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 2);

        // Assert — prefix(3) + 2 chars
        assert_eq!(cursor_x, 5);
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_cursor_before_wrapped_char() {
        // Arrange
        let input = "12345678";
        let width = 12;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 7);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_move_input_cursor_up_on_wrapped_layout() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let cursor = move_input_cursor_up(input, width, cursor);

        // Assert
        assert_eq!(cursor, 1);
    }

    #[test]
    fn test_move_input_cursor_down_on_wrapped_layout() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = 1;

        // Act
        let cursor = move_input_cursor_down(input, width, cursor);

        // Assert
        assert_eq!(cursor, input.chars().count());
    }

    #[test]
    fn test_compute_input_layout_explicit_newline() {
        // Arrange
        let input = "ab\ncd";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].to_string(), "   cd");
        assert_eq!(cursor_x, 5); // continuation padding + "cd"
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_multiple_newlines() {
        // Arrange
        let input = "a\n\nb";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert — 3 lines: "a", padded continuation line, "b"
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].to_string(), "   ");
        assert_eq!(cursor_x, 4);
        assert_eq!(cursor_y, 2);
    }

    #[test]
    fn test_compute_input_layout_cursor_on_second_line() {
        // Arrange — cursor right after the newline
        let input = "ab\ncd";
        let width = 20;

        // Act — cursor at char index 3 = 'a','b','\n' -> position 0 of second line
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 3);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_wrap_lines_basic() {
        // Arrange
        let text = "hello world";
        let width = 20;

        // Act
        let wrapped = wrap_lines(text, width);

        // Assert
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].to_string(), "hello world");
    }

    #[test]
    fn test_wrap_lines_wrapping() {
        // Arrange
        let text = "hello world";
        let width = 5;

        // Act
        let wrapped = wrap_lines(text, width);

        // Assert
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].to_string(), "hello");
        assert_eq!(wrapped[1].to_string(), "world");
    }

    #[test]
    fn test_wrap_styled_line_wraps_and_preserves_style() {
        // Arrange
        let style = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let spans = vec![Span::styled("hello world".to_string(), style)];

        // Act
        let wrapped = wrap_styled_line(spans, 5);

        // Assert
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].to_string(), "hello");
        assert_eq!(wrapped[1].to_string(), "world");
        assert_eq!(wrapped[0].spans[0].style, style);
        assert_eq!(wrapped[1].spans[0].style, style);
    }

    #[test]
    fn test_wrap_styled_line_zero_width_returns_original_line() {
        // Arrange
        let style = Style::default().fg(Color::Blue);
        let spans = vec![Span::styled("one two".to_string(), style)];

        // Act
        let wrapped = wrap_styled_line(spans, 0);

        // Assert
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].to_string(), "one two");
        assert_eq!(wrapped[0].spans[0].style, style);
    }

    #[test]
    fn test_wrap_styled_line_collapses_extra_whitespace() {
        // Arrange
        let spans = vec![
            Span::styled("hello   ".to_string(), Style::default().fg(Color::Green)),
            Span::styled("   world".to_string(), Style::default().fg(Color::Red)),
        ];

        // Act
        let wrapped = wrap_styled_line(spans, 20);

        // Assert
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].to_string(), "hello world");
    }

    #[test]
    fn test_first_table_column_width_uses_remaining_layout_space() {
        // Arrange
        let constraints = [
            Constraint::Fill(1),
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(4),
            Constraint::Length(6),
        ];

        // Act
        let width = first_table_column_width(50, &constraints, 1, 3);

        // Assert
        assert_eq!(width, 21);
    }

    #[test]
    fn test_truncate_with_ellipsis_keeps_full_text_when_it_fits() {
        // Arrange
        let text = "short title";

        // Act
        let truncated = truncate_with_ellipsis(text, 20);

        // Assert
        assert_eq!(truncated, "short title");
    }

    #[test]
    fn test_truncate_with_ellipsis_adds_three_dots_when_text_overflows() {
        // Arrange
        let text = "1234567890";

        // Act
        let truncated = truncate_with_ellipsis(text, 8);

        // Assert
        assert_eq!(truncated, "12345...");
    }

    #[test]
    fn test_truncate_with_ellipsis_uses_only_dots_for_tiny_widths() {
        // Arrange
        let text = "overflow";

        // Act
        let width_three = truncate_with_ellipsis(text, 3);
        let width_two = truncate_with_ellipsis(text, 2);
        let width_zero = truncate_with_ellipsis(text, 0);

        // Assert
        assert_eq!(width_three, "...");
        assert_eq!(width_two, "..");
        assert_eq!(width_zero, "");
    }

    #[test]
    fn test_format_token_count_small() {
        // Arrange & Act & Assert
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(500), "500");
        assert_eq!(format_token_count(999), "999");
    }

    #[test]
    fn test_format_token_count_thousands() {
        // Arrange & Act & Assert
        assert_eq!(format_token_count(1000), "1.0k");
        assert_eq!(format_token_count(1500), "1.5k");
        assert_eq!(format_token_count(12345), "12.3k");
        assert_eq!(format_token_count(999_999), "1000.0k");
    }

    #[test]
    fn test_format_token_count_millions() {
        // Arrange & Act & Assert
        assert_eq!(format_token_count(1_000_000), "1.0M");
        assert_eq!(format_token_count(1_500_000), "1.5M");
        assert_eq!(format_token_count(12_345_678), "12.3M");
    }

    #[test]
    fn test_format_token_count() {
        // Arrange & Act & Assert
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(500), "500");
        assert_eq!(format_token_count(1500), "1.5k");
        assert_eq!(format_token_count(1_500_000), "1.5M");
    }

    #[test]
    fn test_format_duration_compact() {
        // Arrange & Act
        let zero = format_duration_compact(0);
        let less_than_one_minute = format_duration_compact(59);
        let one_minute = format_duration_compact(60);
        let one_hour = format_duration_compact(3_600);
        let one_day = format_duration_compact(90_061);

        // Assert
        assert_eq!(zero, "0m");
        assert_eq!(less_than_one_minute, "<1m");
        assert_eq!(one_minute, "1m");
        assert_eq!(one_hour, "1h 0m");
        assert_eq!(one_day, "1d 1h");
    }

    #[test]
    fn test_activity_day_key_with_offset_applies_positive_offset() {
        // Arrange
        let timestamp_seconds = 86_399_i64;
        let utc_offset_seconds = 3_600_i64;

        // Act
        let day_key = activity_day_key_with_offset(timestamp_seconds, utc_offset_seconds);

        // Assert
        assert_eq!(day_key, 1);
    }

    #[test]
    fn test_activity_day_key_with_offset_applies_negative_offset() {
        // Arrange
        let timestamp_seconds = 86_400_i64;
        let utc_offset_seconds = -3_600_i64;

        // Act
        let day_key = activity_day_key_with_offset(timestamp_seconds, utc_offset_seconds);

        // Assert
        assert_eq!(day_key, 0);
    }

    #[test]
    fn test_build_activity_heatmap_grid_places_values_in_expected_cells() {
        // Arrange
        let end_day_key = 0_i64;
        let activity = vec![
            DailyActivity {
                day_key: 0,
                session_count: 2,
            },
            DailyActivity {
                day_key: -3,
                session_count: 1,
            },
        ];

        // Act
        let grid = build_activity_heatmap_grid(&activity, end_day_key);

        // Assert
        assert_eq!(grid[3][52], 2);
        assert_eq!(grid[0][52], 1);
    }

    #[test]
    fn test_heatmap_month_markers_start_on_month_changes() {
        // Arrange
        let end_day_key = 0_i64;

        // Act
        let markers = heatmap_month_markers(end_day_key);

        // Assert
        assert_eq!(markers.first(), Some(&(0, "Dec")));
        assert!(markers.iter().any(|marker| marker.1 == "Jan"));
    }

    #[test]
    fn test_build_heatmap_month_row_places_labels_on_week_columns() {
        // Arrange
        let end_day_key = 0_i64;

        // Act
        let month_row = build_heatmap_month_row(end_day_key, 4, 2);

        // Assert
        assert!(month_row.starts_with("    Dec"));
        assert_eq!(month_row.chars().count(), 110);
    }

    #[test]
    fn test_heatmap_intensity_level_scales_from_zero_to_max() {
        // Arrange
        let max_count = 8_u32;

        // Act
        let zero = heatmap_intensity_level(0, max_count);
        let low = heatmap_intensity_level(1, max_count);
        let medium = heatmap_intensity_level(4, max_count);
        let max = heatmap_intensity_level(8, max_count);

        // Assert
        assert_eq!(zero, 0);
        assert_eq!(low, 1);
        assert_eq!(medium, 2);
        assert_eq!(max, 4);
    }

    #[test]
    fn test_heatmap_max_count_returns_largest_daily_value() {
        // Arrange
        let grid = vec![vec![0, 2, 1], vec![3, 4, 0]];

        // Act
        let max_count = heatmap_max_count(&grid);

        // Assert
        assert_eq!(max_count, 4);
    }

    #[test]
    fn test_parse_hunk_header_basic() {
        // Arrange
        let line = "@@ -10,5 +20,7 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((10, 5, 20, 7)));
    }

    #[test]
    fn test_parse_hunk_header_no_count() {
        // Arrange
        let line = "@@ -1 +1 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((1, 1, 1, 1)));
    }

    #[test]
    fn test_parse_hunk_header_with_context() {
        // Arrange
        let line = "@@ -100,3 +200,4 @@ fn main() {";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((100, 3, 200, 4)));
    }

    #[test]
    fn test_parse_hunk_header_invalid() {
        // Arrange & Act & Assert
        assert_eq!(parse_hunk_header("not a hunk"), None);
        assert_eq!(parse_hunk_header("@@@ invalid @@@"), None);
    }

    #[test]
    fn test_parse_diff_lines_full() {
        // Arrange
        let diff = "\
diff --git a/file.rs b/file.rs
index abc..def 100644
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2
-removed";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 9);

        assert_eq!(lines[0].kind, DiffLineKind::FileHeader);
        assert_eq!(lines[0].content, "diff --git a/file.rs b/file.rs");
        assert_eq!(lines[0].old_line, None);

        assert_eq!(lines[4].kind, DiffLineKind::HunkHeader);
        assert_eq!(lines[4].old_line, None);

        assert_eq!(lines[5].kind, DiffLineKind::Context);
        assert_eq!(lines[5].content, "line1");
        assert_eq!(lines[5].old_line, Some(1));
        assert_eq!(lines[5].new_line, Some(1));

        assert_eq!(lines[6].kind, DiffLineKind::Addition);
        assert_eq!(lines[6].content, "added");
        assert_eq!(lines[6].old_line, None);
        assert_eq!(lines[6].new_line, Some(2));

        assert_eq!(lines[7].kind, DiffLineKind::Context);
        assert_eq!(lines[7].content, "line2");
        assert_eq!(lines[7].old_line, Some(2));
        assert_eq!(lines[7].new_line, Some(3));

        assert_eq!(lines[8].kind, DiffLineKind::Deletion);
        assert_eq!(lines[8].content, "removed");
        assert_eq!(lines[8].old_line, Some(3));
        assert_eq!(lines[8].new_line, None);
    }

    #[test]
    fn test_parse_diff_lines_empty() {
        // Arrange
        let diff = "";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn test_max_diff_line_number() {
        // Arrange
        let diff = "\
@@ -95,3 +100,4 @@
 context
+added
 context2
-removed";
        let lines = parse_diff_lines(diff);

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 102);
    }

    #[test]
    fn test_max_diff_line_number_empty() {
        // Arrange
        let lines: Vec<DiffLine<'_>> = Vec::new();

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 0);
    }

    #[test]
    fn test_wrap_diff_content_fits() {
        // Arrange
        let content = "short line";

        // Act
        let chunks = wrap_diff_content(content, 80);

        // Assert
        assert_eq!(chunks, vec!["short line"]);
    }

    #[test]
    fn test_wrap_diff_content_wraps() {
        // Arrange
        let content = "abcdefghij";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn test_wrap_diff_content_empty() {
        // Arrange & Act
        let chunks = wrap_diff_content("", 10);

        // Assert
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn test_wrap_diff_content_exact() {
        // Arrange
        let content = "abcd";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd"]);
    }
}
