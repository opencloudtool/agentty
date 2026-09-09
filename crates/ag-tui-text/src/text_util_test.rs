use ratatui::style::{Color, Modifier, Style};

use super::*;

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
fn test_wrap_lines_uses_terminal_width_for_wide_characters() {
    // Arrange
    let text = "ab \u{754c}";
    let width = 4;

    // Act
    let wrapped = wrap_lines(text, width);

    // Assert
    assert_eq!(wrapped.len(), 2);
    assert_eq!(wrapped[0].to_string(), "ab");
    assert_eq!(wrapped[1].to_string(), "\u{754c}");
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
fn test_wrap_styled_line_preserves_adjacent_span_boundaries() {
    // Arrange
    let code_style = Style::default().fg(Color::Yellow);
    let spans = vec![
        Span::raw("Use (".to_string()),
        Span::styled("session_messages_from_rows".to_string(), code_style),
        Span::raw("), then [".to_string()),
        Span::styled("Image #1".to_string(), code_style),
        Span::raw("].".to_string()),
    ];

    // Act
    let wrapped = wrap_styled_line(spans, 80);

    // Assert
    assert_eq!(wrapped.len(), 1);
    assert_eq!(
        wrapped[0].to_string(),
        "Use (session_messages_from_rows), then [Image #1]."
    );
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
fn test_truncate_with_ellipsis_uses_terminal_width_for_wide_characters() {
    // Arrange
    let text = "\u{754c}\u{754c}abc";

    // Act
    let truncated = truncate_with_ellipsis(text, 5);

    // Assert
    assert_eq!(truncated, "\u{754c}...");
    assert_eq!(span_display_width(&truncated), 5);
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
fn test_inline_text_collapses_multiline_whitespace_runs() {
    // Arrange
    let text = "First draft\n\nSecond\t draft";

    // Act
    let inline = inline_text(text);

    // Assert
    assert_eq!(inline, "First draft Second draft");
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
    let two_minutes = format_duration_compact(120);
    let one_hour = format_duration_compact(3_600);
    let one_hour_one_minute_one_second = format_duration_compact(3_661);
    let one_day_one_hour_one_minute_one_second = format_duration_compact(90_061);

    // Assert
    assert_eq!(zero, "0s");
    assert_eq!(less_than_one_minute, "59s");
    assert_eq!(one_minute, "1m 0s");
    assert_eq!(two_minutes, "2m 0s");
    assert_eq!(one_hour, "1h 0s");
    assert_eq!(one_hour_one_minute_one_second, "1h 1m 1s");
    assert_eq!(one_day_one_hour_one_minute_one_second, "25h 1m 1s");
}

#[test]
fn test_truncate_spans_with_ellipsis_keeps_spans_when_they_fit() {
    // Arrange
    let spans = vec![
        Span::styled("hello".to_string(), Style::default().fg(Color::Green)),
        Span::styled(" world".to_string(), Style::default()),
    ];

    // Act
    let result = truncate_spans_with_ellipsis(spans.clone(), 20);

    // Assert
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].content.as_ref(), "hello");
    assert_eq!(result[1].content.as_ref(), " world");
}

#[test]
fn test_truncate_spans_with_ellipsis_truncates_within_span() {
    // Arrange
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let spans = vec![
        Span::styled("There are ".to_string(), Style::default()),
        Span::styled("4 items".to_string(), bold),
        Span::styled(" in Ready Now".to_string(), Style::default()),
    ];

    // Act — width 20 means 17 visible chars + "..."
    let result = truncate_spans_with_ellipsis(spans, 20);

    // Assert
    let text: String = result.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "There are 4 items...");
    assert_eq!(result[1].style, bold);
}

#[test]
fn test_truncate_spans_with_ellipsis_returns_dots_for_tiny_widths() {
    // Arrange
    let spans = vec![Span::raw("overflow".to_string())];

    // Act
    let width_three = truncate_spans_with_ellipsis(spans.clone(), 3);
    let width_two = truncate_spans_with_ellipsis(spans.clone(), 2);
    let width_zero = truncate_spans_with_ellipsis(spans, 0);

    // Assert
    assert_eq!(width_three[0].content.as_ref(), "...");
    assert_eq!(width_two[0].content.as_ref(), "..");
    assert_eq!(width_zero[0].content.as_ref(), "");
}

