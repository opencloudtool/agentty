//! Transport-neutral prompt payloads shared by frontends and agent adapters.

use std::fmt;
use std::path::PathBuf;

/// One local image attachment referenced from a prompt placeholder.
///
/// Serialized JSON consumers must match fields by name; object key order is
/// not part of this transport contract.
#[derive(Debug, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TurnPromptAttachment {
    /// Local file path persisted for transport upload.
    pub local_image_path: PathBuf,
    /// Inline placeholder token such as `[Image #1]` used in prompt text.
    pub placeholder: String,
}

/// Structured prompt payload for one agent turn.
#[derive(Debug, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TurnPrompt {
    /// Ordered local image attachments referenced by `text`.
    pub attachments: Vec<TurnPromptAttachment>,
    /// Prompt text payload, including inline placeholders when present.
    pub text: String,
    /// Source classification that controls prompt-only text rewrites.
    #[serde(default)]
    pub text_source: TurnPromptTextSource,
}

impl TurnPrompt {
    /// Creates a text-only prompt payload.
    #[must_use]
    pub fn from_text(text: String) -> Self {
        Self {
            attachments: Vec::new(),
            text,
            text_source: TurnPromptTextSource::UserPrompt,
        }
    }

    /// Creates a generated-data prompt payload that is sent to the agent
    /// without user prompt `@path` lookup rewriting.
    #[must_use]
    pub fn from_agent_data(text: String) -> Self {
        Self {
            attachments: Vec::new(),
            text,
            text_source: TurnPromptTextSource::AgentData,
        }
    }

    /// Returns whether the payload contains no text and no attachments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.attachments.is_empty()
    }

    /// Returns whether the payload contains one or more image attachments.
    #[must_use]
    pub fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    /// Returns the local image paths referenced by this prompt payload.
    pub fn local_image_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.attachments
            .iter()
            .map(|attachment| &attachment.local_image_path)
    }

    /// Returns whether the prompt text contains `needle`.
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }

    /// Returns whether the prompt text ends with `suffix`.
    #[must_use]
    pub fn ends_with(&self, suffix: &str) -> bool {
        self.text.ends_with(suffix)
    }

    /// Returns the prompt text as it should be sent to an agent runtime.
    ///
    /// User-authored `@path` lookups are rewritten to quoted path tokens for
    /// transport. Generated agent data is returned unchanged so source content
    /// such as Python decorators keeps its leading `@`.
    #[must_use]
    pub fn agent_text(&self) -> String {
        match self.text_source {
            TurnPromptTextSource::UserPrompt => render_prompt_text_for_agent(&self.text),
            TurnPromptTextSource::AgentData => self.text.clone(),
        }
    }

    /// Returns prompt text spans and image attachments in transport order.
    ///
    /// Attachments are ordered by their placeholder position in `text`. Any
    /// attachment whose placeholder no longer exists in the text is appended
    /// after the trailing text so transports can still serialize the image
    /// instead of dropping it silently.
    #[must_use]
    pub fn content_parts(&self) -> Vec<TurnPromptContentPart<'_>> {
        split_turn_prompt_content(&self.text, &self.attachments)
    }

    /// Returns the prompt text as it should be written into persisted
    /// transcripts.
    ///
    /// Inline `[Image #n]` markers are preserved verbatim. If attachment
    /// metadata somehow survives without its placeholder still present in the
    /// text, the missing placeholders are appended in attachment order so the
    /// transcript does not silently become text-only.
    #[must_use]
    pub fn transcript_text(&self) -> String {
        let mut transcript_text = self.text.clone();
        let missing_placeholders = self
            .attachments
            .iter()
            .filter(|attachment| !self.text.contains(&attachment.placeholder))
            .map(|attachment| attachment.placeholder.as_str())
            .collect::<Vec<_>>();

        if missing_placeholders.is_empty() {
            return transcript_text;
        }

        if transcript_text
            .chars()
            .last()
            .is_some_and(|character| !character.is_whitespace())
        {
            transcript_text.push(' ');
        }

        transcript_text.push_str(&missing_placeholders.join(" "));

        transcript_text
    }
}

impl From<String> for TurnPrompt {
    fn from(text: String) -> Self {
        Self::from_text(text)
    }
}

impl From<&str> for TurnPrompt {
    fn from(text: &str) -> Self {
        Self::from_text(text.to_string())
    }
}

impl From<&TurnPrompt> for TurnPrompt {
    fn from(prompt: &TurnPrompt) -> Self {
        prompt.clone()
    }
}

