use std::borrow::Cow;
use std::fmt::Write as _;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

struct StyledWord {
    leading_space_style: Option<Style>,
    spans: Vec<Span<'static>>,
    width: usize,
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
            let word_len = span_display_width(word);
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

/// Truncate text to `max_width` and append `...` when it overflows.
pub fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    let text_width = span_display_width(text);
    if text_width <= max_width {
        return text.to_string();
    }

    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let visible_width = max_width - 3;
    let truncated = take_columns(text, visible_width);

    format!("{truncated}...")
}

/// Truncates a sequence of styled spans to `max_width` terminal columns,
/// appending an ellipsis (`...`) when the text overflows.
///
/// Width is measured using Unicode display widths so CJK characters (2
/// columns), emoji, and combining characters (0 columns) are accounted for
/// correctly. Span styles are preserved: if truncation falls inside a span,
/// only the visible prefix of that span is kept with its original style. The
/// trailing `...` inherits the style of the last emitted span.
pub fn truncate_spans_with_ellipsis(
    spans: Vec<Span<'static>>,
    max_width: usize,
) -> Vec<Span<'static>> {
    if max_width == 0 {
        return vec![Span::raw(String::new())];
    }

    let total_width: usize = spans
        .iter()
        .map(|span| span_display_width(&span.content))
        .sum();

    if total_width <= max_width {
        return spans;
    }

    if max_width <= 3 {
        return vec![Span::raw(".".repeat(max_width))];
    }

    let visible_width = max_width - 3;
    let mut remaining = visible_width;
    let mut result: Vec<Span<'static>> = Vec::new();
    let mut last_style = Style::default();

    for span in spans {
        if remaining == 0 {
            break;
        }

        last_style = span.style;
        let width = span_display_width(&span.content);

        if width <= remaining {
            remaining -= width;
            result.push(span);
        } else {
            let truncated = take_columns(&span.content, remaining);
            result.push(Span::styled(truncated, span.style));
            remaining = 0;
        }
    }

    result.push(Span::styled("...".to_string(), last_style));

    result
}

/// Returns the terminal display width of a string slice.
fn span_display_width(text: &str) -> usize {
    text.chars().map(character_display_width).sum()
}

fn character_display_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// Takes the leading prefix of `text` that fits within `columns` terminal
/// columns without splitting a wide character.
fn take_columns(text: &str, columns: usize) -> String {
    let mut remaining = columns;
    let mut result = String::new();

    for character in text.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width > remaining {
            break;
        }

        remaining -= width;
        result.push(character);
    }

    result
}

/// Converts arbitrary text into a single inline label by collapsing all
/// whitespace runs, including embedded newlines, into single spaces.
pub fn inline_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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

    let words = styled_words(spans);
    if words.is_empty() {
        return vec![Line::from("")];
    }

    let mut wrapped_lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    for word in words {
        let space_width = usize::from(word.leading_space_style.is_some() && current_width > 0);

        if current_width + space_width + word.width > width && !current_spans.is_empty() {
            wrapped_lines.push(Line::from(std::mem::take(&mut current_spans)));
            current_width = 0;
        }

        if current_width > 0
            && let Some(space_style) = word.leading_space_style
        {
            current_spans.push(Span::styled(" ".to_string(), space_style));
            current_width += 1;
        }

        current_width += word.width;
        current_spans.extend(word.spans);
    }

    if !current_spans.is_empty() {
        wrapped_lines.push(Line::from(current_spans));
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Line::from(""));
    }

    wrapped_lines
}

fn styled_words(spans: Vec<Span<'static>>) -> Vec<StyledWord> {
    let mut words = Vec::new();
    let mut word_spans = Vec::new();
    let mut word_width = 0;
    let mut leading_space_style = None;
    let mut next_space_style = None;
    let mut has_previous_word = false;

    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();

        for character in content.chars() {
            if character.is_whitespace() {
                flush_styled_word(
                    &mut words,
                    &mut word_spans,
                    &mut word_width,
                    &mut leading_space_style,
                    &mut has_previous_word,
                );
                if has_previous_word {
                    next_space_style = Some(style);
                }

                continue;
            }

            if word_spans.is_empty() {
                leading_space_style = next_space_style.take();
            }

            push_styled_character(&mut word_spans, style, character);
            word_width += character_display_width(character);
        }
    }

    flush_styled_word(
        &mut words,
        &mut word_spans,
        &mut word_width,
        &mut leading_space_style,
        &mut has_previous_word,
    );

    words
}

fn flush_styled_word(
    words: &mut Vec<StyledWord>,
    word_spans: &mut Vec<Span<'static>>,
    word_width: &mut usize,
    leading_space_style: &mut Option<Style>,
    has_previous_word: &mut bool,
) {
    if word_spans.is_empty() {
        return;
    }

    words.push(StyledWord {
        leading_space_style: leading_space_style.take(),
        spans: std::mem::take(word_spans),
        width: *word_width,
    });
    *word_width = 0;
    *has_previous_word = true;
}

fn push_styled_character(spans: &mut Vec<Span<'static>>, style: Style, character: char) {
    if let Some(last_span) = spans.last_mut()
        && last_span.style == style
    {
        last_span.content.to_mut().push(character);

        return;
    }

    spans.push(Span::styled(character.to_string(), style));
}

/// Builds short-lived paint lines that borrow span text from cached owned
/// `Line<'static>` entries.
pub fn borrowed_paint_lines<'line>(lines: &'line [Line<'static>]) -> Vec<Line<'line>> {
    lines.iter().map(borrowed_paint_line).collect()
}

/// Builds one short-lived paint line that borrows span text from a cached
/// owned line.
pub fn borrowed_paint_line<'line>(line: &'line Line<'static>) -> Line<'line> {
    Line {
        alignment: line.alignment,
        spans: line.spans.iter().map(borrowed_paint_span).collect(),
        style: line.style,
    }
}

/// Builds one borrowed paint span from a cached static span.
fn borrowed_paint_span<'span>(span: &'span Span<'static>) -> Span<'span> {
    Span {
        content: Cow::Borrowed(span.content.as_ref()),
        style: span.style,
    }
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

/// Formats elapsed seconds as a compact `1h 1m 1s` label.
///
/// Hours and minutes are omitted when their value is zero, but seconds are
/// always rendered so live timers never lose sub-minute visibility.
pub fn format_duration_compact(duration_seconds: i64) -> String {
    if duration_seconds <= 0 {
        return "0s".to_string();
    }

    let duration_seconds = u64::try_from(duration_seconds).unwrap_or(0);
    let hour_count = duration_seconds / 3_600;
    let minute_count = (duration_seconds % 3_600) / 60;
    let second_count = duration_seconds % 60;
    let mut label = String::new();

    if hour_count > 0 {
        let _ = write!(label, "{hour_count}h ");
    }

    if minute_count > 0 {
        let _ = write!(label, "{minute_count}m ");
    }

    let _ = write!(label, "{second_count}s");

    label
}

fn format_scaled_token_count(count: u64, divisor: u64, suffix: &str) -> String {
    let scaled_tenths =
        ((u128::from(count) * 10) + (u128::from(divisor) / 2)) / u128::from(divisor);
    let whole = scaled_tenths / 10;
    let decimal = scaled_tenths % 10;

    format!("{whole}.{decimal}{suffix}")
}

#[cfg(test)]
mod tests {
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
}
