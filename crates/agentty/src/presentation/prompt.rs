use std::cell::RefCell;
use std::sync::Arc;

pub use crate::domain::composer::{
    PromptAttachment, PromptAttachmentState, PromptComposerState, PromptComposerSubmission,
    PromptHistoryState, PromptSlashStage, PromptSlashState, PromptSuggestionItem,
    PromptSuggestionList, PromptSuggestionSelection, apply_prompt_delete_range,
    build_prompt_slash_suggestion_list, current_line_delete_range, drain_prompt_submission,
    expand_delete_range_to_image_tokens, image_token_ranges, insert_prompt_character,
    insert_prompt_local_image, insert_prompt_text, prompt_slash_option_count,
    resolve_prompt_slash_selection,
};
use crate::domain::file_entry::{self, FileEntry};

/// UI state for prompt `@` file and directory mention selection.
#[derive(Clone, Debug)]
pub struct PromptAtMentionState {
    /// Currently selected index in the filtered dropdown.
    pub selected_index: usize,
    all_entries: Vec<FileEntry>,
    /// One ranked query shared by input handling and painting. Replacing the
    /// index or changing the query invalidates it; selection does not.
    ranked_entries: RefCell<Option<(String, Arc<[FileEntry]>)>>,
}

impl PromptAtMentionState {
    /// Creates a new at-mention state with the given file entries.
    #[must_use]
    pub fn new(all_entries: Vec<FileEntry>) -> Self {
        Self {
            all_entries,
            selected_index: 0,
            ranked_entries: RefCell::new(None),
        }
    }

    /// Replaces the file index and invalidates selection and ranked results.
    pub fn replace_entries(&mut self, entries: Vec<FileEntry>) {
        self.all_entries = entries;
        self.selected_index = 0;
        *self.ranked_entries.get_mut() = None;
    }

    /// Returns shared ranked results without repeating search on redraws.
    #[must_use]
    pub fn filtered_entries(&self, query: &str) -> Arc<[FileEntry]> {
        let mut cached = self.ranked_entries.borrow_mut();
        if let Some((cached_query, entries)) = cached.as_ref()
            && cached_query == query
        {
            return Arc::clone(entries);
        }

        let entries: Arc<[FileEntry]> = file_entry::filter_entries(&self.all_entries, query)
            .into_iter()
            .cloned()
            .collect();
        *cached = Some((query.to_string(), Arc::clone(&entries)));

        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_results_survive_selection_and_invalidate_on_query_or_index_change() {
        // Arrange
        let mut state = PromptAtMentionState::new(vec![
            FileEntry {
                is_dir: false,
                path: "src/main.rs".into(),
            },
            FileEntry {
                is_dir: false,
                path: "README.md".into(),
            },
        ]);

        // Act
        let initial = state.filtered_entries("main");
        state.selected_index = 1;
        let redraw = state.filtered_entries("main");
        let changed_query = state.filtered_entries("README");
        state.replace_entries(vec![FileEntry {
            is_dir: false,
            path: "main.txt".into(),
        }]);
        let changed_index = state.filtered_entries("main");

        // Assert
        assert!(Arc::ptr_eq(&initial, &redraw));
        assert_eq!(initial[0].path, "src/main.rs");
        assert_eq!(changed_query[0].path, "README.md");
        assert_eq!(changed_index[0].path, "main.txt");
        assert_eq!(state.selected_index, 0);
        assert!(state.filtered_entries("missing").is_empty());
        assert_eq!(state.filtered_entries("").len(), 1);
    }
}
