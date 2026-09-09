use std::fmt::Write;
use std::sync::Arc;

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
    let input = " › /model antigravity";

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
    assert_eq!(lines[1].spans[1].style.fg, Some(style::palette::text()));
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
fn test_render_markdown_styles_clarification_block_differently_from_user_prompt() {
    // Arrange
    let input = " › Clarifications:\n   1. Q: Need tests?\n      A: Yes";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[1].to_string().trim_end(), " › Clarifications:");
    assert_eq!(lines[1].spans[0].style, clarification_prompt_prefix_style());
    assert_eq!(lines[1].spans[1].style, clarification_header_style());
    assert_ne!(lines[1].spans[1].style.bg, user_prompt_content_style().bg);
    assert!(lines[2].spans.iter().any(|span| {
        span.content.as_ref() == "1. " && span.style == clarification_question_index_style()
    }));
    assert!(lines[2].spans.iter().any(|span| {
        span.content.as_ref() == "Q: " && span.style == clarification_question_label_style()
    }));
    assert!(lines[3].spans.iter().any(|span| {
        span.content.as_ref() == "A: " && span.style == clarification_answer_label_style()
    }));
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
fn test_render_markdown_wraps_user_prompt_on_word_boundaries() {
    // Arrange
    let input = " › one two three";

    // Act
    let lines = render_markdown(input, 8);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(rendered_lines.contains(&" › one".to_string()));
    assert!(rendered_lines.contains(&"   two".to_string()));
    assert!(rendered_lines.contains(&"   three".to_string()));
}

#[test]
fn test_render_markdown_wraps_clarification_answer_on_word_boundaries() {
    // Arrange
    let input =
        " › Clarifications:\n   1. Q: Need tests?\n      A: very long answer text for review";

    // Act
    let lines = render_markdown(input, 18);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(rendered_lines.contains(&"      A: very".to_string()));
    assert!(
        rendered_lines
            .iter()
            .any(|line| line.trim_start().starts_with("long answer"))
    );
    assert!(
        rendered_lines
            .iter()
            .any(|line| line.trim_start().starts_with("text for"))
    );
    assert!(
        rendered_lines
            .iter()
            .any(|line| line.trim_start().starts_with("review"))
    );
}

#[test]
fn test_render_markdown_wraps_long_prompt_word_with_hard_fallback() {
    // Arrange
    let input = " › supercalifragilisticexpialidocious";

    // Act
    let lines = render_markdown(input, 8);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(rendered_lines.contains(&" › super".to_string()));
    assert!(rendered_lines.contains(&"   calif".to_string()));
    assert!(rendered_lines.contains(&"   ragil".to_string()));
}

#[test]
fn test_wrap_verbatim_spans_with_word_boundaries_handles_wide_characters() {
    // Arrange
    let spans = vec![Span::raw("你好 你好".to_string())];

    // Act
    let lines = wrap_verbatim_spans_with_word_boundaries(spans, 5);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "你好 ");
    assert_eq!(lines[0].width(), 5);
    assert_eq!(lines[1].to_string(), "你好");
    assert_eq!(lines[1].width(), 4);
}

#[test]
fn test_wrap_verbatim_spans_with_word_boundaries_wraps_when_word_reaches_edge() {
    // Arrange
    let spans = vec![Span::raw("foo bar".to_string())];

    // Act
    let lines = wrap_verbatim_spans_with_word_boundaries(spans, 7);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "foo ");
    assert_eq!(lines[1].to_string(), "bar");
}

#[test]
fn test_wrap_verbatim_spans_handles_wide_characters() {
    // Arrange
    let spans = vec![Span::raw("你好你好".to_string())];

    // Act
    let lines = wrap_verbatim_spans(spans, 5);

    // Assert
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "你好");
    assert_eq!(lines[0].width(), 4);
    assert_eq!(lines[1].to_string(), "你好");
    assert_eq!(lines[1].width(), 4);
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
fn test_render_markdown_renders_inline_right_arrow_math_symbol() {
    // Arrange
    let input = r"Move $\rightarrow$ forward.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), "Move → forward.");
}

