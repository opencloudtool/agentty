use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::util::wrap_styled_line;

const USER_PROMPT_PREFIX: &str = " › ";
const STATS_LABEL_WIDTH: usize = 22;

#[derive(Clone, Copy)]
enum BlockState {
    Paragraph,
    FencedCode,
    FencedStats,
}

/// Converts markdown text into styled, word-wrapped lines for terminal display.
pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut rendered_lines = Vec::new();
    let mut block_state = BlockState::Paragraph;
    let mut is_user_prompt_block = false;

    for raw_line in text.split('\n') {
        let starts_user_prompt_block = raw_line.starts_with(USER_PROMPT_PREFIX);
        if let Some(prompt_line) = user_prompt_block_line(raw_line, &mut is_user_prompt_block) {
            if starts_user_prompt_block {
                // Prompt lines are session metadata, not markdown content.
                block_state = BlockState::Paragraph;
                rendered_lines.push(user_prompt_padding_line(width));
            }

            let closes_user_prompt_block = prompt_line.is_empty() && !is_user_prompt_block;
            rendered_lines.extend(render_user_prompt_line(
                prompt_line,
                starts_user_prompt_block,
                width,
            ));
            if closes_user_prompt_block {
                rendered_lines.push(Line::from(""));
            }

            continue;
        }

        if is_fence_delimiter(raw_line) {
            block_state = match block_state {
                BlockState::Paragraph => opening_fence_block_state(raw_line),
                BlockState::FencedCode | BlockState::FencedStats => BlockState::Paragraph,
            };

            continue;
        }

        match block_state {
            BlockState::Paragraph => rendered_lines.extend(render_markdown_line(raw_line, width)),
            BlockState::FencedCode => rendered_lines.extend(render_code_line(raw_line, width)),
            BlockState::FencedStats => rendered_lines.extend(render_stats_line(raw_line, width)),
        }
    }

    if is_user_prompt_block {
        rendered_lines.push(user_prompt_padding_line(width));
    }

    if rendered_lines.is_empty() {
        rendered_lines.push(Line::from(""));
    }

    rendered_lines
}

/// Returns prompt block lines that must be rendered with prompt styling.
///
/// Prompt blocks start with `USER_PROMPT_PREFIX` and continue until the first
/// empty line.
fn user_prompt_block_line<'a>(
    raw_line: &'a str,
    is_user_prompt_block: &mut bool,
) -> Option<&'a str> {
    if *is_user_prompt_block && raw_line.is_empty() {
        *is_user_prompt_block = false;

        return Some(raw_line);
    }

    if raw_line.starts_with(USER_PROMPT_PREFIX) {
        *is_user_prompt_block = true;

        return Some(raw_line);
    }

    if *is_user_prompt_block {
        return Some(raw_line);
    }

    None
}

/// Renders a user prompt line verbatim so markdown syntax in prompts is not
/// parsed.
///
/// The first prompt line keeps the `USER_PROMPT_PREFIX` marker while all
/// continuation lines are padded to align with the prompt text.
fn render_user_prompt_line(
    raw_line: &str,
    starts_user_prompt_block: bool,
    width: usize,
) -> Vec<Line<'static>> {
    if raw_line.is_empty() {
        return vec![user_prompt_padding_line(width)];
    }

    let continuation_padding = user_prompt_continuation_padding();
    let content_style = user_prompt_content_style();

    let prompt_lines = if starts_user_prompt_block
        && let Some(content) = raw_line.strip_prefix(USER_PROMPT_PREFIX)
    {
        render_prefixed_verbatim_line(
            USER_PROMPT_PREFIX,
            &continuation_padding,
            content,
            user_prompt_prefix_style(),
            content_style,
            width,
        )
    } else {
        let continuation_content = raw_line
            .strip_prefix(continuation_padding.as_str())
            .unwrap_or(raw_line);

        render_prefixed_verbatim_line(
            &continuation_padding,
            &continuation_padding,
            continuation_content,
            content_style,
            content_style,
            width,
        )
    };

    prompt_lines
        .into_iter()
        .map(|line| pad_line_to_width(line, width, content_style))
        .collect()
}