#[test]
fn test_truncate_spans_with_ellipsis_preserves_style_on_ellipsis() {
    // Arrange
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let spans = vec![Span::styled("long bold text".to_string(), bold)];

    // Act
    let result = truncate_spans_with_ellipsis(spans, 8);

    // Assert — "long ..." = 5 visible + "..."
    let text: String = result.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "long ...");
    assert_eq!(result[result.len() - 1].style, bold);
}

#[test]
fn test_truncate_spans_with_ellipsis_cuts_at_span_boundary() {
    // Arrange
    let green = Style::default().fg(Color::Green);
    let red = Style::default().fg(Color::Red);
    let spans = vec![
        Span::styled("abcd".to_string(), green),
        Span::styled("efgh".to_string(), red),
    ];

    // Act — width 7 means 4 visible + "...", cutting exactly at first span
    // boundary
    let result = truncate_spans_with_ellipsis(spans, 7);

    // Assert
    let text: String = result.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "abcd...");
    assert_eq!(result[0].style, green);
    assert_eq!(result[1].style, green);
}

#[test]
fn test_truncate_spans_with_ellipsis_accounts_for_cjk_double_width() {
    // Arrange — each CJK character occupies 2 terminal columns.
    // "表示テスト" = 10 columns total (5 chars × 2 cols each).
    let spans = vec![Span::raw("表示テスト".to_string())];

    // Act — max_width 9 means 6 visible columns + "...".
    // "表"(2) + "示"(2) + "テ"(2) = 6, then "..."
    let result = truncate_spans_with_ellipsis(spans, 9);

    // Assert
    let text: String = result.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "表示テ...");
}

#[test]
fn test_truncate_spans_with_ellipsis_does_not_split_wide_character() {
    // Arrange — "表示" = 4 columns total.
    let spans = vec![Span::raw("表示end".to_string())];

    // Act — max_width 6 means 3 visible columns + "...".
    // "表" takes 2 cols, "示" needs 2 more but only 1 remains → skip it.
    let result = truncate_spans_with_ellipsis(spans, 6);

    // Assert — only the first CJK char fits, leaving 1 unused column.
    let text: String = result.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "表...");
}

#[test]
fn test_truncate_spans_with_ellipsis_fits_wide_chars_without_truncation() {
    // Arrange — "表示" = 4 columns, fits within max_width 4.
    let spans = vec![Span::raw("表示".to_string())];

    // Act
    let result = truncate_spans_with_ellipsis(spans, 4);

    // Assert — no truncation needed.
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].content.as_ref(), "表示");
}

#[test]
fn test_truncate_spans_with_ellipsis_handles_emoji() {
    // Arrange — "🚀" is 2 columns wide.
    // "🚀🎉hello" = 2 + 2 + 5 = 9 columns total.
    let spans = vec![Span::raw("🚀🎉hello".to_string())];

    // Act — max_width 8 means 5 visible columns + "...".
    // "🚀"(2) + "🎉"(2) + "h"(1) = 5, then "..."
    let result = truncate_spans_with_ellipsis(spans, 8);

    // Assert
    let text: String = result.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "🚀🎉h...");
}

#[test]
fn test_truncate_spans_with_ellipsis_handles_combining_characters() {
    // Arrange — combining accent has 0 display width, so "e\u{0301}" = 1
    // column. "e\u{0301}abcdef" = 7 columns total.
    let spans = vec![Span::raw("e\u{0301}abcdef".to_string())];

    // Act — max_width 7 means all 7 columns fit.
    let result = truncate_spans_with_ellipsis(spans, 7);

    // Assert — no truncation needed.
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].content.as_ref(), "e\u{0301}abcdef");
}

#[test]
fn test_truncate_spans_with_ellipsis_cjk_across_styled_spans() {
    // Arrange — two styled spans with CJK content.
    // "表示" = 4 cols, "テスト" = 6 cols → total 10 cols.
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let spans = vec![
        Span::styled("表示".to_string(), bold),
        Span::raw("テスト".to_string()),
    ];

    // Act — max_width 9 means 6 visible columns + "...".
    // "表示" = 4 cols (full span), "テ" = 2 cols → 6, then "..."
    let result = truncate_spans_with_ellipsis(spans, 9);

    // Assert
    let text: String = result.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "表示テ...");
    assert_eq!(result[0].style, bold);
}
