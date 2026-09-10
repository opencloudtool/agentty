use std::path::PathBuf;

use super::*;

#[test]
/// Ensures attachment JSON accepts either object key order while
/// preserving the complete named-field wire shape.
fn test_turn_prompt_attachment_json_uses_named_fields_independent_of_order() {
    // Arrange
    let attachment = TurnPromptAttachment {
        local_image_path: PathBuf::from("/tmp/image-1.png"),
        placeholder: "[Image #1]".to_string(),
    };
    let alternate_order_json =
        r#"{"placeholder":"[Image #1]","local_image_path":"/tmp/image-1.png"}"#;

    // Act
    let deserialized_attachment =
        serde_json::from_str::<TurnPromptAttachment>(alternate_order_json)
            .expect("attachment JSON should deserialize by field name");
    let serialized_value = serde_json::to_value(&attachment).expect("attachment should serialize");

    // Assert
    assert_eq!(deserialized_attachment, attachment);
    assert_eq!(
        serialized_value,
        serde_json::json!({
            "local_image_path": "/tmp/image-1.png",
            "placeholder": "[Image #1]",
        })
    );
}

#[test]
/// Ensures prompt text comparisons work with string slices on either side.
fn test_turn_prompt_compares_with_str_in_both_directions() {
    // Arrange
    let prompt = TurnPrompt::from("Review this change");

    // Act
    let prompt_matches = prompt == "Review this change";
    let text_matches = "Review this change" == prompt;

    // Assert
    assert!(prompt_matches);
    assert!(text_matches);
}

#[test]
/// Ensures transcript text keeps inline image placeholders unchanged.
fn test_turn_prompt_transcript_text_keeps_inline_placeholders() {
    // Arrange
    let prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            local_image_path: PathBuf::from("/tmp/image-1.png"),
            placeholder: "[Image #1]".to_string(),
        }],
        text: "Review [Image #1] carefully".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let transcript_text = prompt.transcript_text();

    // Assert
    assert_eq!(transcript_text, "Review [Image #1] carefully");
}

#[test]
/// Ensures agent-bound prompt text rewrites raw `@` lookups without
/// mutating transcript-facing text.
fn test_turn_prompt_agent_text_rewrites_user_at_lookups() {
    // Arrange
    let prompt = TurnPrompt::from("Review @src/main.rs and person@example.com");

    // Act
    let agent_text = prompt.agent_text();
    let transcript_text = prompt.transcript_text();

    // Assert
    assert_eq!(agent_text, "Review \"src/main.rs\" and person@example.com");
    assert_eq!(
        transcript_text,
        "Review @src/main.rs and person@example.com"
    );
}

#[test]
/// Ensures generated agent data bypasses user prompt `@` lookup rewriting.
fn test_turn_prompt_agent_text_preserves_agent_data_at_tokens() {
    // Arrange
    let prompt = TurnPrompt::from_agent_data(
        "Diff:\n```diff\n+@dataclass\n+class Config:\n+    pass\n```".to_string(),
    );

    // Act
    let agent_text = prompt.agent_text();

    // Assert
    assert!(agent_text.contains("+@dataclass"));
    assert!(!agent_text.contains("+\"dataclass\""));
}

#[test]
/// Ensures transcript text appends any attachment markers missing from the
/// text payload.
fn test_turn_prompt_transcript_text_appends_missing_placeholders() {
    // Arrange
    let prompt = TurnPrompt {
        attachments: vec![
            TurnPromptAttachment {
                local_image_path: PathBuf::from("/tmp/image-1.png"),
                placeholder: "[Image #1]".to_string(),
            },
            TurnPromptAttachment {
                local_image_path: PathBuf::from("/tmp/image-2.png"),
                placeholder: "[Image #2]".to_string(),
            },
        ],
        text: "Review".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let transcript_text = prompt.transcript_text();

    // Assert
    assert_eq!(transcript_text, "Review [Image #1] [Image #2]");
}

#[test]
/// Ensures prompt content without attachments remains one text part.
fn test_split_turn_prompt_content_returns_text_without_attachments() {
    // Arrange
    let text = "Review this change";

    // Act
    let content_parts = split_turn_prompt_content(text, &[]);

    // Assert
    assert_eq!(content_parts, vec![TurnPromptContentPart::Text(text)]);
}

#[test]
/// Ensures prompt content parts follow placeholder order and keep orphaned
/// attachments at the end.
fn test_split_turn_prompt_content_orders_placeholders_and_appends_orphans() {
    // Arrange
    let attachments = vec![
        TurnPromptAttachment {
            local_image_path: PathBuf::from("/tmp/image-1.png"),
            placeholder: "[Image #1]".to_string(),
        },
        TurnPromptAttachment {
            local_image_path: PathBuf::from("/tmp/image-2.png"),
            placeholder: "[Image #2]".to_string(),
        },
        TurnPromptAttachment {
            local_image_path: PathBuf::from("/tmp/image-3.png"),
            placeholder: "[Image #3]".to_string(),
        },
    ];

    // Act
    let content_parts =
        split_turn_prompt_content("Compare [Image #2] with [Image #1] now", &attachments);

    // Assert
    assert_eq!(
        content_parts,
        vec![
            TurnPromptContentPart::Text("Compare "),
            TurnPromptContentPart::Attachment(&attachments[1]),
            TurnPromptContentPart::Text(" with "),
            TurnPromptContentPart::Attachment(&attachments[0]),
            TurnPromptContentPart::Text(" now"),
            TurnPromptContentPart::OrphanAttachment(&attachments[2]),
        ]
    );
}
