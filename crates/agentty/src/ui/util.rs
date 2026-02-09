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

            if current_width + space_len + word_len > width {
                if !current_line.is_empty() {
                    wrapped.push(Line::from(current_line));
                    current_line = String::new();
                    current_width = 0;
                }
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
        .map(Line::width)
        .unwrap_or(0);
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
        .map(|(cursor_index, _)| cursor_index)
        .unwrap_or(fallback_cursor)
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
}
