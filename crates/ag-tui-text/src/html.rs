use ratatui::text::Line;

use crate::{TextRenderSettings, markdown};

/// Maximum forge-authored HTML input normalized for one render.
const MAX_HTML_INPUT_BYTE_COUNT: usize = 1024 * 1024;
/// Notice appended after forge-authored HTML input is truncated.
const HTML_INPUT_TRUNCATION_NOTICE: &str = "\n\n[Forge content truncated at 1 MiB.]";
/// Maximum HTML tag size inspected while normalizing untrusted forge text.
const MAX_HTML_TAG_BYTE_COUNT: usize = 4_096;
/// Maximum HTML entity size inspected while normalizing untrusted forge text.
const MAX_HTML_ENTITY_BYTE_COUNT: usize = 10;

/// Converts HTML embedded in forge-authored Markdown into styled,
/// word-wrapped terminal lines.
pub fn render_html(text: &str, width: usize) -> Vec<Line<'static>> {
    render_html_with_settings(text, width, TextRenderSettings::default())
}

/// Converts HTML embedded in forge-authored Markdown into styled,
/// word-wrapped terminal lines using caller-provided theme settings. Inputs
/// larger than 1 MiB are truncated before normalization.
pub fn render_html_with_settings(
    text: &str,
    width: usize,
    settings: TextRenderSettings,
) -> Vec<Line<'static>> {
    let normalized = html_to_markdown(text);

    markdown::render_markdown_with_settings(&normalized, width, settings)
}

/// Converts common HTML snippets embedded in forge-authored Markdown to
/// Markdown-like text before terminal rendering.
fn html_to_markdown(html: &str) -> String {
    let (html, was_truncated) = bounded_html_input(html);
    let mut rendered = String::new();
    let mut index = 0;
    let mut is_fenced_code = false;
    let mut is_inline_code = false;

    while index < html.len() {
        let character = html[index..].chars().next().unwrap_or_default();
        if normalize_markdown_code(
            html,
            character,
            &mut rendered,
            &mut index,
            &mut is_fenced_code,
            &mut is_inline_code,
        ) {
            continue;
        }

        if let Some(end_index) = html_comment_end_index(html, index) {
            index = end_index;

            continue;
        }

        if let Some(tag) = parse_html_tag(html, index) {
            append_html_tag_replacement(&mut rendered, &tag);
            index = tag.end_index;

            continue;
        }

        if let Some((decoded, consumed)) = decode_html_entity(&html[index..]) {
            rendered.push(decoded);
            index += consumed;

            continue;
        }

        rendered.push(character);
        index += character.len_utf8();
    }

    let mut normalized = compact_blank_lines(&rendered);
    if was_truncated {
        normalized.push_str(HTML_INPUT_TRUNCATION_NOTICE);
    }

    normalized
}

/// Bounds untrusted forge HTML at a valid UTF-8 boundary before parsing.
fn bounded_html_input(html: &str) -> (&str, bool) {
    if html.len() <= MAX_HTML_INPUT_BYTE_COUNT {
        return (html, false);
    }

    let mut end_index = MAX_HTML_INPUT_BYTE_COUNT;
    while !html.is_char_boundary(end_index) {
        end_index -= 1;
    }

    (&html[..end_index], true)
}

/// Preserves a Markdown code token that must bypass HTML normalization,
/// returning whether the current input position was consumed.
fn normalize_markdown_code(
    markdown: &str,
    character: char,
    rendered: &mut String,
    index: &mut usize,
    is_fenced_code: &mut bool,
    is_inline_code: &mut bool,
) -> bool {
    if let Some((line_end_index, is_fence_delimiter)) =
        preserved_code_line(markdown, *index, *is_fenced_code)
    {
        rendered.push_str(&markdown[*index..line_end_index]);
        *index = line_end_index;
        if is_fence_delimiter {
            *is_fenced_code = !*is_fenced_code;
        }

        return true;
    }

    if character == '`' {
        rendered.push(character);
        *index += character.len_utf8();
        if *is_inline_code {
            *is_inline_code = false;
        } else {
            *is_inline_code = has_inline_code_closer(&markdown[*index..]);
        }

        return true;
    }

    if *is_inline_code {
        rendered.push(character);
        *index += character.len_utf8();

        return true;
    }

    false
}