#[test]
fn test_render_markdown_renders_inline_right_arrow_math_inside_bold() {
    // Arrange
    let input = r"Move **$\rightarrow$** forward.";

    // Act
    let lines = render_markdown(input, 80);
    let arrow_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "→")
        .expect("right arrow should render");

    // Assert
    assert!(arrow_span.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(lines[0].to_string(), "Move → forward.");
}

#[test]
fn test_render_markdown_renders_inline_right_arrow_math_inside_italic() {
    // Arrange
    let input = r"Move *$\rightarrow$* forward.";

    // Act
    let lines = render_markdown(input, 80);
    let arrow_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "→")
        .expect("right arrow should render");

    // Assert
    assert!(arrow_span.style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(lines[0].to_string(), "Move → forward.");
}

#[test]
fn test_render_markdown_preserves_unsupported_inline_math() {
    // Arrange
    let input = r"Keep $x + y$, unmatched $\rightarrow literal, and $$text $\rightarrow$ literal.";

    // Act
    let lines = render_markdown(input, 120);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_display_math() {
    // Arrange
    let input = r"Keep $$text $\rightarrow$ text$$ literal.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_bold_syntax_inside_display_math() {
    // Arrange
    let input = r"Keep $$text **$\rightarrow$** text$$ literal.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_italic_syntax_inside_display_math() {
    // Arrange
    let input = r"Keep $$text *$\rightarrow$* text$$ literal.";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), input);
}

#[test]
fn test_render_markdown_preserves_display_math_inside_bold() {
    // Arrange
    let input = r"Keep **$$text $\rightarrow$ text$$** literal.";

    // Act
    let lines = render_markdown(input, 80);
    let math_text = lines[0]
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
        .map(|span| span.content.as_ref())
        .collect::<String>();

    // Assert
    assert_eq!(math_text, r"$$text $\rightarrow$ text$$");
    assert_eq!(
        lines[0].to_string(),
        r"Keep $$text $\rightarrow$ text$$ literal."
    );
}

#[test]
fn test_render_markdown_preserves_display_math_inside_italic() {
    // Arrange
    let input = r"Keep *$$text $\rightarrow$ text$$* literal.";

    // Act
    let lines = render_markdown(input, 80);
    let math_text = lines[0]
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::ITALIC))
        .map(|span| span.content.as_ref())
        .collect::<String>();

    // Assert
    assert_eq!(math_text, r"$$text $\rightarrow$ text$$");
    assert_eq!(
        lines[0].to_string(),
        r"Keep $$text $\rightarrow$ text$$ literal."
    );
}

#[test]
fn test_render_markdown_preserves_inline_code_inside_bold() {
    // Arrange
    let input = r"Keep **`$\rightarrow$`** literal.";

    // Act
    let lines = render_markdown(input, 80);
    let code_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == r"`$\rightarrow$`")
        .expect("inline code should remain literal");

    // Assert
    assert!(code_span.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(lines[0].to_string(), r"Keep `$\rightarrow$` literal.");
}

#[test]
fn test_render_markdown_preserves_inline_code_inside_italic() {
    // Arrange
    let input = r"Keep *`$\rightarrow$`* literal.";

    // Act
    let lines = render_markdown(input, 80);
    let code_span = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == r"`$\rightarrow$`")
        .expect("inline code should remain literal");

    // Assert
    assert!(code_span.style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(lines[0].to_string(), r"Keep `$\rightarrow$` literal.");
}

