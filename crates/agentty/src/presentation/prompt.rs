pub use crate::domain::composer::{
    PromptAttachment, PromptAttachmentState, PromptComposerState, PromptComposerSubmission,
    PromptHistoryState, PromptSlashStage, PromptSlashState, PromptSuggestionItem,
    PromptSuggestionList, PromptSuggestionSelection, apply_prompt_delete_range,
    build_prompt_slash_suggestion_list, current_line_delete_range, drain_prompt_submission,
    expand_delete_range_to_image_tokens, image_token_ranges, insert_prompt_character,
    insert_prompt_local_image, insert_prompt_text, prompt_slash_option_count,
    resolve_prompt_slash_selection,
};
use crate::domain::file_entry::FileEntry;

/// UI state for prompt `@` file and directory mention selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAtMentionState {
    /// Cached list of all files and directories in the session directory.
    pub all_entries: Vec<FileEntry>,
    /// Currently selected index in the filtered dropdown.
    pub selected_index: usize,
}

impl PromptAtMentionState {
    /// Creates a new at-mention state with the given file entries.
    #[must_use]
    pub fn new(all_entries: Vec<FileEntry>) -> Self {
        Self {
            all_entries,
            selected_index: 0,
        }
    }
}