/// Returns a Markdown fence or fenced-content line that must bypass HTML
/// normalization, together with whether the line toggles the fence state.
fn preserved_code_line(
    markdown: &str,
    index: usize,
    is_fenced_code: bool,
) -> Option<(usize, bool)> {
    if index != 0 && markdown.as_bytes().get(index - 1) != Some(&b'\n') {
        return None;
    }

    let line_end_index = markdown[index..]
        .find('\n')
        .map_or(markdown.len(), |offset| index + offset + 1);
    let is_fence_delimiter = markdown[index..line_end_index].trim().starts_with("```");
    if !is_fenced_code && !is_fence_delimiter {
        return None;
    }

    Some((line_end_index, is_fence_delimiter))
}

/// Whether the remainder of the current Markdown line closes an inline-code
/// span containing at least one character.
fn has_inline_code_closer(markdown: &str) -> bool {
    let current_line = markdown.split_once('\n').map_or(markdown, |(line, _)| line);

    current_line.find('`').is_some_and(|offset| offset > 0)
}

/// Returns the byte index following an HTML comment at `index`.
///
/// Unterminated comments consume the remainder of the body so hidden issue
/// template instructions are never painted as visible text.
fn html_comment_end_index(html: &str, index: usize) -> Option<usize> {
    let suffix = html.get(index..)?;
    if !suffix.starts_with("<!--") {
        return None;
    }

    Some(
        suffix
            .find("-->")
            .map_or(html.len(), |offset| index + offset + 3),
    )
}

/// Parsed representation of one HTML tag embedded in a forge body.
struct HtmlTag<'a> {
    /// Byte index immediately after the closing `>` in the source string.
    end_index: usize,
    /// Whether the tag starts with `/`.
    is_closing: bool,
    /// Lowercase-insensitive tag name without attributes.
    name: &'a str,
}

/// Parses one bounded ASCII HTML tag at `index`, returning `None` for literal
/// `<` characters, oversized tags, or malformed tags.
fn parse_html_tag(html: &str, index: usize) -> Option<HtmlTag<'_>> {
    let suffix = html.get(index..)?;
    if !suffix.starts_with('<') {
        return None;
    }

    let close_offset = html_tag_close_offset(suffix)?;
    let raw_tag = &suffix[1..close_offset];
    let (is_closing, tag_content) = raw_tag
        .strip_prefix('/')
        .map_or((false, raw_tag), |content| (true, content));
    if tag_content.starts_with(char::is_whitespace) {
        return None;
    }
    let name_end = tag_content
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_alphanumeric())
        .map(|(offset, character)| offset + character.len_utf8())
        .last()?;
    let name = &tag_content[..name_end];
    if !name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
    {
        return None;
    }
    let tag_suffix = &tag_content[name_end..];
    if tag_suffix
        .chars()
        .next()
        .is_some_and(|character| !character.is_ascii_whitespace() && character != '/')
    {
        return None;
    }

    Some(HtmlTag {
        end_index: index + close_offset + 1,
        is_closing,
        name,
    })
}

/// Finds the closing `>` for a bounded HTML tag while ignoring delimiters in
/// single- or double-quoted attribute values.
fn html_tag_close_offset(tag: &str) -> Option<usize> {
    let mut attribute_quote = None;

    for (offset, byte) in tag
        .bytes()
        .take(MAX_HTML_TAG_BYTE_COUNT.saturating_add(1))
        .enumerate()
    {
        match (byte, attribute_quote) {
            (b'\'' | b'"', None) => attribute_quote = Some(byte),
            (quote, Some(active_quote)) if quote == active_quote => {
                attribute_quote = None;
            }
            (b'>', None) => return Some(offset),
            _ => {}
        }
    }

    None
}