/// Returns one full-width line used as top or bottom padding inside the user
/// prompt area.
fn user_prompt_padding_line(width: usize) -> Line<'static> {
    pad_line_to_width(Line::from(""), width, user_prompt_content_style())
}

/// Pads one rendered line to the target width using one style for trailing
/// cells.
fn pad_line_to_width(mut line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    if width == 0 {
        return line;
    }

    let line_width = line.width();
    if line_width >= width {
        return line;
    }

    line.spans
        .push(Span::styled(" ".repeat(width - line_width), style));

    line
}

fn render_markdown_line(raw_line: &str, width: usize) -> Vec<Line<'static>> {
    if raw_line.is_empty() {
        return vec![Line::from("")];
    }

    if raw_line.starts_with(USER_PROMPT_PREFIX) {
        return render_user_prompt_line(raw_line, true, width);
    }

    if let Some((level, content)) = parse_heading(raw_line) {
        return render_inline_line(content, heading_style(level), width);
    }

    if is_horizontal_rule(raw_line) {
        return vec![horizontal_rule_line(width)];
    }

    if let Some(content) = raw_line.strip_prefix("> ") {
        return render_prefixed_inline_line(
            "│ ",
            "│ ",
            content,
            blockquote_prefix_style(),
            Style::default().fg(Color::Gray),
            width,
        );
    }

    if let Some(content) = parse_bullet_content(raw_line) {
        return render_prefixed_inline_line(
            "- ",
            "  ",
            content,
            list_prefix_style(),
            Style::default(),
            width,
        );
    }

    if let Some((prefix, content)) = parse_numbered_content(raw_line) {
        let continuation_prefix = " ".repeat(prefix.chars().count());

        return render_prefixed_inline_line(
            &prefix,
            &continuation_prefix,
            content,
            list_prefix_style(),
            Style::default(),
            width,
        );
    }

    render_inline_line(raw_line, Style::default(), width)
}

fn render_prefixed_inline_line(
    prefix: &str,
    continuation_prefix: &str,
    content: &str,
    prefix_style: Style,
    content_style: Style,
    width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = prefix.chars().count();
    if width <= prefix_width {
        let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
        spans.extend(parse_inline_spans(content, content_style));

        return wrap_styled_line(spans, width);
    }

    let wrapped_content = render_inline_line(content, content_style, width - prefix_width);
    let mut lines = Vec::with_capacity(wrapped_content.len());

    for (index, line) in wrapped_content.into_iter().enumerate() {
        let marker = if index == 0 {
            prefix
        } else {
            continuation_prefix
        };
        let mut spans = vec![Span::styled(marker.to_string(), prefix_style)];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

/// Wraps one verbatim line while preserving a fixed prefix for wrapped
/// continuations.
fn render_prefixed_verbatim_line(
    prefix: &str,
    continuation_prefix: &str,
    content: &str,
    prefix_style: Style,
    content_style: Style,
    width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = prefix.chars().count();
    if width <= prefix_width {
        let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
        spans.extend(user_prompt_lookup_spans(content, content_style));

        return wrap_styled_line(spans, width);
    }

    let wrapped_content = wrap_verbatim_spans(
        user_prompt_lookup_spans(content, content_style),
        width - prefix_width,
    );
    let mut lines = Vec::with_capacity(wrapped_content.len());

    for (index, line) in wrapped_content.into_iter().enumerate() {
        let marker = if index == 0 {
            prefix
        } else {
            continuation_prefix
        };
        let marker_style = if index == 0 {
            prefix_style
        } else {
            content_style
        };
        let mut spans = vec![Span::styled(marker.to_string(), marker_style)];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

/// Splits one prompt content line into styled spans with `@` lookup token
/// highlighting.
fn user_prompt_lookup_spans(content: &str, content_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut is_lookup = false;
    let mut previous_character = None;

    for character in content.chars() {
        if character == '@' && previous_character.is_none_or(char::is_whitespace) {
            is_lookup = true;
        } else if character.is_whitespace() {
            is_lookup = false;
        }

        let style = if is_lookup {
            user_prompt_lookup_style()
        } else {
            content_style
        };
        push_verbatim_span_character(&mut spans, style, character);
        previous_character = Some(character);
    }

    spans
}

fn render_inline_line(content: &str, base_style: Style, width: usize) -> Vec<Line<'static>> {
    let inline_spans = parse_inline_spans(content, base_style);

    wrap_styled_line(inline_spans, width)
}

fn render_code_line(raw_line: &str, width: usize) -> Vec<Line<'static>> {
    wrap_verbatim_line(raw_line, code_block_style(), width)
}

fn render_stats_line(raw_line: &str, width: usize) -> Vec<Line<'static>> {
    if raw_line.is_empty() {
        return vec![Line::from("")];
    }

    if let Some((metric, value)) = parse_stats_metric_line(raw_line) {
        let metric_cell = format!("{metric:<STATS_LABEL_WIDTH$}");
        let spans = vec![
            Span::styled(metric_cell, stats_metric_style()),
            Span::styled(value.to_string(), stats_value_style()),
        ];

        return wrap_verbatim_spans(spans, width);
    }

    if raw_line == "Tokens Usage" {
        return wrap_verbatim_line(raw_line, stats_section_style(), width);
    }

    wrap_verbatim_line(raw_line, Style::default(), width)
}

fn wrap_verbatim_line(content: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(vec![Span::styled(content.to_string(), style)])];
    }

    if content.is_empty() {
        return vec![Line::from("")];
    }

    let mut wrapped_lines = Vec::new();
    let mut current_segment = String::new();
    let mut current_width = 0;

    for character in content.chars() {
        if current_width == width {
            wrapped_lines.push(Line::from(vec![Span::styled(
                std::mem::take(&mut current_segment),
                style,
            )]));
            current_width = 0;
        }

        current_segment.push(character);
        current_width += 1;
    }

    if !current_segment.is_empty() {
        wrapped_lines.push(Line::from(vec![Span::styled(current_segment, style)]));
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Line::from(""));
    }

    wrapped_lines
}

