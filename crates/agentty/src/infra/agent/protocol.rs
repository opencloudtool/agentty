//! Structured agent communication protocol types and response parsing.
//!
//! Defines the [`AgentResponse`] type returned by agent turns and the
//! [`parse_agent_response`] function that splits a raw assistant message
//! into display text and serde-deserialized metadata. Agents that follow
//! the protocol append a `---agentty-meta---` delimiter followed by a
//! JSON [`AgentResponseMeta`] block at the end of their response. Agents
//! that do not follow the protocol are handled gracefully via a fallback
//! that treats the entire text as a plain answer.

use serde::{Deserialize, Serialize};

/// Delimiter line separating display text from the metadata JSON block.
///
/// Agents are instructed to place this on its own line after their
/// markdown content, followed by a JSON object on subsequent lines.
pub(crate) const METADATA_DELIMITER: &str = "---agentty-meta---";

/// Classification of an agent response.
///
/// Determines how the application processes the response — for example,
/// presenting clarification questions to the user or entering plan
/// approval mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentResponseKind {
    /// Standard completion — the agent finished its work for this turn.
    #[default]
    Answer,
    /// The agent needs clarification before proceeding.
    Question,
    /// The agent is presenting an implementation plan for approval.
    Plan,
}

/// Structured metadata appended by agents after the delimiter.
///
/// Deserialized from the JSON block that follows `---agentty-meta---`.
/// Unknown fields are silently ignored so the protocol can be extended
/// without breaking older versions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResponseMeta {
    /// Classification of the response.
    #[serde(rename = "type", default)]
    pub kind: AgentResponseKind,
    /// Clarification questions extracted from the response, if any.
    #[serde(default)]
    pub questions: Vec<String>,
}

/// Parsed agent response combining display text and structured metadata.
///
/// Produced by [`parse_agent_response`] from the raw assistant message
/// string. The `text` field contains the human-readable markdown content
/// (ready for display), while `meta` carries machine-readable metadata
/// that drives application logic.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// Structured metadata parsed from the JSON block.
    pub meta: AgentResponseMeta,
    /// Human-readable response text for display (markdown).
    pub text: String,
}

impl AgentResponse {
    /// Creates a plain answer response with no metadata.
    ///
    /// Convenience constructor for tests and fallback paths.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            meta: AgentResponseMeta::default(),
            text: text.into(),
        }
    }
}

