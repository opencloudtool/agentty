use ratatui::style::Modifier;

use super::*;

#[test]
fn test_render_html_normalizes_block_and_inline_markup() {
    // Arrange
    let input = concat!(
        "<details><summary>Release notes</summary>",
        "<h1>One</h1><h2>Two</h2><h3>Three</h3><h4>Four</h4>",
        "<h5>Five</h5><h6>Six</h6>",
        "<ul><li><strong>Fix</strong> <em>parser</em> ",
        "with <code>fast</code> and <kbd>Enter</kbd>.</li></ul>",
        "<blockquote>Quoted</blockquote><hr></details>",
    );

    // Act
    let lines = render_html(input, 80);
    let text = lines.iter().map(Line::to_string).collect::<Vec<_>>();

    // Assert
    assert!(text.contains(&"Release notes".to_string()));
    assert!(text.contains(&"One".to_string()));
    assert!(text.contains(&"Two".to_string()));
    assert!(text.contains(&"Three".to_string()));
    assert!(text.contains(&"Four".to_string()));
    assert!(text.contains(&"Five".to_string()));
    assert!(text.contains(&"Six".to_string()));
    assert!(text.contains(&"- Fix parser with fast and Enter.".to_string()));
    assert!(text.contains(&"│ Quoted".to_string()));
    assert!(lines.iter().any(|line| line.spans.iter().any(|span| {
        span.content.as_ref() == "Fix" && span.style.add_modifier.contains(Modifier::BOLD)
    })));
    assert!(!text.join("\n").contains('<'));
}

#[test]
fn test_render_html_handles_layout_tags_comments_and_entities() {
    // Arrange
    let input = concat!(
        "<!-- hidden issue template -->",
        "<article><section><div><p>A&amp;B&lt;C&gt;D &quot;Q&quot; ",
        "&apos;x&apos; &#39;y&#39; &nbsp; &#65; &#x42; &#X43;</p>",
        "line<br>next</div></section></article>",
    );

    // Act
    let lines = render_html_with_settings(input, 80, TextRenderSettings::default());
    let text = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert_eq!(text, "A&B<C>D \"Q\" 'x' 'y' A B C\nline\nnext");
    assert!(!text.contains("hidden issue template"));
}

#[test]
fn test_html_to_markdown_preserves_literal_and_malformed_markup() {
    // Arrange
    let input = "Keep 2 < 3 and 5 > 4, &unknown;, <1>, <broken, and <x>tag</x>.";

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(
        rendered,
        "Keep 2 < 3 and 5 > 4, &unknown;, <1>, <broken, and tag."
    );
}

#[test]
fn test_html_to_markdown_preserves_whitespace_prefixed_tags() {
    // Arrange
    let input = "a < b > c and d </ b > e";

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(rendered, input);
}

#[test]
fn test_html_to_markdown_ignores_tag_delimiters_inside_quoted_attributes() {
    // Arrange
    let input = concat!(
        r#"<a title="x ' > y">double quoted</a> and "#,
        r#"<span data-note='a " > b'>single quoted</span>"#,
    );

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(rendered, "double quoted and single quoted");
}

#[test]
fn test_html_to_markdown_preserves_fenced_markdown_code() {
    // Arrange
    let input = concat!(
        "Before <strong>bold</strong>\n",
        "```html\n",
        "<div>&amp;</div>  \n",
        "\n",
        "```\n",
        "After <em>text</em>",
    );

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(
        rendered,
        concat!(
            "Before **bold**\n",
            "```html\n",
            "<div>&amp;</div>  \n",
            "\n",
            "```\n",
            "After *text*",
        )
    );
}

#[test]
fn test_html_to_markdown_preserves_inline_markdown_code() {
    // Arrange
    let input = concat!(
        "Use `<div>&amp;</div>` with <strong>care</strong>. ",
        "An unmatched ` leaves <em>markup</em> active.",
    );

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(
        rendered,
        concat!(
            "Use `<div>&amp;</div>` with **care**. ",
            "An unmatched ` leaves *markup* active.",
        )
    );
}

#[test]
fn test_html_to_markdown_discards_unterminated_comment_and_invalid_entities() {
    // Arrange
    let input = "Visible &; &#; &#x; &#99999999;<!-- hidden forever";

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(rendered, "Visible &; &#; &#x; &#99999999;");
}

#[test]
fn test_html_to_markdown_preserves_oversized_tag_text() {
    // Arrange
    let input = format!(
        "<div {}>visible",
        "x".repeat(MAX_HTML_TAG_BYTE_COUNT.saturating_add(1))
    );

    // Act
    let rendered = html_to_markdown(&input);

    // Assert
    assert_eq!(rendered, input);
}

#[test]
fn test_html_to_markdown_bounds_input_at_utf8_boundary() {
    // Arrange
    let mut input = "x".repeat(MAX_HTML_INPUT_BYTE_COUNT - 1);
    input.push('é');
    input.push_str("<strong>hidden overflow</strong>");

    // Act
    let rendered = html_to_markdown(&input);

    // Assert
    assert_eq!(
        rendered.len(),
        MAX_HTML_INPUT_BYTE_COUNT - 1 + HTML_INPUT_TRUNCATION_NOTICE.len()
    );
    assert!(rendered.ends_with(HTML_INPUT_TRUNCATION_NOTICE));
    assert!(!rendered.contains("hidden overflow"));
}

#[test]
fn test_html_to_markdown_bounds_malformed_entity_scanning() {
    // Arrange
    let input = format!("{};", "&".repeat(100_000));

    // Act
    let rendered = html_to_markdown(&input);

    // Assert
    assert_eq!(rendered, input);
}

#[test]
fn test_decode_html_entity_accepts_maximum_supported_length() {
    // Arrange
    let input = "&#1114111;";

    // Act
    let decoded = decode_html_entity(input);

    // Assert
    assert_eq!(decoded, Some(('\u{10ffff}', MAX_HTML_ENTITY_BYTE_COUNT)));
}

#[test]
fn test_html_to_markdown_rejects_control_character_entities() {
    // Arrange
    let input = concat!(
        "Keep &#x1b;[2J, &#27;[H, &#127;, and &#159;; ",
        "allow &#10;line and &#9;tab.",
    );

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(
        rendered,
        concat!(
            "Keep &#x1b;[2J, &#27;[H, &#127;, and &#159;; allow\n",
            "line and \ttab.",
        )
    );
    assert!(!rendered.contains('\u{1b}'));
}

#[test]
fn test_html_to_markdown_compacts_adjacent_block_spacing() {
    // Arrange
    let input = "<p>First</p>\n\n<div>Second</div><ol><li>Third</li></ol>";

    // Act
    let rendered = html_to_markdown(input);

    // Assert
    assert_eq!(rendered, "First\n\nSecond\n- Third");
}

#[test]
fn test_append_line_prefix_starts_a_new_logical_line() {
    // Arrange
    let mut output = "Existing".to_string();

    // Act
    append_line_prefix(&mut output, "# ");

    // Assert
    assert_eq!(output, "Existing\n# ");
}
