//! Animated empty-state decoration for the session chat panel.
//!
//! Two robots carry boxes on their heads, walking toward each other and
//! then apart in a ping-pong cycle. A subtle hint appears below the
//! scene.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::domain::session::Status;
use crate::ui::style::palette;

/// Duration of each animation frame in milliseconds.
const FRAME_DURATION_MS: u128 = 300;

/// Total display width of the animation scene in columns.
const SCENE_WIDTH: usize = 28;

/// Robot face string rendered for each robot in the scene.
const ROBOT_FACE: &str = "[•_•]";

/// Box symbol displayed above each robot's head.
const BOX_SYMBOL: char = '■';

/// Open leg pose for alternating walk frames.
const LEGS_OPEN: &str = "/ \\";

/// Closed leg pose for alternating walk frames.
const LEGS_CLOSED: &str = "\\ /";

/// Hint text shown below the animation.
const HINT_TEXT: &str = "Send a message to start";

/// Number of scene content rows (box line + face line + legs line).
const SCENE_ROWS: usize = 3;

/// Total content height (scene rows + blank separator + hint line).
const CONTENT_HEIGHT: usize = SCENE_ROWS + 2;

/// Robot column positions `(left, right)` for each animation frame.
///
/// The robots walk toward each other and then back apart in a
/// ping-pong cycle.
const FRAME_POSITIONS: [(usize, usize); 8] = [
    (0, 23),
    (2, 21),
    (4, 19),
    (6, 17),
    (8, 15),
    (6, 17),
    (4, 19),
    (2, 21),
];

/// Returns `true` when the session output panel should display the idle
/// robot animation instead of normal content.
pub fn should_show(output: &str, status: Status) -> bool {
    output.is_empty() && status == Status::New
}

/// Generates vertically and horizontally centered animation lines for
/// the empty session chat panel.
///
/// Each frame shows two small robots carrying boxes on their heads,
/// walking toward each other and then apart. A subtle hint line
/// appears below.
pub fn animation_lines(panel_width: u16, panel_height: u16) -> Vec<Line<'static>> {
    let frame_index = current_frame_index();
    let (left_pos, right_pos) = FRAME_POSITIONS[frame_index % FRAME_POSITIONS.len()];
    let legs_open = frame_index.is_multiple_of(2);

    let box_line = build_box_line(left_pos, right_pos);
    let face_line = build_face_line(left_pos, right_pos);
    let legs_line = build_legs_line(left_pos, right_pos, legs_open);

    let width = usize::from(panel_width);
    let height = usize::from(panel_height);
    let top_padding = height.saturating_sub(CONTENT_HEIGHT) / 2;

    let mut lines = Vec::with_capacity(top_padding + CONTENT_HEIGHT);

    for _ in 0..top_padding {
        lines.push(Line::from(""));
    }

    lines.push(center_line(box_line, width));
    lines.push(center_line(face_line, width));
    lines.push(center_line(legs_line, width));
    lines.push(Line::from(""));
    lines.push(center_hint_line(width));

    lines
}

/// Returns the current animation frame index derived from wall-clock
/// time.
fn current_frame_index() -> usize {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    (now / FRAME_DURATION_MS) as usize
}

/// Builds the box row with `■` characters centered above each robot
/// head.
fn build_box_line(left_pos: usize, right_pos: usize) -> Line<'static> {
    let box_style = Style::default().fg(palette::WARNING);
    let left_box_col = left_pos + 2;
    let right_box_col = right_pos + 2;

    build_styled_scene_line(&[
        (left_box_col, BOX_SYMBOL.to_string(), box_style),
        (right_box_col, BOX_SYMBOL.to_string(), box_style),
    ])
}

/// Builds the face row with styled robot heads.
fn build_face_line(left_pos: usize, right_pos: usize) -> Line<'static> {
    let robot_style = Style::default().fg(palette::ACCENT);

    build_styled_scene_line(&[
        (left_pos, ROBOT_FACE.to_string(), robot_style),
        (right_pos, ROBOT_FACE.to_string(), robot_style),
    ])
}

/// Builds the legs row with alternating walk poses.
fn build_legs_line(left_pos: usize, right_pos: usize, legs_open: bool) -> Line<'static> {
    let legs_style = Style::default().fg(palette::TEXT_SUBTLE);
    let legs_str = if legs_open { LEGS_OPEN } else { LEGS_CLOSED };

    build_styled_scene_line(&[
        (left_pos + 1, legs_str.to_string(), legs_style),
        (right_pos + 1, legs_str.to_string(), legs_style),
    ])
}

/// Builds a fixed-width scene line by placing styled text fragments at
/// specified column positions, filling the rest with spaces.
fn build_styled_scene_line(placements: &[(usize, String, Style)]) -> Line<'static> {
    let mut cells: Vec<(char, Option<Style>)> = vec![(' ', None); SCENE_WIDTH];

    for (pos, text, style) in placements {
        for (offset, ch) in text.chars().enumerate() {
            let col = pos + offset;

            if col < SCENE_WIDTH {
                cells[col] = (ch, Some(*style));
            }
        }
    }

    let mut spans = Vec::new();
    let mut run_start = 0;

    while run_start < SCENE_WIDTH {
        let current_style = cells[run_start].1;
        let mut run_end = run_start + 1;

        while run_end < SCENE_WIDTH && cells[run_end].1 == current_style {
            run_end += 1;
        }

        let segment: String = cells[run_start..run_end].iter().map(|(ch, _)| ch).collect();
        let style = current_style.unwrap_or_default();
        spans.push(Span::styled(segment, style));

        run_start = run_end;
    }

    Line::from(spans)
}