/// Parses a raw assistant message into an [`AgentResponse`].
///
/// Looks for a `---agentty-meta---` delimiter line. If found, the text
/// before it becomes the display text and the JSON after it is
/// deserialized as [`AgentResponseMeta`]. If the delimiter is absent or
/// the JSON is malformed, the entire raw message is used as display text
/// with default metadata (kind: `Answer`, empty questions).
pub(crate) fn parse_agent_response(raw: &str) -> AgentResponse {
    if let Some(delimiter_pos) = raw.rfind(METADATA_DELIMITER) {
        let text = raw[..delimiter_pos].trim_end().to_string();
        let json_start = delimiter_pos + METADATA_DELIMITER.len();
        let json_part = raw[json_start..].trim();

        if let Ok(meta) = serde_json::from_str::<AgentResponseMeta>(json_part) {
            return AgentResponse { meta, text };
        }
    }

    AgentResponse::plain(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Parses a valid answer metadata block from the response footer.
    fn test_parse_agent_response_answer_with_metadata() {
        // Arrange
        let raw = "Here is my analysis.\n\n---agentty-meta---\n{\"type\": \"answer\"}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, "Here is my analysis.");
        assert_eq!(response.meta.kind, AgentResponseKind::Answer);
        assert!(response.meta.questions.is_empty());
    }

    #[test]
    /// Parses a question response with populated questions array.
    fn test_parse_agent_response_question_with_items() {
        // Arrange
        let raw = "I need some clarification.\n\n---agentty-meta---\n{\"type\": \"question\", \
                   \"questions\": [\"Should we use JWT?\", \"Which table?\"]}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, "I need some clarification.");
        assert_eq!(response.meta.kind, AgentResponseKind::Question);
        assert_eq!(response.meta.questions.len(), 2);
        assert_eq!(response.meta.questions[0], "Should we use JWT?");
        assert_eq!(response.meta.questions[1], "Which table?");
    }

    #[test]
    /// Parses a plan response kind.
    fn test_parse_agent_response_plan_kind() {
        // Arrange
        let raw = "Here is my implementation plan.\n\n---agentty-meta---\n{\"type\": \"plan\"}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, "Here is my implementation plan.");
        assert_eq!(response.meta.kind, AgentResponseKind::Plan);
    }

    #[test]
    /// Falls back to plain answer when no delimiter is present.
    fn test_parse_agent_response_no_delimiter_fallback() {
        // Arrange
        let raw = "Just a normal response with no metadata.";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, raw);
        assert_eq!(response.meta.kind, AgentResponseKind::Answer);
        assert!(response.meta.questions.is_empty());
    }

    #[test]
    /// Falls back to plain answer when JSON after delimiter is malformed.
    fn test_parse_agent_response_malformed_json_fallback() {
        // Arrange
        let raw = "Some text.\n\n---agentty-meta---\n{invalid json}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, raw);
        assert_eq!(response.meta.kind, AgentResponseKind::Answer);
    }

    #[test]
    /// Handles empty text before the delimiter.
    fn test_parse_agent_response_empty_text_before_delimiter() {
        // Arrange
        let raw = "---agentty-meta---\n{\"type\": \"answer\"}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, "");
        assert_eq!(response.meta.kind, AgentResponseKind::Answer);
    }

    #[test]
    /// Uses the last delimiter when multiple exist in the response.
    fn test_parse_agent_response_uses_last_delimiter() {
        // Arrange
        let raw = "First part.\n---agentty-meta---\nThis was not valid JSON.\nSecond \
                   part.\n\n---agentty-meta---\n{\"type\": \"question\", \"questions\": [\"Q1?\"]}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.meta.kind, AgentResponseKind::Question);
        assert_eq!(response.meta.questions.len(), 1);
        assert!(response.text.contains("First part."));
        assert!(response.text.contains("Second part."));
    }

    #[test]
    /// Ignores unknown fields in the metadata JSON (forward compatibility).
    fn test_parse_agent_response_ignores_unknown_fields() {
        // Arrange
        let raw = "Response text.\n\n---agentty-meta---\n{\"type\": \"answer\", \"future_field\": \
                   true, \"extra\": 42}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, "Response text.");
        assert_eq!(response.meta.kind, AgentResponseKind::Answer);
    }

    #[test]
    /// Trims trailing whitespace from the display text.
    fn test_parse_agent_response_trims_trailing_whitespace() {
        // Arrange
        let raw = "Response text.  \n\n\n---agentty-meta---\n{\"type\": \"answer\"}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.text, "Response text.");
    }

    #[test]
    /// Creates a plain answer via the convenience constructor.
    fn test_agent_response_plain_constructor() {
        // Arrange / Act
        let response = AgentResponse::plain("Hello");

        // Assert
        assert_eq!(response.text, "Hello");
        assert_eq!(response.meta.kind, AgentResponseKind::Answer);
        assert!(response.meta.questions.is_empty());
    }

    #[test]
    /// Round-trips metadata through serialization and deserialization.
    fn test_agent_response_meta_serde_round_trip() {
        // Arrange
        let meta = AgentResponseMeta {
            kind: AgentResponseKind::Question,
            questions: vec!["Q1?".to_string(), "Q2?".to_string()],
        };

        // Act
        let json = serde_json::to_string(&meta).expect("serialization should succeed");
        let deserialized: AgentResponseMeta =
            serde_json::from_str(&json).expect("deserialization should succeed");

        // Assert
        assert_eq!(deserialized.kind, AgentResponseKind::Question);
        assert_eq!(deserialized.questions, meta.questions);
    }
}