impl fmt::Display for TurnPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl PartialEq<&str> for TurnPrompt {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<TurnPrompt> for &str {
    fn eq(&self, other: &TurnPrompt) -> bool {
        *self == other.text
    }
}

/// Source classification for one turn prompt text payload.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum TurnPromptTextSource {
    /// User-authored prompt text where `@path` lookup tokens should be
    /// converted before transport delivery.
    #[default]
    UserPrompt,
    /// Agent-facing generated data such as utility prompts, protocol repair
    /// prompts, diffs, and transcript previews that must be sent unchanged.
    AgentData,
}

/// Ordered content piece produced when serializing one turn prompt.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TurnPromptContentPart<'prompt> {
    /// One local image attachment referenced from the prompt text.
    Attachment(&'prompt TurnPromptAttachment),
    /// One attachment whose placeholder no longer appears in the prompt text.
    OrphanAttachment(&'prompt TurnPromptAttachment),
    /// One plain-text span from the prompt.
    Text(&'prompt str),
}

/// Rewrites user-entered `@` lookups into quoted agent-facing path tokens.
///
/// File and directory lookups like `@path/to/file` are rewritten to
/// `"path/to/file"`. Non-lookup uses of `@`, including email-like tokens and
/// lone `@`, are preserved so frontends and persisted transcripts can continue
/// storing the original user input unchanged.
#[must_use]
pub fn render_prompt_text_for_agent(text: &str) -> String {
    let characters = text.chars().collect::<Vec<char>>();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;

    while let Some(&character) = characters.get(index) {
        if character != '@'
            || !is_at_mention_boundary(characters.get(index.wrapping_sub(1)).copied())
            || index + 1 >= characters.len()
        {
            output.push(character);
            index += 1;

            continue;
        }

        let mut scan_index = index + 1;
        while scan_index < characters.len() && is_at_mention_query_character(characters[scan_index])
        {
            scan_index += 1;
        }

        if scan_index == index + 1 {
            output.push(character);
            index += 1;

            continue;
        }

        output.push('"');
        output.extend(characters[index + 1..scan_index].iter());
        output.push('"');
        index = scan_index;
    }

    output
}

/// Splits prompt text into ordered text and attachment parts for transport
/// serialization.
#[must_use]
pub fn split_turn_prompt_content<'prompt>(
    text: &'prompt str,
    attachments: &'prompt [TurnPromptAttachment],
) -> Vec<TurnPromptContentPart<'prompt>> {
    if attachments.is_empty() {
        return vec![TurnPromptContentPart::Text(text)];
    }

    let mut ordered_attachments = attachments.iter().collect::<Vec<_>>();
    ordered_attachments
        // Attachments without an inline placeholder are appended after the
        // text-bearing attachments so transcript order stays deterministic.
        .sort_by_key(|attachment| text.find(&attachment.placeholder).unwrap_or(usize::MAX));

    let mut content_parts = Vec::new();
    let mut orphan_attachments = Vec::new();
    let mut remaining_text = text;

    for attachment in ordered_attachments {
        if let Some(placeholder_index) = remaining_text.find(&attachment.placeholder) {
            let (before_placeholder, after_placeholder) =
                remaining_text.split_at(placeholder_index);

            if !before_placeholder.is_empty() {
                content_parts.push(TurnPromptContentPart::Text(before_placeholder));
            }

            content_parts.push(TurnPromptContentPart::Attachment(attachment));
            remaining_text = &after_placeholder[attachment.placeholder.len()..];

            continue;
        }

        orphan_attachments.push(attachment);
    }

    if !remaining_text.is_empty() {
        content_parts.push(TurnPromptContentPart::Text(remaining_text));
    }

    content_parts.extend(
        orphan_attachments
            .into_iter()
            .map(TurnPromptContentPart::OrphanAttachment),
    );

    content_parts
}

/// Returns whether the character before `@` starts a file lookup token.
fn is_at_mention_boundary(previous_character: Option<char>) -> bool {
    previous_character.is_none_or(|character| {
        character.is_whitespace() || is_at_mention_opening_delimiter(character)
    })
}

/// Returns whether `character` can appear inside an active `@` lookup token.
fn is_at_mention_query_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
}

/// Returns whether `character` is an opening delimiter that can precede `@`.
fn is_at_mention_opening_delimiter(character: char) -> bool {
    matches!(character, '(' | '[' | '{')
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
