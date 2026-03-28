use ratatui::text::{Line, Span};

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

/// Splits one trailing footer line block from `text` when its last non-empty
/// line starts with any provided prefix.
///
/// This keeps transcript content and synthetic footers renderable in separate
/// sections without rewriting the stored output string.
pub fn split_trailing_line_block<'a>(
    text: &'a str,
    line_prefixes: &[&str],
) -> (&'a str, Option<&'a str>) {
    let trimmed_text = text.trim_end_matches('\n');
    if trimmed_text.is_empty() {
        return (text, None);
    }

    let line_start = trimmed_text.rfind('\n').map_or(0, |index| index + 1);
    let trailing_line = &trimmed_text[line_start..];
    if !line_prefixes
        .iter()
        .any(|line_prefix| trailing_line.starts_with(line_prefix))
    {
        return (text, None);
    }

    (&text[..line_start], Some(&text[line_start..]))
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
    fn test_split_trailing_line_block_returns_matching_footer() {
        // Arrange
        let text = "Implemented the change.\n\n[Commit] committed with hash `abc1234`\n";

        // Act
        let (body, footer) = split_trailing_line_block(text, &["[Commit]", "[Commit Error]"]);

        // Assert
        assert_eq!(body, "Implemented the change.\n\n");
        assert_eq!(footer, Some("[Commit] committed with hash `abc1234`\n"));
    }

    #[test]
    fn test_split_trailing_line_block_ignores_nonmatching_last_line() {
        // Arrange
        let text = "[Commit] committed with hash `abc1234`\n\n › Follow up\n";

        // Act
        let (body, footer) = split_trailing_line_block(text, &["[Commit]", "[Commit Error]"]);

        // Assert
        assert_eq!(body, text);
        assert_eq!(footer, None);
    }
}
