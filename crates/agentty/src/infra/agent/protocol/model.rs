//! Structured response protocol data model and display helpers.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Hard cap on the number of clarification questions extracted from one agent
/// response. Prevents runaway output from flooding the question UI even when
/// the agent ignores the prompt-level limit.
///
/// This constant is also injected into the protocol instruction prompt
/// templates so the prompt-level guidance and the server-side cap stay in
/// sync automatically.
pub(crate) const MAX_QUESTIONS: usize = 5;

/// Protocol-owned request family preserved across prompt submission and repair
/// retries.
///
/// Session discussion turns and isolated utility prompts share the same
/// top-level [`AgentResponse`] schema. Agentty still carries the request
/// family through transport boundaries so call sites can keep one consistent
/// protocol contract even when some callers ignore parts of the response, such
/// as the optional top-level `summary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtocolRequestProfile {
    /// Interactive session turn.
    SessionTurn,
    /// Isolated utility prompt.
    UtilityPrompt,
}

/// One extracted question with predefined answer choices.
///
/// The UI and persistence layers use this as the canonical clarification
/// question representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "QuestionItem",
    description = "One clarification question emitted by the assistant protocol payload. Keep \
                   each item focused to one actionable decision."
)]
pub struct QuestionItem {
    /// Predefined answer choices the user can select from.
    #[serde(default)]
    #[schemars(
        title = "options",
        description = "Predefined answer choices the user can select from. Keep this list focused \
                       to 1-3 likely answers, put the recommended choice first, and omit deferral \
                       or non-answer choices. Defaults to an empty list when omitted."
    )]
    pub options: Vec<String>,
    /// The clarification question text.
    #[schemars(
        title = "text",
        description = "Human-readable markdown text for this question. Ask one specific \
                       actionable question instead of bundling multiple decisions into one item."
    )]
    pub text: String,
}

impl QuestionItem {
    /// Constructs one clarification question without predefined answer
    /// options.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            options: Vec::new(),
            text: text.into(),
        }
    }

    /// Constructs one clarification question with predefined answer options.
    pub fn with_options(text: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            options,
            text: text.into(),
        }
    }
}

/// Structured session summary block emitted alongside protocol messages.
///
/// Session-discussion turns use this object instead of embedding the change
/// summary inside `answer` message text. One-shot prompts set the top-level
/// `summary` field to `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "AgentResponseSummary",
    description = "Structured session summary block emitted alongside protocol messages instead \
                   of embedding the change summary inside `answer` markdown on session-discussion \
                   turns."
)]
pub struct AgentResponseSummary {
    /// Cumulative summary of active changes on the current session branch.
    #[schemars(
        title = "session",
        description = "Cumulative summary of active changes on the current session branch."
    )]
    pub session: String,
    /// Concise summary of only the work completed in the current turn.
    #[schemars(
        title = "turn",
        description = "Concise summary of only the work completed in the current turn."
    )]
    pub turn: String,
}

/// Wire-format protocol payload used for schema-driven provider output.
///
/// Providers that support output schemas (for example, Codex app-server) are
/// asked to emit this object as the entire assistant response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "AgentResponse",
    description = "Wire-format protocol payload used for schema-driven provider output. Return \
                   this object as the entire assistant response payload. Providers that support \
                   output schemas (for example, Codex app-server) are asked to emit this object \
                   directly."
)]
pub struct AgentResponse {
    /// Markdown answer text emitted for this turn.
    #[serde(default)]
    #[schemars(
        title = "answer",
        description = "Markdown answer text for delivered work, status updates, or concise \
                       completion notes. Keep clarification requests out of this field and emit \
                       them through `questions` instead."
    )]
    pub answer: String,
    /// Ordered clarification questions emitted for this turn.
    #[serde(default)]
    #[schemars(
        title = "questions",
        description = "Ordered clarification questions emitted for this turn. Emit at most \
                       `MAX_QUESTIONS` items, and use an empty array when no user input is \
                       required. Defaults to an empty array when omitted."
    )]
    pub questions: Vec<QuestionItem>,
    /// Structured summary for session-discussion turns, or `None` for legacy
    /// payloads and one-shot prompts.
    #[serde(default)]
    #[schemars(
        title = "summary",
        description = "Structured summary for session-discussion turns, kept outside `answer` \
                       markdown. Use `null` for one-shot prompts and legacy payloads."
    )]
    pub summary: Option<AgentResponseSummary>,
}

/// Structured response parsing failure details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentResponseParseError {
    /// Response was empty or whitespace-only.
    Empty,
    /// Response was JSON, but it did not satisfy the structured protocol
    /// contract.
    InvalidFormat { reason: String },
}

impl fmt::Display for AgentResponseParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "response is empty"),
            Self::InvalidFormat { reason } => {
                write!(formatter, "response is not valid protocol JSON: {reason}")
            }
        }
    }
}

impl AgentResponse {
    /// Creates a plain response from raw text as one `answer` string.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            answer: text.into(),
            questions: Vec::new(),
            summary: None,
        }
    }

    /// Returns display text by joining non-empty answer and question text with
    /// blank lines.
    pub fn to_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        push_display_message(&mut display_messages, &self.answer);
        push_question_display_messages(&mut display_messages, &self.questions);

        display_messages.join("\n\n")
    }

    /// Returns transcript text for session output by joining non-empty
    /// `answer` content with blank lines.
    pub fn to_answer_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        push_display_message(&mut display_messages, &self.answer);

        display_messages.join("\n\n")
    }

    /// Returns the answer as one single-item vector when it is non-empty.
    pub fn answers(&self) -> Vec<String> {
        let answer = self.to_answer_display_text();
        if answer.is_empty() {
            return Vec::new();
        }

        vec![answer]
    }

    /// Returns up to [`MAX_QUESTIONS`] clarification questions in response
    /// order.
    pub fn question_items(&self) -> Vec<QuestionItem> {
        self.questions.iter().take(MAX_QUESTIONS).cloned().collect()
    }
}

/// Appends non-empty clarification question text in order.
fn push_question_display_messages(display_messages: &mut Vec<String>, questions: &[QuestionItem]) {
    for question in questions {
        push_display_message(display_messages, &question.text);
    }
}

/// Appends one non-empty display message.
fn push_display_message(display_messages: &mut Vec<String>, text: &str) {
    if text.trim().is_empty() {
        return;
    }

    display_messages.push(text.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Ensures display text includes the answer and clarification questions in
    /// order.
    fn test_agent_response_to_display_text_joins_answer_and_questions() {
        // Arrange
        let response = AgentResponse {
            answer: "Primary answer".to_string(),
            questions: vec![QuestionItem::new("Need one clarification.")],
            summary: None,
        };

        // Act
        let display_text = response.to_display_text();

        // Assert
        assert_eq!(display_text, "Primary answer\n\nNeed one clarification.");
    }

    #[test]
    /// Ensures question extraction respects the protocol question cap.
    fn test_agent_response_question_items_applies_question_cap() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: (0..=MAX_QUESTIONS)
                .map(|index| QuestionItem::new(format!("Question {index}")))
                .collect(),
            summary: None,
        };

        // Act
        let questions = response.question_items();

        // Assert
        assert_eq!(questions.len(), MAX_QUESTIONS);
    }
}