#[test]
fn test_render_markdown_keeps_inline_style_punctuation_adjacent() {
    // Arrange
    let input = "Use (`session_messages_from_rows`), then [`Image #1`].";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].to_string(),
        "Use (session_messages_from_rows), then [Image #1]."
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
fn test_render_markdown_wraps_fenced_code_on_word_boundaries() {
    // Arrange
    let input = "```text\nformatted blocks in user messages without words breaking\n```";

    // Act
    let lines = render_markdown(input, 32);
    let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

    // Assert
    assert_eq!(rendered_lines[0], "formatted blocks in user ");
    assert_eq!(rendered_lines[1], "messages without words breaking");
    assert!(!rendered_lines.iter().any(|line| line.ends_with("message")));
    assert!(!rendered_lines.iter().any(|line| line.starts_with("s ")));
    assert!(lines.iter().all(|line| {
        line.spans
            .first()
            .is_some_and(|span| span.style == code_block_style())
    }));
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
fn test_render_markdown_renders_mermaid_block_as_diagram() {
    // Arrange
    let input = "```mermaid {theme=default}\ngraph TD\n    A[Start] --> B[Finish]\n```";

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains("┌"));
    assert!(text.contains("▼"));
    assert!(!text.contains("graph TD"));
    assert!(!text.contains("```"));
}

#[test]
fn test_render_markdown_stacks_over_wide_left_right_mermaid_block() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "flowchart LR\n",
        "    Q[Qwen complete] --> T[Tracing spans and events]\n",
        "    Q --> M[OTel metrics API]\n",
        "    T --> S[Trace and log providers]\n",
        "    M --> P[Meter provider]\n",
        "    S --> O[OTLP HTTP protobuf]\n",
        "    P --> O\n",
        "    O --> C[Collector on port 4318]\n",
        "    C --> B[Telemetry backends]\n",
        "    B --> G[Grafana on port 3000]\n",
        "```",
    );

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Qwen complete"));
    assert!(text.contains("Tracing spans and events"));
    assert!(text.contains("Grafana on port 3000"));
    assert!(text.contains('▼'));
    assert!(!text.contains("flowchart LR"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_mermaid_prefix_language() {
    // Arrange
    let input = "```mermaid-diagram\ngraph TD\n    A[Start] --> B[Finish]\n```";

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("graph TD"));
    assert!(text.contains("A[Start] --> B[Finish]"));
    assert!(!text.contains("▼"));
    assert_eq!(lines[0].spans[0].style, code_block_style());
}

#[test]
fn test_render_markdown_accepts_mermaid_fence_with_tab_separator() {
    // Arrange
    let input = "```mermaid\t{theme=default}\ngraph TD\n    A[Start] --> B[Finish]\n```";

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains("▼"));
    assert!(!text.contains("graph TD"));
    assert!(!text.contains("```"));
}

#[test]
fn test_render_markdown_mermaid_uses_injected_palette() {
    // Arrange
    let input = "```mermaid\ngraph TD\n    A[Start] --> B[Finish]\n```";
    let settings = TextRenderSettings {
        cache_version: 7,
        palette: crate::TextPalette {
            text: Color::Red,
            ..crate::TextPalette::default()
        },
    };

    // Act
    let lines = render_markdown_with_settings(input, 80, settings);

    // Assert
    let start_span = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains("Start"))
        .expect("start label should render");
    assert_eq!(start_span.style.fg, Some(Color::Red));
}

#[test]
fn test_render_markdown_renders_feedback_mermaid_block_as_diagram() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "flowchart LR\n",
        "    A[\"App\"] -- \"commands:<br/>prompt · interrupt · permission answer\" --> \
         H[\"ag-harness\"]\n",
        "    H -- \"typed events:<br/>deltas · tool calls · diffs · usage\" --> A\n",
        "```",
    );

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("App"));
    assert!(text.contains("ag-harness"));
    assert!(text.contains("commands:"));
    assert!(text.contains("typed events:"));
    assert!(text.contains("◀"));
    assert!(!text.contains("flowchart LR"));
    assert!(!text.contains("<br/>"));
}