fn wrap_verbatim_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(spans)];
    }

    let mut wrapped_lines = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0;

    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();

        for character in content.chars() {
            if current_width == width {
                wrapped_lines.push(Line::from(std::mem::take(&mut current_spans)));
                current_width = 0;
            }

            push_verbatim_span_character(&mut current_spans, style, character);
            current_width += 1;
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

fn push_verbatim_span_character(spans: &mut Vec<Span<'static>>, style: Style, character: char) {
    if let Some(last_span) = spans.last_mut()
        && last_span.style == style
    {
        last_span.content.to_mut().push(character);

        return;
    }

    spans.push(Span::styled(character.to_string(), style));
}

fn parse_inline_spans(content: &str, base_style: Style) -> Vec<Span<'static>> {
    let characters: Vec<char> = content.chars().collect();
    let mut spans = Vec::new();
    let mut literal = String::new();
    let mut index = 0;

    while index < characters.len() {
        if characters[index] == '`'
            && let Some(end_index) = find_matching_backtick(&characters, index + 1)
            && end_index > index + 1
        {
            flush_literal_span(&mut spans, &mut literal, base_style);
            let inline_code: String = characters[index + 1..end_index].iter().collect();
            spans.push(Span::styled(inline_code, inline_code_style()));
            index = end_index + 1;

            continue;
        }

        if characters[index] == '*'
            && index + 1 < characters.len()
            && characters[index + 1] == '*'
            && let Some(end_index) = find_matching_double_asterisk(&characters, index + 2)
            && end_index > index + 2
        {
            flush_literal_span(&mut spans, &mut literal, base_style);
            let bold_content: String = characters[index + 2..end_index].iter().collect();
            spans.push(Span::styled(
                bold_content,
                base_style.add_modifier(Modifier::BOLD),
            ));
            index = end_index + 2;

            continue;
        }

        if characters[index] == '*'
            && let Some(end_index) = find_matching_single_asterisk(&characters, index + 1)
            && end_index > index + 1
        {
            flush_literal_span(&mut spans, &mut literal, base_style);
            let italic_content: String = characters[index + 1..end_index].iter().collect();
            spans.push(Span::styled(
                italic_content,
                base_style.add_modifier(Modifier::ITALIC),
            ));
            index = end_index + 1;

            continue;
        }

        literal.push(characters[index]);
        index += 1;
    }

    flush_literal_span(&mut spans, &mut literal, base_style);

    spans
}

