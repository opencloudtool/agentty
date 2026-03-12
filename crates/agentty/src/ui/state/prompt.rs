use std::path::PathBuf;

use crate::domain::agent::AgentKind;
use crate::infra::file_index::FileEntry;

/// Inline attachment metadata for one pasted local image placeholder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAttachment {
    /// Stable display number shown inside the inline `[Image #n]` token.
    pub attachment_number: usize,
    /// Local image path that will later be handed off to runtime transport.
    pub local_image_path: PathBuf,
    /// Placeholder token inserted into the prompt composer text.
    pub placeholder: String,
}

impl PromptAttachment {
    /// Creates attachment metadata for one pasted local image.
    #[must_use]
    pub fn new(attachment_number: usize, local_image_path: PathBuf) -> Self {
        Self {
            attachment_number,
            local_image_path,
            placeholder: Self::placeholder_for(attachment_number),
        }
    }

    /// Builds the inline placeholder token for one attachment number.
    #[must_use]
    pub fn placeholder_for(attachment_number: usize) -> String {
        format!("[Image #{attachment_number}]")
    }
}

/// UI state for pasted local-image attachments in prompt mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAttachmentState {
    /// Attachments in the same order their placeholders were inserted.
    pub attachments: Vec<PromptAttachment>,
    /// Next placeholder number that should be assigned to a pasted image.
    pub next_attachment_number: usize,
}

impl PromptAttachmentState {
    /// Creates empty prompt attachment state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            attachments: Vec::new(),
            next_attachment_number: 1,
        }
    }

    /// Registers a pasted local image and returns the placeholder inserted
    /// into the prompt input text.
    pub fn register_local_image(&mut self, local_image_path: PathBuf) -> String {
        let attachment = PromptAttachment::new(self.next_attachment_number, local_image_path);
        let placeholder = attachment.placeholder.clone();

        self.attachments.push(attachment);
        self.refresh_next_attachment_number();

        placeholder
    }

    /// Returns attachment metadata for the given inline placeholder token.
    #[must_use]
    pub fn attachment_for_placeholder(&self, placeholder: &str) -> Option<&PromptAttachment> {
        self.attachments
            .iter()
            .find(|attachment| attachment.placeholder == placeholder)
    }

    /// Recomputes the next placeholder number by reusing the smallest missing
    /// positive attachment number.
    pub fn refresh_next_attachment_number(&mut self) {
        let mut next_attachment_number = 1;

        while self
            .attachments
            .iter()
            .any(|attachment| attachment.attachment_number == next_attachment_number)
        {
            next_attachment_number += 1;
        }

        self.next_attachment_number = next_attachment_number;
    }

    /// Clears all tracked attachments and resets numbering back to the first
    /// placeholder.
    pub fn reset(&mut self) {
        self.attachments.clear();
        self.next_attachment_number = 1;
    }
}

impl Default for PromptAttachmentState {
    fn default() -> Self {
        Self::new()
    }
}

/// UI state for prompt `@` file and directory mention selection.
#[derive(Clone, Debug)]
pub struct PromptAtMentionState {
    /// Cached list of all files and directories in the session directory.
    pub all_entries: Vec<FileEntry>,
    /// Currently selected index in the filtered dropdown.
    pub selected_index: usize,
}

impl PromptAtMentionState {
    /// Creates a new at-mention state with the given file entries.
    pub fn new(all_entries: Vec<FileEntry>) -> Self {
        Self {
            all_entries,
            selected_index: 0,
        }
    }
}

/// UI state for navigating previously sent prompts with `Up` and `Down`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptHistoryState {
    /// Draft input captured before entering history navigation.
    pub draft_text: Option<String>,
    /// Previously sent user prompts in chronological order.
    pub entries: Vec<String>,
    /// Currently selected history entry index, if any.
    pub selected_index: Option<usize>,
}

impl PromptHistoryState {
    /// Creates history state from prior prompt entries.
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            draft_text: None,
            entries,
            selected_index: None,
        }
    }

    /// Clears active history navigation and stored draft text.
    pub fn reset_navigation(&mut self) {
        self.draft_text = None;
        self.selected_index = None;
    }
}

/// Steps in prompt slash command selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptSlashStage {
    /// Selecting the agent for the current slash command.
    Agent,
    /// Selecting the slash command itself.
    Command,
    /// Selecting a model after choosing an agent.
    Model,
}

/// UI state for prompt-only slash command selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PromptSlashState {
    /// Agent selected for the current slash workflow, when applicable.
    pub selected_agent: Option<AgentKind>,
    /// Highlighted option inside the active slash menu.
    pub selected_index: usize,
    /// Active slash-command selection stage.
    pub stage: PromptSlashStage,
}

impl PromptSlashState {
    /// Creates a new slash state at command selection.
    pub fn new() -> Self {
        Self {
            selected_agent: None,
            selected_index: 0,
            stage: PromptSlashStage::Command,
        }
    }

    /// Resets slash state back to command selection.
    pub fn reset(&mut self) {
        self.selected_agent = None;
        self.selected_index = 0;
        self.stage = PromptSlashStage::Command;
    }
}

impl Default for PromptSlashState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_prompt_attachment_state_registers_images_in_placeholder_order() {
        // Arrange
        let mut attachment_state = PromptAttachmentState::new();

        // Act
        let first_placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"));
        let second_placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"));

        // Assert
        assert_eq!(first_placeholder, "[Image #1]");
        assert_eq!(second_placeholder, "[Image #2]");
        assert_eq!(attachment_state.attachments.len(), 2);
        assert_eq!(
            attachment_state.attachment_for_placeholder("[Image #2]"),
            Some(&PromptAttachment {
                attachment_number: 2,
                local_image_path: PathBuf::from("/tmp/second-image.png"),
                placeholder: "[Image #2]".to_string(),
            })
        );
    }

    #[test]
    fn test_prompt_attachment_state_reset_clears_attachments_and_restarts_numbering() {
        // Arrange
        let mut attachment_state = PromptAttachmentState::new();
        let _ = attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"));

        // Act
        attachment_state.reset();
        let placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/reused-number.png"));

        // Assert
        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(attachment_state.next_attachment_number, 2);
        assert_eq!(placeholder, "[Image #1]");
        assert_eq!(
            attachment_state.attachment_for_placeholder("[Image #1]"),
            Some(&PromptAttachment {
                attachment_number: 1,
                local_image_path: PathBuf::from("/tmp/reused-number.png"),
                placeholder: "[Image #1]".to_string(),
            })
        );
    }

    #[test]
    fn test_prompt_attachment_state_refresh_next_attachment_number_reuses_deleted_gap() {
        // Arrange
        let mut attachment_state = PromptAttachmentState::new();
        let _ = attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"));
        let _ = attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"));
        let _ = attachment_state.register_local_image(PathBuf::from("/tmp/third-image.png"));
        attachment_state
            .attachments
            .retain(|attachment| attachment.placeholder != "[Image #2]");

        // Act
        attachment_state.refresh_next_attachment_number();
        let placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/reused-gap-image.png"));

        // Assert
        assert_eq!(attachment_state.next_attachment_number, 4);
        assert_eq!(placeholder, "[Image #2]");
        assert_eq!(
            attachment_state.attachment_for_placeholder("[Image #2]"),
            Some(&PromptAttachment {
                attachment_number: 2,
                local_image_path: PathBuf::from("/tmp/reused-gap-image.png"),
                placeholder: "[Image #2]".to_string(),
            })
        );
    }
}
