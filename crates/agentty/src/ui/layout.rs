use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Maximum number of visible content lines inside the chat input viewport.
pub const CHAT_INPUT_MAX_VISIBLE_LINES: u16 = 10;

const CHAT_INPUT_BORDER_HEIGHT: u16 = 2;

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

/// Calculate the chat input widget height with a capped visible viewport.
///
/// The returned height includes top and bottom borders and limits the visible
/// content area to [`CHAT_INPUT_MAX_VISIBLE_LINES`].
pub fn calculate_input_height(width: u16, input: &str) -> u16 {
    let char_count = input.chars().count();
    let (_, _, cursor_y) = compute_input_layout(input, width, char_count);

    let content_line_count = cursor_y.saturating_add(1);

    content_line_count
        .min(CHAT_INPUT_MAX_VISIBLE_LINES)
        .saturating_add(CHAT_INPUT_BORDER_HEIGHT)
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

/// Calculate the input viewport scroll offset and cursor row inside it.
///
/// Returns `(scroll_offset, cursor_row)` where:
/// - `scroll_offset` is the number of content lines hidden above the viewport.
/// - `cursor_row` is the cursor's row relative to the viewport top.
pub fn calculate_input_viewport(
    total_line_count: usize,
    cursor_y: u16,
    viewport_height: u16,
) -> (u16, u16) {
    if viewport_height == 0 {
        return (0, 0);
    }

    let total_line_count = u16::try_from(total_line_count).unwrap_or(u16::MAX).max(1);
    let clamped_cursor_y = cursor_y.min(total_line_count.saturating_sub(1));
    let viewport_height = viewport_height.min(total_line_count);
    let max_scroll = total_line_count.saturating_sub(viewport_height);
    let scroll_offset = clamped_cursor_y
        .saturating_sub(viewport_height.saturating_sub(1))
        .min(max_scroll);
    let cursor_row = clamped_cursor_y.saturating_sub(scroll_offset);

    (scroll_offset, cursor_row)
}

/// Move the cursor one visual line up in the wrapped chat input layout.
pub fn move_input_cursor_up(input: &str, width: u16, cursor: usize) -> usize {
    move_input_cursor_vertical(input, width, cursor, VerticalDirection::Up)
}

/// Move the cursor one visual line down in the wrapped chat input layout.
pub fn move_input_cursor_down(input: &str, width: u16, cursor: usize) -> usize {
    move_input_cursor_vertical(input, width, cursor, VerticalDirection::Down)
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
        assert_eq!(calculate_input_height(12, &"a".repeat(120)), 12);
    }

    #[test]
    fn test_calculate_input_viewport_without_scroll() {
        // Arrange
        let total_line_count = 4;
        let cursor_y = 2;
        let viewport_height = 10;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 0);
        assert_eq!(cursor_row, 2);
    }

    #[test]
    fn test_calculate_input_viewport_with_scroll() {
        // Arrange
        let total_line_count = 20;
        let cursor_y = 15;
        let viewport_height = 10;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 6);
        assert_eq!(cursor_row, 9);
    }

    #[test]
    fn test_calculate_input_viewport_clamps_cursor_to_last_line() {
        // Arrange
        let total_line_count = 3;
        let cursor_y = 10;
        let viewport_height = 2;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 1);
        assert_eq!(cursor_row, 1);
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
}