#[test]
fn test_render_markdown_renders_multi_node_feedback_mermaid_block_as_diagram() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "flowchart LR\n",
        "    U[User and TUI] --> C[Orchestrator controller]\n",
        "    M[Agent model] --> P[Typed command response]\n",
        "    P --> C\n",
        "    C --> S[ag-session service]\n",
        "    S --> A[Agentty host adapter]\n",
        "    A --> W[Session workers]\n",
        "    W --> E[Session events]\n",
        "    E --> C\n",
        "    C --> M\n",
        "```",
    );

    // Act
    let lines = render_markdown(input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("Orchestrator controller"));
    assert!(text.contains("Session events"));
    assert!(text.contains("Session events ───▶ Orchestrator controller"));
    assert!(text.contains("Orchestrator controller ───▶ Agent model"));
    assert!(!text.contains("flowchart LR"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_unsupported_mermaid() {
    // Arrange
    let input = "```mermaid\nclassDiagram\n    A <|-- B\n```";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[0].to_string(), "classDiagram");
    assert_eq!(lines[0].spans[0].style, code_block_style());
}

#[test]
fn test_render_markdown_renders_sequence_mermaid_block_as_diagram() {
    // Arrange
    let input = concat!(
        "```mermaid\n",
        "sequenceDiagram\n",
        "    participant User\n",
        "    participant Agentty\n",
        "    User->>Agentty: Start session\n",
        "```\n",
    );

    // Act
    let lines = render_markdown(input, 120);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("User"));
    assert!(text.contains("Agentty"));
    assert!(text.contains("Start session"));
    assert!(!text.contains("sequenceDiagram"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_unclosed_mermaid_fence() {
    // Arrange
    let input = "```mermaid\ngraph TD\n    A[Start] --> B[Finish]";

    // Act
    let lines = render_markdown(input, 80);

    // Assert
    assert_eq!(lines[0].to_string(), "graph TD");
    assert_eq!(lines[0].spans[0].style, code_block_style());
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_diagram_wider_than_width() {
    // Arrange
    let input = "```mermaid\ngraph TD\n    A[Start] --> B[Long finish label]\n```";

    // Act
    let lines = render_markdown(input, 10);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("graph TD"));
    assert!(!text.contains("┌"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_line_limited_mermaid_source() {
    // Arrange
    let mut input = String::from("```mermaid\ngraph TD");
    for node_index in 0..mermaid::MAX_SOURCE_LINE_COUNT {
        write!(&mut input, "\n    N{node_index}").expect("writing to String should succeed");
    }
    input.push_str("\n```");

    // Act
    let lines = render_markdown(&input, 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("graph TD"));
    assert!(!text.contains("┌"));
}

#[test]
fn test_render_markdown_keeps_code_fallback_for_byte_limited_mermaid_source() {
    // Arrange
    let label = "x".repeat(mermaid::MAX_SOURCE_BYTE_COUNT);
    let input = format!("```mermaid\ngraph TD\n    A[{label}]\n```");

    // Act
    let lines = render_markdown(&input, 80);

    // Assert
    assert_eq!(lines[0].to_string(), "graph TD");
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

#[test]
fn test_render_markdown_renders_pipe_table() {
    // Arrange
    let input = "| Name | Status |\n| --- | ---: |\n| Build | passing |\n| Docs | queued |";

    // Act
    let lines = render_markdown(input, 80);
    let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    let rendered_text = rendered_lines.join("\n");

    // Assert
    assert!(rendered_text.contains("Name"));
    assert!(rendered_text.contains("Status"));
    assert!(rendered_text.contains("Build"));
    assert!(rendered_text.contains("passing"));
    assert!(!rendered_text.contains("| --- | ---: |"));
    assert!(rendered_lines.iter().any(|line| line.starts_with("┌")));
    assert!(rendered_lines.iter().any(|line| line.starts_with("├")));
    assert!(lines[1].spans.iter().any(|span| {
        span.content.as_ref().contains("Name") && span.style == table_header_style()
    }));
}

#[test]
fn test_markdown_block_preservation_mask_uses_shared_block_classifiers() {
    // Arrange
    let input = concat!(
        "  plain\n",
        "  | Name | Status |\n",
        "  | --- | ---: |\n",
        "  | Build | passing |\n",
        "\n",
        "  ---\n",
        "  ```text\n",
        "    fenced\n",
        "  ```",
    );

    // Act
    let preservation_mask = markdown_block_preservation_mask(input);

    // Assert
    assert_eq!(
        preservation_mask,
        vec![false, true, true, true, false, true, true, true, true]
    );
}

#[test]
fn test_render_markdown_wraps_table_cells_to_available_width() {
    // Arrange
    let input = "| Column | Notes |\n| --- | --- |\n| One | alpha beta gamma delta |";

    // Act
    let lines = render_markdown(input, 20);
    let rendered_text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(rendered_text.contains("alpha"));
    assert!(rendered_text.contains("beta"));
    assert!(rendered_text.contains("gamma"));
    assert!(lines.iter().all(|line| line.width() <= 20));
}

#[test]
fn test_markdown_render_cache_retains_multiple_entries() {
    // Arrange
    let cache = MarkdownRenderCache::default();

    // Act
    let first_lines = cache.render("# First", 24);
    let second_lines = cache.render("# Second", 24);
    let cached_first_lines = cache.render("# First", 24);

    // Assert
    assert!(Arc::ptr_eq(&first_lines, &cached_first_lines));
    assert_eq!(
        second_lines.as_ref(),
        render_markdown("# Second", 24).as_slice()
    );
    assert_eq!(cache.entries.borrow().len(), 2);
}

#[test]
fn test_markdown_render_cache_reuses_custom_renderer_result() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let settings = TextRenderSettings::default();
    let render_count = Cell::new(0);

    // Act
    let first_lines =
        cache.render_with_settings_and_renderer("custom", 24, settings, |_text, _width| {
            render_count.set(render_count.get() + 1);

            vec![Line::from("custom render")]
        });
    let cached_lines =
        cache.render_with_settings_and_renderer("custom", 24, settings, |_text, _width| {
            render_count.set(render_count.get() + 1);

            vec![Line::from("unexpected render")]
        });

    // Assert
    assert!(Arc::ptr_eq(&first_lines, &cached_lines));
    assert_eq!(first_lines[0].to_string(), "custom render");
    assert_eq!(render_count.get(), 1);
}

#[test]
fn test_markdown_render_cache_evicts_least_recently_used_entry() {
    // Arrange
    let cache = MarkdownRenderCache::default();

    // Act
    for index in 0..MARKDOWN_RENDER_CACHE_ENTRY_LIMIT {
        let markdown = format!("# Entry {index}");
        cache.render(&markdown, 24);
    }
    cache.render("# Entry 0", 24);
    cache.render("# Overflow", 24);

    // Assert
    let cached_hashes = cache
        .entries
        .borrow()
        .iter()
        .map(|entry| entry.key.content_hash)
        .collect::<Vec<_>>();
    assert_eq!(cached_hashes.len(), MARKDOWN_RENDER_CACHE_ENTRY_LIMIT);
    assert!(cached_hashes.contains(&MarkdownRenderCache::hash_text("# Entry 0")));
    assert!(!cached_hashes.contains(&MarkdownRenderCache::hash_text("# Entry 1")));
    assert!(cached_hashes.contains(&MarkdownRenderCache::hash_text("# Overflow")));
}

#[test]
fn test_markdown_render_cache_uses_injected_palette() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let settings = TextRenderSettings {
        cache_version: 7,
        palette: crate::TextPalette {
            accent: Color::Red,
            ..crate::TextPalette::default()
        },
    };

    // Act
    let lines = cache.render_with_settings("# Entry", 24, settings);

    // Assert
    assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
}

#[test]
fn test_markdown_render_cache_bump_version_clears_styled_entries() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    let initial_lines = cache.render("# Entry", 24);

    // Act
    cache.bump_version();
    let refreshed_lines = cache.render("# Entry", 24);

    // Assert
    assert!(!Arc::ptr_eq(&initial_lines, &refreshed_lines));
    assert_eq!(cache.entries.borrow().len(), 1);
    assert_eq!(cache.version.get(), 1);
}
