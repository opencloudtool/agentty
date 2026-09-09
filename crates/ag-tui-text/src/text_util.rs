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
#[path = "text_util_test.rs"]
mod tests;
