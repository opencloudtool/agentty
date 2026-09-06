use std::path::PathBuf;
use std::sync::Arc;

#[cfg(test)]
use at_mention_task::clear_pending_load;
use tokio::sync::mpsc;

#[cfg(test)]
use crate::app::at_mention_task;
use crate::app::session::SessionManager;
use crate::app::{AppEvent, TaskService};
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputState;
use crate::domain::session::SessionId;
use crate::presentation::prompt::PromptAtMentionState;

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

/// Starts asynchronous loading of `@`-mention entries for one composer root.
///
/// When a fresh cache entry already exists for `lookup_root`, this emits the
/// loaded event immediately and skips the debounced filesystem walk.
pub(crate) fn start_loading_entries(
    event_tx: mpsc::UnboundedSender<AppEvent>,
    lookup_root: PathBuf,
    session_id: SessionId,
    session_manager: &mut SessionManager,
) {
    let cached_entries = session_manager.at_mention_index_for_root(&lookup_root);

    TaskService::spawn_at_mention_entries_task(event_tx, cached_entries, lookup_root, session_id);
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
    let filtered = at_mention_state.filtered_entries(&query);
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
fn filtered_entries(
    input: &InputState,
    at_mention_state: &PromptAtMentionState,
) -> Option<Arc<[FileEntry]>> {
    let (_, query) = input.at_mention_query()?;

    Some(at_mention_state.filtered_entries(&query))
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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use ag_git as git;
    use tempfile::TempDir;

    use super::*;
    use crate::app::SessionState;
    use crate::app::session::SessionDefaults;
    use crate::domain::agent::AgentModel;
    use crate::domain::selection::SelectionState;
    use crate::domain::session::{
        Session, SessionHandles, SessionRole, SessionSize, SessionStats, Status,
    };
    use crate::domain::transient_message::TransientMessageStore;
    use crate::infra::clock::RealClock;

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

    #[tokio::test]
    async fn test_start_loading_entries_aborts_stale_debounced_loads() {
        // Arrange
        let temp_dir = TempDir::new().expect("create temp dir");
        let event_session_id = "session-1".to_string();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut initial_session_manager = test_session_manager(&event_session_id);
        std::fs::write(temp_dir.path().join("first.txt"), "").expect("write first file");

        start_loading_entries(
            event_tx.clone(),
            temp_dir.path().to_path_buf(),
            event_session_id.clone().into(),
            &mut initial_session_manager,
        );

        std::fs::write(temp_dir.path().join("second.txt"), "").expect("write second file");

        // Act
        let mut session_manager = test_session_manager(&event_session_id);

        start_loading_entries(
            event_tx,
            temp_dir.path().to_path_buf(),
            event_session_id.clone().into(),
            &mut session_manager,
        );
        let next_event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("at-mention event should arrive")
            .expect("event channel should stay open");

        // Assert
        match next_event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => {
                assert_eq!(session_id, event_session_id);
                assert!(entries.iter().any(|entry| entry.path == "second.txt"));
            }
            _ => unreachable!("expected at-mention entries event"),
        }

        let extra_event = tokio::time::timeout(Duration::from_millis(250), event_rx.recv()).await;
        assert!(!matches!(
            extra_event,
            Ok(Some(AppEvent::AtMentionEntriesLoaded { .. }))
        ));
    }

    #[tokio::test]
    async fn test_clear_pending_load_aborts_pending_task_for_session() {
        // Arrange
        let temp_dir = TempDir::new().expect("create temp dir");
        let session_id = "session-1".to_string();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let mut session_manager = test_session_manager(&session_id);

        start_loading_entries(
            event_tx,
            temp_dir.path().to_path_buf(),
            session_id.clone().into(),
            &mut session_manager,
        );

        // Act
        clear_pending_load(&session_id);

        // Assert
        let next_event = tokio::time::timeout(Duration::from_millis(250), event_rx.recv()).await;
        assert!(!matches!(
            next_event,
            Ok(Some(AppEvent::AtMentionEntriesLoaded { .. }))
        ));
    }

    #[tokio::test]
    async fn test_start_loading_entries_uses_cached_index_without_debounced_walk() {
        // Arrange
        let temp_dir = TempDir::new().expect("create temp dir");
        let lookup_root = temp_dir.path().to_path_buf();
        let session_id = "session-1".to_string();
        let cached_entries = vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }];
        let mut session_manager = test_session_manager(&session_id);
        session_manager.set_at_mention_index_for_root(lookup_root.clone(), cached_entries.clone());
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        // Act
        start_loading_entries(
            event_tx,
            lookup_root,
            session_id.clone().into(),
            &mut session_manager,
        );
        let next_event = tokio::time::timeout(Duration::from_millis(25), event_rx.recv())
            .await
            .expect("cached at-mention event should arrive immediately")
            .expect("event channel should stay open");

        // Assert
        assert_eq!(
            next_event,
            AppEvent::AtMentionEntriesLoaded {
                entries: cached_entries,
                session_id: session_id.into(),
            }
        );

        let extra_event = tokio::time::timeout(Duration::from_millis(125), event_rx.recv()).await;
        assert!(
            !matches!(
                extra_event,
                Ok(Some(AppEvent::AtMentionEntriesLoaded { .. }))
            ),
            "cache hits should not schedule a debounced filesystem walk"
        );
    }

    /// Builds a minimal `SessionManager` for cache tests.
    fn test_session_manager(session_id: &str) -> SessionManager {
        let mut handles = HashMap::new();
        handles.insert(
            session_id.to_string().into(),
            SessionHandles::new(Status::Review),
        );

        let state = SessionState::new(
            handles,
            vec![Session {
                base_branch: "main".to_string(),
                created_at: 0,
                draft_attachments: Vec::new(),
                folder: PathBuf::from(format!("/tmp/{session_id}")),
                follow_up_tasks: Vec::new(),
                id: session_id.into(),
                in_progress_started_at: None,
                in_progress_total_seconds: 0,
                is_draft: false,
                controller_session_id: None,
                orchestration_progress: None,
                role: SessionRole::default(),
                agent: crate::domain::agent::AgentSelection::new(
                    crate::domain::agent::AgentKind::Codex,
                    AgentModel::Gpt56Sol,
                ),
                parent_session_id: None,
                permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
                personality_id: None,
                project_name: "project".to_string(),
                prompt: String::new(),
                queued_messages: Vec::new(),
                reasoning_level_override: None,
                response_style: crate::domain::agent::ResponseStyle::default(),
                published_upstream_ref: None,
                questions: Vec::new(),
                review_request: None,
                size: SessionSize::Xs,
                speed_mode: crate::domain::agent::SpeedMode::default(),
                stats: SessionStats::default(),
                status: Status::Review,
                title: Some("Title".to_string()),
                transcript: None,
                updated_at: 0,
                transient_messages: TransientMessageStore::default(),
            }],
            SelectionState::default(),
            Arc::new(RealClock),
            1,
            0,
        );

        SessionManager::new(
            SessionDefaults {
                model: AgentModel::Gpt56Sol,
            },
            Arc::new(git::MockGitClient::new()),
            state,
            Vec::new(),
        )
    }
}