/// Returns the hint line centered within the available width.
fn center_hint_line(available_width: usize) -> Line<'static> {
    center_line(
        Line::from(Span::styled(
            HINT_TEXT,
            Style::default().fg(palette::TEXT_SUBTLE),
        )),
        available_width,
    )
}

/// Horizontally centers a line within the given width by prepending
/// spaces.
fn center_line(line: Line<'static>, available_width: usize) -> Line<'static> {
    let line_width = line.width();

    if line_width >= available_width {
        return line;
    }

    let padding = (available_width - line_width) / 2;
    let mut spans = vec![Span::raw(" ".repeat(padding))];
    spans.extend(line.spans);

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_show_returns_true_for_empty_new_session() {
        // Arrange & Act & Assert
        assert!(should_show("", Status::New));
    }

    #[test]
    fn test_should_show_returns_false_for_non_empty_output() {
        // Arrange & Act & Assert
        assert!(!should_show("hello", Status::New));
    }

    #[test]
    fn test_should_show_returns_false_for_non_new_status() {
        // Arrange & Act & Assert
        assert!(!should_show("", Status::InProgress));
        assert!(!should_show("", Status::Review));
        assert!(!should_show("", Status::Done));
    }

    #[test]
    fn test_animation_lines_returns_non_empty() {
        // Arrange & Act
        let lines = animation_lines(80, 20);

        // Assert
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_animation_lines_contains_hint_text() {
        // Arrange & Act
        let lines = animation_lines(80, 20);
        let text: String = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains(HINT_TEXT));
    }

    #[test]
    fn test_animation_lines_contains_robot_faces() {
        // Arrange & Act
        let lines = animation_lines(80, 20);
        let text: String = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains(ROBOT_FACE));
    }

    #[test]
    fn test_animation_lines_contains_box_symbol() {
        // Arrange & Act
        let lines = animation_lines(80, 20);
        let text: String = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains(BOX_SYMBOL));
    }

    #[test]
    fn test_animation_lines_centers_vertically() {
        // Arrange
        let height: u16 = 20;

        // Act
        let lines = animation_lines(80, height);

        // Assert
        let expected_top_padding = (usize::from(height) - CONTENT_HEIGHT) / 2;
        let leading_empty = lines
            .iter()
            .take_while(|line| line.to_string().trim().is_empty())
            .count();
        assert_eq!(leading_empty, expected_top_padding);
    }

    #[test]
    fn test_animation_lines_centers_horizontally() {
        // Arrange
        let width: u16 = 80;
        let expected_pad = (usize::from(width) - SCENE_WIDTH) / 2;

        // Act
        let lines = animation_lines(width, 20);
        let face_line = lines
            .iter()
            .find(|line| line.to_string().contains(ROBOT_FACE))
            .expect("expected robot face line");

        // Assert - the first span is the centering pad from center_line.
        let first_span = &face_line.spans[0];
        assert_eq!(first_span.content.len(), expected_pad);
        assert!(first_span.content.chars().all(|ch| ch == ' '));
    }
    #[test]
    fn test_build_face_line_contains_two_robots() {
        // Arrange & Act
        let line = build_face_line(0, 23);
        let text = line.to_string();

        // Assert
        assert_eq!(text.matches(ROBOT_FACE).count(), 2);
    }

    #[test]
    fn test_build_box_line_contains_two_boxes() {
        // Arrange & Act
        let line = build_box_line(0, 23);
        let text = line.to_string();

        // Assert
        assert_eq!(text.matches(BOX_SYMBOL).count(), 2);
    }

    #[test]
    fn test_build_legs_line_alternates_poses() {
        // Arrange & Act
        let open_line = build_legs_line(0, 23, true);
        let closed_line = build_legs_line(0, 23, false);
        let open_text = open_line.to_string();
        let closed_text = closed_line.to_string();

        // Assert
        assert!(open_text.contains(LEGS_OPEN));
        assert!(closed_text.contains(LEGS_CLOSED));
    }

    #[test]
    fn test_frame_positions_keep_robots_non_overlapping() {
        // Arrange & Act & Assert
        for (left, right) in FRAME_POSITIONS {
            let robot_width = 5;
            let left_end = left + robot_width;

            assert!(
                left_end <= right,
                "robots overlap at left={left}, right={right}"
            );
        }
    }

    #[test]
    fn test_animation_lines_with_small_panel() {
        // Arrange & Act
        let lines = animation_lines(30, 5);

        // Assert
        assert!(!lines.is_empty());
        let text: String = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(ROBOT_FACE));
    }

    #[test]
    fn test_center_line_adds_left_padding() {
        // Arrange
        let line = Line::from("abc");

        // Act
        let centered = center_line(line, 10);
        let text = centered.to_string();

        // Assert — padding = (10 - 3) / 2 = 3
        assert!(text.starts_with("   "));
        assert!(text.contains("abc"));
    }

    #[test]
    fn test_center_line_returns_unchanged_when_too_wide() {
        // Arrange
        let line = Line::from("abcdefghij");

        // Act
        let centered = center_line(line, 5);

        // Assert
        assert_eq!(centered.to_string(), "abcdefghij");
    }

    #[test]
    fn test_build_styled_scene_line_clips_overflow() {
        // Arrange & Act
        let line =
            build_styled_scene_line(&[(SCENE_WIDTH - 1, "XY".to_string(), Style::default())]);
        let text = line.to_string();

        // Assert
        assert_eq!(text.chars().count(), SCENE_WIDTH);
        assert_eq!(text.chars().last(), Some('X'));
    }
}