/// Appends Markdown punctuation or spacing for one recognized HTML tag.
fn append_html_tag_replacement(output: &mut String, tag: &HtmlTag<'_>) {
    match (tag.name.to_ascii_lowercase().as_str(), tag.is_closing) {
        ("h1", false) => append_line_prefix(output, "# "),
        ("h2", false) => append_line_prefix(output, "## "),
        ("h3" | "summary", false) => append_line_prefix(output, "### "),
        ("h4" | "h5" | "h6", false) => append_line_prefix(output, "#### "),
        ("li", false) => append_line_prefix(output, "- "),
        ("blockquote", false) => append_line_prefix(output, "> "),
        ("hr", false) => {
            append_line_prefix(output, "---");
            append_line_break(output);
        }
        ("code" | "kbd", _) => output.push('`'),
        ("strong" | "b", _) => output.push_str("**"),
        ("em" | "i", _) => output.push('*'),
        (
            "br" | "p" | "div" | "details" | "summary" | "blockquote" | "h1" | "h2" | "h3" | "h4"
            | "h5" | "h6" | "li" | "section" | "article",
            true,
        )
        | ("br" | "p" | "div" | "ul" | "ol" | "details" | "section" | "article", false) => {
            append_line_break(output);
        }
        _ => {}
    }
}

/// Appends `prefix` at the beginning of a logical Markdown line.
fn append_line_prefix(output: &mut String, prefix: &str) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }

    output.push_str(prefix);
}

/// Appends a single line break while avoiding duplicate blank lines from
/// adjacent HTML block tags.
fn append_line_break(output: &mut String) {
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

/// Decodes named and numeric HTML entities common in forge bodies.
fn decode_html_entity(input: &str) -> Option<(char, usize)> {
    if !input.starts_with('&') {
        return None;
    }

    let semicolon_index = input
        .bytes()
        .take(MAX_HTML_ENTITY_BYTE_COUNT)
        .position(|byte| byte == b';')?;
    let entity = &input[1..semicolon_index];
    let decoded = match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" => ' ',
        _ => decode_numeric_html_entity(entity)?,
    };

    Some((decoded, semicolon_index + 1))
}

/// Decodes decimal and hexadecimal numeric HTML entities.
fn decode_numeric_html_entity(entity: &str) -> Option<char> {
    let codepoint = if let Some(hexadecimal) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hexadecimal, 16).ok()?
    } else {
        let decimal = entity.strip_prefix('#')?;
        decimal.parse::<u32>().ok()?
    };

    let decoded = char::from_u32(codepoint)?;

    (!decoded.is_control() || matches!(decoded, '\n' | '\t')).then_some(decoded)
}

/// Collapses runs of blank lines produced by neighboring HTML block tags while
/// preserving meaningful leading indentation inside content lines.
fn compact_blank_lines(markdown: &str) -> String {
    let mut compacted = Vec::new();
    let mut is_fenced_code = false;
    let mut previous_blank = false;

    for raw_line in markdown.lines() {
        let is_fence_delimiter = raw_line.trim().starts_with("```");
        let preserve_line = is_fenced_code || is_fence_delimiter;
        let line = if preserve_line {
            raw_line
        } else {
            raw_line.trim_end()
        };
        let is_blank = line.trim().is_empty();
        if !preserve_line && is_blank && previous_blank {
            continue;
        }

        compacted.push(line);
        if is_fence_delimiter {
            is_fenced_code = !is_fenced_code;
        }
        previous_blank = !preserve_line && is_blank;
    }

    compacted.join("\n").trim().to_string()
}

#[cfg(test)]
#[path = "html_test.rs"]
mod tests;