fn flush_literal_span(spans: &mut Vec<Span<'static>>, literal: &mut String, style: Style) {
    if literal.is_empty() {
        return;
    }

    spans.push(Span::styled(std::mem::take(literal), style));
}

fn parse_heading(raw_line: &str) -> Option<(usize, &str)> {
    if let Some(content) = raw_line.strip_prefix("#### ") {
        return Some((4, content));
    }

    if let Some(content) = raw_line.strip_prefix("### ") {
        return Some((3, content));
    }

    if let Some(content) = raw_line.strip_prefix("## ") {
        return Some((2, content));
    }

    raw_line.strip_prefix("# ").map(|content| (1, content))
}

fn parse_bullet_content(raw_line: &str) -> Option<&str> {
    if let Some(content) = raw_line.strip_prefix("- ") {
        return Some(content);
    }

    raw_line.strip_prefix("* ")
}

fn parse_numbered_content(raw_line: &str) -> Option<(String, &str)> {
    let digit_count = raw_line.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }

    let (digits, suffix) = raw_line.split_at(digit_count);
    let content = suffix.strip_prefix(". ")?;

    Some((format!("{digits}. "), content))
}

fn opening_fence_block_state(raw_line: &str) -> BlockState {
    if is_stats_fence(raw_line) {
        return BlockState::FencedStats;
    }

    BlockState::FencedCode
}

fn is_fence_delimiter(raw_line: &str) -> bool {
    raw_line.trim().starts_with("```")
}

fn is_stats_fence(raw_line: &str) -> bool {
    raw_line.trim().starts_with("```stats")
}

fn parse_stats_metric_line(raw_line: &str) -> Option<(&str, &str)> {
    let (metric, value) = raw_line.split_once('\t')?;

    Some((metric, value))
}

fn is_horizontal_rule(raw_line: &str) -> bool {
    let trimmed = raw_line.trim();
    if trimmed.len() < 3 {
        return false;
    }

    trimmed.chars().all(|character| character == '-')
        || trimmed.chars().all(|character| character == '*')
}

fn horizontal_rule_line(width: usize) -> Line<'static> {
    if width == 0 {
        return Line::from("");
    }

    Line::from(vec![Span::styled(
        "-".repeat(width),
        horizontal_rule_style(),
    )])
}

fn heading_style(level: usize) -> Style {
    let color = match level {
        1 => Color::Cyan,
        2 => Color::Blue,
        3 => Color::Green,
        _ => Color::Yellow,
    };

    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn list_prefix_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn blockquote_prefix_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM)
}

fn horizontal_rule_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM)
}

fn code_block_style() -> Style {
    Style::default().fg(Color::Gray).bg(Color::Black)
}

fn stats_metric_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn stats_section_style() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

fn stats_value_style() -> Style {
    inline_code_style()
}

/// Returns the background color used for rendered user prompt blocks.
fn user_prompt_background_color() -> Color {
    Color::DarkGray
}

/// Returns the style for the visible `USER_PROMPT_PREFIX` marker.
fn user_prompt_prefix_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .bg(user_prompt_background_color())
        .add_modifier(Modifier::BOLD)
}

/// Returns the style for user prompt text content.
fn user_prompt_content_style() -> Style {
    Style::default().bg(user_prompt_background_color())
}

/// Returns the style for one `@` lookup token within user prompt content.
fn user_prompt_lookup_style() -> Style {
    Style::default()
        .fg(Color::LightBlue)
        .bg(user_prompt_background_color())
}

/// Returns continuation padding that aligns with `USER_PROMPT_PREFIX` width.
fn user_prompt_continuation_padding() -> String {
    " ".repeat(USER_PROMPT_PREFIX.chars().count())
}

fn inline_code_style() -> Style {
    Style::default().fg(Color::Yellow)
}

