use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

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

/// Formats a token count for display: "500", "1.5k", "1.5M".
pub fn format_token_count(count: i64) -> String {
    if count >= 1_000_000 {
        return format_scaled_token_count(count, 1_000_000, "M");
    }
    if count >= 1_000 {
        return format_scaled_token_count(count, 1_000, "k");
    }

    count.to_string()
}

/// Formats an optional token count, returning "-" when absent.
pub fn format_optional_tokens(value: Option<i64>) -> String {
    match value {
        Some(count) => format_token_count(count),
        None => "-".to_string(),
    }
}

fn format_scaled_token_count(count: i64, divisor: i64, suffix: &str) -> String {
    let scaled_tenths =
        ((i128::from(count) * 10) + (i128::from(divisor) / 2)) / i128::from(divisor);
    let whole = scaled_tenths / 10;
    let decimal = scaled_tenths % 10;

    format!("{whole}.{decimal}{suffix}")
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
    fn test_first_table_column_width_uses_remaining_layout_space() {
        // Arrange
        let constraints = [
            Constraint::Min(0),
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
    fn test_format_optional_tokens() {
        // Arrange & Act & Assert
        assert_eq!(format_optional_tokens(Some(1500)), "1.5k");
        assert_eq!(format_optional_tokens(None), "-");
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
