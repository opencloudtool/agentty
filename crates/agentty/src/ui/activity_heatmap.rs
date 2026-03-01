use std::time::{SystemTime, UNIX_EPOCH};

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

/// Returns how many trailing heatmap week columns fit in `available_width`.
///
/// The returned value is clamped between `1` and `53` so the heatmap always
/// renders at least one data column.
pub fn visible_heatmap_week_count(
    available_width: usize,
    day_label_width: usize,
    cell_width: usize,
) -> usize {
    let safe_cell_width = cell_width.max(1);
    let available_week_width = available_width.saturating_sub(day_label_width);
    let visible_weeks = available_week_width / safe_cell_width;

    visible_weeks.clamp(1, HEATMAP_WEEK_COUNT)
}

/// Builds the month-label row for the trailing `visible_week_count` weeks.
///
/// This is used by narrow layouts that cannot display the full 53-week grid.
pub fn build_visible_heatmap_month_row(
    end_day_key: i64,
    day_label_width: usize,
    cell_width: usize,
    visible_week_count: usize,
) -> String {
    let normalized_visible_week_count = visible_week_count.clamp(1, HEATMAP_WEEK_COUNT);
    let total_width = day_label_width + (normalized_visible_week_count * cell_width);
    let first_visible_week_index = HEATMAP_WEEK_COUNT.saturating_sub(normalized_visible_week_count);
    let mut row_characters = vec![' '; total_width];
    let mut last_label_end = 0_usize;

    for (week_index, month_label) in heatmap_month_markers(end_day_key) {
        if week_index < first_visible_week_index {
            continue;
        }

        let relative_week_index = week_index.saturating_sub(first_visible_week_index);
        let label_start = day_label_width + (relative_week_index * cell_width);
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
    fn test_visible_heatmap_week_count_clamps_to_available_width() {
        // Arrange
        let available_width = 26_usize;

        // Act
        let visible_week_count = visible_heatmap_week_count(available_width, 4, 2);

        // Assert
        assert_eq!(visible_week_count, 11);
    }

    #[test]
    fn test_build_visible_heatmap_month_row_uses_trailing_weeks() {
        // Arrange
        let end_day_key = 0_i64;

        // Act
        let month_row = build_visible_heatmap_month_row(end_day_key, 4, 2, 11);

        // Assert
        assert_eq!(month_row.chars().count(), 26);
        assert!(month_row.contains("Dec"));
        assert!(!month_row.trim().is_empty());
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
}