fn find_matching_backtick(characters: &[char], start_index: usize) -> Option<usize> {
    characters[start_index..]
        .iter()
        .position(|character| *character == '`')
        .map(|index| index + start_index)
}

fn find_matching_double_asterisk(characters: &[char], start_index: usize) -> Option<usize> {
    let mut index = start_index;

    while index + 1 < characters.len() {
        if characters[index] == '*' && characters[index + 1] == '*' {
            return Some(index);
        }

        index += 1;
    }

    None
}

fn find_matching_single_asterisk(characters: &[char], start_index: usize) -> Option<usize> {
    characters[start_index..]
        .iter()
        .position(|character| *character == '*')
        .map(|index| index + start_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_markdown_styles_heading() {
        // Arrange
        let input = "# Heading";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "Heading");
        assert_eq!(lines[0].spans[0].style, heading_style(1));
    }

    #[test]
    fn test_render_markdown_styles_user_prompt() {
        // Arrange
        let input = " › /model gemini";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].to_string().trim_end(), "");
        assert_eq!(lines[0].width(), 80);
        assert_eq!(lines[1].to_string().trim_end(), input);
        assert_eq!(lines[1].width(), 80);
        assert_eq!(lines[1].spans[0].style, user_prompt_prefix_style());
        assert_eq!(lines[1].spans[1].style, user_prompt_content_style());
        assert_eq!(
            lines[1].spans.last().expect("padding span").style,
            user_prompt_content_style()
        );
        assert_eq!(lines[2].to_string().trim_end(), "");
        assert_eq!(lines[2].width(), 80);
        assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
    }

    #[test]
    fn test_render_markdown_styles_multiline_user_prompt() {
        // Arrange
        let input = " › first line\nsecond line\n\nassistant line";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].to_string().trim_end(), "");
        assert_eq!(lines[1].to_string().trim_end(), " › first line");
        assert_eq!(lines[2].to_string().trim_end(), "   second line");
        assert_eq!(lines[1].width(), 80);
        assert_eq!(lines[2].width(), 80);
        assert_eq!(lines[4].to_string(), "");
        assert_eq!(lines[5].to_string(), "assistant line");
        assert_eq!(lines[1].spans[0].style, user_prompt_prefix_style());
        assert_eq!(lines[2].spans[0].content, "   ");
        assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
        assert_eq!(lines[2].spans[1].style, user_prompt_content_style());
        assert_eq!(lines[5].spans[0].style, Style::default());
    }

    #[test]
    fn test_render_markdown_keeps_prompt_continuation_line_verbatim() {
        // Arrange
        let input = " › first line\n**bold**\n\nassistant";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines[2].to_string().trim_end(), "   **bold**");
        assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
        assert_eq!(lines[4].to_string(), "");
        assert_eq!(lines[5].to_string(), "assistant");
    }

    #[test]
    fn test_render_markdown_wraps_user_prompt_content_with_continuation_padding() {
        // Arrange
        let input = " › one two three";

        // Act
        let lines = render_markdown(input, 8);

        // Assert
        assert!(lines.len() >= 4);
        assert_eq!(lines[0].to_string().trim_end(), "");
        assert!(lines[1].to_string().starts_with(" › "));
        assert!(lines[2].to_string().starts_with("   "));
        assert_eq!(lines[0].spans[0].style, user_prompt_content_style());
        assert_eq!(lines[2].spans[0].style, user_prompt_content_style());
        assert_eq!(
            lines.last().expect("bottom padding").spans[0].style,
            user_prompt_content_style()
        );
    }

    #[test]
    fn test_render_markdown_highlights_file_lookups_in_user_prompt_block() {
        // Arrange
        let input = " › check @crates/agentty/src/ui/markdown.rs";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert!(lines[1].spans.iter().any(|span| {
            span.content.as_ref() == "@crates/agentty/src/ui/markdown.rs"
                && span.style == user_prompt_lookup_style()
        }));
    }

    #[test]
    fn test_render_markdown_does_not_highlight_non_lookup_at_symbol_in_user_prompt_block() {
        // Arrange
        let input = " › reach me at email@example.com";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert!(
            !lines[1]
                .spans
                .iter()
                .any(|span| span.style == user_prompt_lookup_style())
        );
    }

    #[test]
    fn test_render_markdown_keeps_text_after_multiple_blank_lines_in_user_prompt_block() {
        // Arrange
        let input = " › first line\n   \n   \n   after gap\n\nassistant";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert!(lines.iter().any(|line| {
            line.to_string().trim_end() == "   after gap"
                && line
                    .spans
                    .iter()
                    .all(|span| span.style == user_prompt_content_style())
        }));
        assert_eq!(
            lines.last().expect("assistant line").to_string(),
            "assistant"
        );
    }

    #[test]
    fn test_render_markdown_parses_inline_styles() {
        // Arrange
        let input = "before **bold** *italic* `code`";

        // Act
        let lines = render_markdown(input, 80);
        let line = &lines[0];

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(line.to_string(), "before bold italic code");
        assert!(line.spans.iter().any(|span| {
            span.content.as_ref() == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(line.spans.iter().any(|span| {
            span.content.as_ref() == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
        assert!(
            line.spans
                .iter()
                .any(|span| span.content.as_ref() == "code" && span.style == inline_code_style())
        );
    }

    #[test]
    fn test_render_markdown_leaves_unmatched_inline_delimiters_literal() {
        // Arrange
        let input = "text **bold";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), input);
        assert!(
            !lines[0]
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn test_render_markdown_renders_fenced_code_without_inline_parsing() {
        // Arrange
        let input = "```rust\nlet value = **raw**;\n```";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "let value = **raw**;");
        assert_eq!(lines[0].spans[0].style, code_block_style());
    }

    #[test]
    fn test_render_markdown_treats_unclosed_fence_as_code() {
        // Arrange
        let input = "```\n**raw**";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "**raw**");
        assert_eq!(lines[0].spans[0].style, code_block_style());
    }

    #[test]
    fn test_render_markdown_renders_stats_metric_with_fixed_alignment() {
        // Arrange
        let input = "```stats\nSession ID\tsession-id\n```";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].to_string().find("session-id"),
            Some(STATS_LABEL_WIDTH)
        );
        assert!(lines[0].spans.iter().any(|span| {
            span.content.as_ref().contains("Session ID") && span.style == stats_metric_style()
        }));
        assert!(lines[0].spans.iter().any(|span| {
            span.content.as_ref().contains("session-id") && span.style == stats_value_style()
        }));
    }

    #[test]
    fn test_render_markdown_renders_stats_section_title_style() {
        // Arrange
        let input = "```stats\nTokens Usage\n```";

        // Act
        let lines = render_markdown(input, 80);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "Tokens Usage");
        assert_eq!(lines[0].spans[0].style, stats_section_style());
    }

    #[test]
    fn test_render_markdown_wraps_bullets_with_continuation_indent() {
        // Arrange
        let input = "- one two three four";

        // Act
        let lines = render_markdown(input, 8);

        // Assert
        assert!(lines.len() >= 2);
        assert!(lines[0].to_string().starts_with("- "));
        assert!(lines[1].to_string().starts_with("  "));
    }

    #[test]
    fn test_render_markdown_wraps_numbered_list_with_continuation_indent() {
        // Arrange
        let input = "12. one two three";

        // Act
        let lines = render_markdown(input, 9);

        // Assert
        assert!(lines.len() >= 2);
        assert!(lines[0].to_string().starts_with("12. "));
        assert!(lines[1].to_string().starts_with("    "));
    }

    #[test]
    fn test_render_markdown_wraps_blockquote_with_prefix() {
        // Arrange
        let input = "> one two three";

        // Act
        let lines = render_markdown(input, 7);

        // Assert
        assert!(lines.len() >= 2);
        assert!(lines[0].to_string().starts_with("│ "));
        assert!(lines[1].to_string().starts_with("│ "));
    }

    #[test]
    fn test_render_markdown_renders_horizontal_rule() {
        // Arrange
        let input = "---";

        // Act
        let lines = render_markdown(input, 5);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "-----");
        assert_eq!(lines[0].spans[0].style, horizontal_rule_style());
    }
}
