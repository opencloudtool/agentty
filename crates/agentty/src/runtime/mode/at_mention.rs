use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::domain::input::InputState;
use crate::infra::file_index::{self, FileEntry};
use crate::ui::state::prompt::PromptAtMentionState;

/// Describes how one mode should update its visible `@`-mention state after an
/// input edit or cursor move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtMentionSyncAction {
    /// Open the dropdown and start loading entries.
    Activate,
    /// Hide the dropdown because the cursor no longer sits inside an `@` token.
    Dismiss,
    /// Keep the dropdown open and reset its selected row.
    KeepOpen,
}

/// Text replacement derived from the currently highlighted `@`-mention row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AtMentionSelection {
    /// End character index of the active `@query`.
    pub cursor: usize,
    /// Replacement text inserted into the input.
    pub text: String,
    /// Start character index of the active `@query`.
    pub at_start: usize,
}

/// Returns the next `@`-mention sync action for one input buffer and dropdown
/// state pair.
pub(crate) fn sync_action(
    input: &InputState,
    at_mention_state: Option<&PromptAtMentionState>,
) -> AtMentionSyncAction {
    match (
        input.at_mention_query().is_some(),
        at_mention_state.is_some(),
    ) {
        (true, true) => AtMentionSyncAction::KeepOpen,
        (true, false) => AtMentionSyncAction::Activate,
        (false, _) => AtMentionSyncAction::Dismiss,
    }
}

/// Starts asynchronous loading of `@`-mention entries for one session.
pub(crate) fn start_loading_entries(
    event_tx: mpsc::UnboundedSender<AppEvent>,
    session_folder: PathBuf,
    session_id: String,
) {
    tokio::spawn(async move {
        let entries = tokio::task::spawn_blocking(move || file_index::list_files(&session_folder))
            .await
            .unwrap_or_default();

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = event_tx.send(AppEvent::AtMentionEntriesLoaded {
            entries,
            session_id,
        });
    });
}

/// Clears one visible `@`-mention dropdown state.
pub(crate) fn dismiss(at_mention_state: &mut Option<PromptAtMentionState>) {
    *at_mention_state = None;
}

/// Resets the highlighted `@`-mention row to the first visible entry.
pub(crate) fn reset_selection(at_mention_state: &mut PromptAtMentionState) {
    at_mention_state.selected_index = 0;
}

/// Moves the highlighted `@`-mention row up by one item.
pub(crate) fn move_selection_up(at_mention_state: &mut PromptAtMentionState) {
    at_mention_state.selected_index = at_mention_state.selected_index.saturating_sub(1);
}

/// Moves the highlighted `@`-mention row down by one filtered item.
pub(crate) fn move_selection_down(input: &InputState, at_mention_state: &mut PromptAtMentionState) {
    let filtered_count =
        filtered_entries(input, at_mention_state).map_or(0_usize, |entries| entries.len());
    let max_index = filtered_count.saturating_sub(1);

    at_mention_state.selected_index = (at_mention_state.selected_index + 1).min(max_index);
}

/// Returns the replacement text for the highlighted `@`-mention entry, if the
/// input still contains an active `@query`.
pub(crate) fn selected_replacement(
    input: &InputState,
    at_mention_state: &PromptAtMentionState,
) -> Option<AtMentionSelection> {
    let (at_start, query) = input.at_mention_query()?;
    let filtered = file_index::filter_entries(&at_mention_state.all_entries, &query);
    let clamped_index = at_mention_state
        .selected_index
        .min(filtered.len().saturating_sub(1));

    filtered.get(clamped_index).map(|entry| AtMentionSelection {
        at_start,
        cursor: input.cursor,
        text: format_mention_text(entry),
    })
}

/// Returns the filtered `@`-mention entries for the current input query.
fn filtered_entries<'a>(
    input: &InputState,
    at_mention_state: &'a PromptAtMentionState,
) -> Option<Vec<&'a FileEntry>> {
    let (_, query) = input.at_mention_query()?;

    Some(file_index::filter_entries(
        &at_mention_state.all_entries,
        &query,
    ))
}

/// Formats one selected file or directory entry for insertion into the input.
fn format_mention_text(entry: &FileEntry) -> String {
    if entry.is_dir {
        return format!("@{}/ ", entry.path);
    }

    format!("@{} ", entry.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_action_requests_activation_for_new_query() {
        // Arrange
        let input = InputState::with_text("@src".to_string());

        // Act
        let action = sync_action(&input, None);

        // Assert
        assert_eq!(action, AtMentionSyncAction::Activate);
    }

    #[test]
    fn test_move_selection_down_clamps_to_last_filtered_entry() {
        // Arrange
        let input = InputState::with_text("@src".to_string());
        let mut at_mention_state = PromptAtMentionState::new(vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
        ]);
        at_mention_state.selected_index = 99;

        // Act
        move_selection_down(&input, &mut at_mention_state);

        // Assert
        assert_eq!(at_mention_state.selected_index, 1);
    }

    #[test]
    fn test_selected_replacement_formats_directory_with_trailing_slash() {
        // Arrange
        let input = InputState::with_text("@src".to_string());
        let at_mention_state = PromptAtMentionState::new(vec![FileEntry {
            is_dir: true,
            path: "src".to_string(),
        }]);

        // Act
        let selection =
            selected_replacement(&input, &at_mention_state).expect("expected directory selection");

        // Assert
        assert_eq!(
            selection,
            AtMentionSelection {
                at_start: 0,
                cursor: 4,
                text: "@src/ ".to_string(),
            }
        );
    }
}
