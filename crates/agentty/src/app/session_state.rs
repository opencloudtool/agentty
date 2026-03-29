use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use ratatui::widgets::TableState;

use crate::app::session::{Clock, SESSION_REFRESH_INTERVAL};
use crate::domain::session::{Session, SessionHandles, SessionSize};

/// Cached ahead/behind snapshots for one session branch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionGitStatus {
    /// Ahead/behind counts comparing the session branch to its base branch.
    pub base_status: Option<(u32, u32)>,
    /// Ahead/behind counts comparing the session branch to its tracked remote.
    pub remote_status: Option<(u32, u32)>,
}

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    /// Selected follow-up-task positions keyed by session id for session-view
    /// affordances.
    pub(crate) follow_up_task_positions: HashMap<String, usize>,
    pub handles: HashMap<String, SessionHandles>,
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) refresh_deadline: Instant,
    pub(crate) row_count: i64,
    pub(crate) session_git_statuses: HashMap<String, SessionGitStatus>,
    pub(crate) updated_at_max: i64,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    ///
    /// Time values are provided by an injected clock so refresh scheduling can
    /// be deterministic in tests.
    pub(crate) fn new(
        handles: HashMap<String, SessionHandles>,
        sessions: Vec<Session>,
        table_state: TableState,
        clock: Arc<dyn Clock>,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        let _state_created_at = clock.now_system_time();
        let refresh_deadline = clock.now_instant() + SESSION_REFRESH_INTERVAL;

        Self {
            follow_up_task_positions: HashMap::new(),
            handles,
            sessions,
            table_state,
            clock,
            refresh_deadline,
            row_count,
            session_git_statuses: HashMap::new(),
            updated_at_max,
        }
    }

    /// Copies current values from one runtime handle into its `Session`
    /// snapshot.
    pub fn sync_session_from_handle(&mut self, session_id: &str) {
        let Some(session_handles) = self.handles.get(session_id) else {
            return;
        };
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        Self::sync_session_with_handles(session, session_handles);
    }

    /// Copies current values from runtime handles into plain `Session` fields.
    pub fn sync_from_handles(&mut self) {
        let handles = &self.handles;

        for session in &mut self.sessions {
            let Some(session_handles) = handles.get(&session.id) else {
                continue;
            };

            Self::sync_session_with_handles(session, session_handles);
        }
    }

    /// Applies one recomputed diff summary to the matching snapshot.
    pub fn apply_session_size_updated(
        &mut self,
        session_id: &str,
        added_lines: u64,
        deleted_lines: u64,
        session_size: SessionSize,
    ) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        session.stats.added_lines = added_lines;
        session.stats.deleted_lines = deleted_lines;
        session.size = session_size;
    }

    /// Returns the currently selected follow-up task position for one session.
    pub fn selected_follow_up_task_position(&self, session_id: &str) -> Option<usize> {
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)?;
        if session.follow_up_tasks.is_empty() {
            return None;
        }

        let selected_position = self
            .follow_up_task_positions
            .get(session_id)
            .copied()
            .unwrap_or(0);

        Some(selected_position.min(session.follow_up_tasks.len().saturating_sub(1)))
    }

    /// Advances the selected follow-up task for one session to the next item,
    /// wrapping at the end of the task list.
    pub fn select_next_follow_up_task(&mut self, session_id: &str) {
        self.advance_follow_up_task_selection(session_id, true);
    }

    /// Moves the selected follow-up task for one session to the previous item,
    /// wrapping at the beginning of the task list.
    pub fn select_previous_follow_up_task(&mut self, session_id: &str) {
        self.advance_follow_up_task_selection(session_id, false);
    }

    /// Sets the launched sibling-session identifier for the matching cached
    /// follow-up task.
    pub fn set_follow_up_task_launched_session_id(
        &mut self,
        session_id: &str,
        position: usize,
        launched_session_id: Option<String>,
    ) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        let Some(follow_up_task) = session
            .follow_up_tasks
            .iter_mut()
            .find(|task| task.position == position)
        else {
            return;
        };

        follow_up_task.launched_session_id = launched_session_id;
    }

    /// Replaces all cached session git-status snapshots with one fresh poll
    /// result.
    pub(crate) fn replace_session_git_statuses(
        &mut self,
        session_git_statuses: HashMap<String, SessionGitStatus>,
    ) {
        self.session_git_statuses = session_git_statuses;
    }

    /// Drops cached git-status entries for sessions that are no longer active
    /// in memory.
    pub(crate) fn retain_session_git_statuses(&mut self, active_session_ids: &HashSet<String>) {
        self.session_git_statuses
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Drops or clamps cached follow-up-task selection for sessions that no
    /// longer exist after a reload.
    pub(crate) fn retain_follow_up_task_positions(&mut self, active_session_ids: &HashSet<String>) {
        let follow_up_task_counts = self
            .sessions
            .iter()
            .map(|session| (session.id.as_str(), session.follow_up_tasks.len()))
            .collect::<HashMap<_, _>>();

        self.follow_up_task_positions
            .retain(|session_id, position| {
                if !active_session_ids.contains(session_id) {
                    return false;
                }

                let Some(follow_up_task_count) =
                    follow_up_task_counts.get(session_id.as_str()).copied()
                else {
                    return false;
                };
                if follow_up_task_count == 0 {
                    return false;
                }

                *position = (*position).min(follow_up_task_count.saturating_sub(1));

                true
            });
    }

    /// Synchronizes one session snapshot from shared runtime handles.
    ///
    /// Output synchronization prefers an append-only fast path: when runtime
    /// output extends the existing snapshot with the same prefix, only the
    /// newly appended suffix is copied. Equal-length rewrites and non-prefix
    /// changes still fall back to full replacement so edits are never missed.
    fn sync_session_with_handles(session: &mut Session, session_handles: &SessionHandles) {
        if let Ok(output) = session_handles.output.lock() {
            Self::sync_session_output(&mut session.output, output.as_str());
        }

        if let Ok(status) = session_handles.status.lock() {
            session.status = *status;
        }
    }

    /// Synchronizes one output snapshot from its shared runtime output buffer.
    ///
    /// This keeps equal-length rewrites correct while reducing copy cost for
    /// append-heavy streams by pushing only the unseen suffix.
    fn sync_session_output(session_output: &mut String, handle_output: &str) {
        let session_output_len = session_output.len();
        let handle_output_len = handle_output.len();

        if session_output_len == handle_output_len {
            if session_output != handle_output {
                session_output.clear();
                session_output.push_str(handle_output);
            }
        } else if handle_output_len > session_output_len
            && handle_output.starts_with(session_output.as_str())
            && let Some(appended_output) = handle_output.get(session_output_len..)
        {
            session_output.push_str(appended_output);
        } else {
            session_output.clear();
            session_output.push_str(handle_output);
        }
    }

    /// Advances the selected follow-up task for one session in the requested
    /// direction when at least one task exists.
    fn advance_follow_up_task_selection(&mut self, session_id: &str, move_forward: bool) {
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        let follow_up_task_count = session.follow_up_tasks.len();
        if follow_up_task_count <= 1 {
            if follow_up_task_count == 1 {
                self.follow_up_task_positions
                    .insert(session_id.to_string(), 0);
            }

            return;
        }

        let next_position = match self.selected_follow_up_task_position(session_id) {
            Some(current_position) if move_forward => (current_position + 1) % follow_up_task_count,
            Some(0) => follow_up_task_count.saturating_sub(1),
            Some(current_position) => current_position.saturating_sub(1),
            None => 0,
        };
        self.follow_up_task_positions
            .insert(session_id.to_string(), next_position);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use ratatui::widgets::TableState;

    use super::*;
    use crate::domain::agent::AgentKind;
    use crate::domain::session::{Session, SessionHandles, SessionSize, SessionStats, Status};

    struct FixedClock {
        instant: Instant,
        system_time: SystemTime,
    }

    impl FixedClock {
        fn new() -> Self {
            Self {
                instant: Instant::now(),
                system_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            }
        }
    }

    impl Clock for FixedClock {
        fn now_instant(&self) -> Instant {
            self.instant
        }

        fn now_system_time(&self) -> SystemTime {
            self.system_time
        }
    }

    #[test]
    /// Verifies handle output replaces session output even when lengths match.
    fn sync_from_handles_updates_output_when_same_length_content_changes() {
        // Arrange
        let session_id = "sess-1".to_string();
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            follow_up_tasks: Vec::new(),
            id: session_id.clone(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "old".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles = HashMap::from([(
            session_id,
            SessionHandles::new("new".to_string(), Status::Review),
        )]);
        let mut state = SessionState::new(
            handles,
            vec![session],
            TableState::default(),
            Arc::new(FixedClock::new()),
            0,
            0,
        );

        // Act
        state.sync_from_handles();

        // Assert
        assert_eq!(state.sessions[0].output, "new");
        assert_eq!(state.sessions[0].status, Status::Review);
    }

    #[test]
    /// Verifies direct single-session sync updates output and status together.
    fn sync_session_with_handles_equal_length_sync() {
        // Arrange
        let mut session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            follow_up_tasks: Vec::new(),
            id: "session-2".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "Old".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::New,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles = SessionHandles::new("New".to_string(), Status::InProgress);

        // Act
        SessionState::sync_session_with_handles(&mut session, &handles);

        // Assert
        assert_eq!(session.output, "New");
        assert_eq!(session.status, Status::InProgress);
    }

    #[test]
    /// Verifies append-only output sync copies only the new suffix.
    fn sync_session_with_handles_appends_suffix_for_extended_output() {
        // Arrange
        let mut session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            follow_up_tasks: Vec::new(),
            id: "session-3".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "first line\n".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles =
            SessionHandles::new("first line\nsecond line\n".to_string(), Status::InProgress);

        // Act
        SessionState::sync_session_with_handles(&mut session, &handles);

        // Assert
        assert_eq!(session.output, "first line\nsecond line\n");
        assert_eq!(session.status, Status::InProgress);
    }

    #[test]
    /// Verifies explicit size updates patch the in-memory session snapshot.
    fn apply_session_size_updated_updates_matching_session() {
        // Arrange
        let session_id = "session-3".to_string();
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            follow_up_tasks: Vec::new(),
            id: session_id.clone(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let mut state = SessionState::new(
            HashMap::new(),
            vec![session],
            TableState::default(),
            Arc::new(FixedClock::new()),
            0,
            0,
        );

        // Act
        state.apply_session_size_updated(&session_id, 12, 4, SessionSize::S);

        // Assert
        assert_eq!(state.sessions[0].stats.added_lines, 12);
        assert_eq!(state.sessions[0].stats.deleted_lines, 4);
        assert_eq!(state.sessions[0].size, SessionSize::S);
    }

    #[test]
    /// Verifies non-prefix changes still use full replacement when lengths
    /// differ.
    fn sync_session_with_handles_replaces_output_when_prefix_changes() {
        // Arrange
        let mut session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            follow_up_tasks: Vec::new(),
            id: "session-4".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "abc".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let handles = SessionHandles::new("xyzq".to_string(), Status::Review);

        // Act
        SessionState::sync_session_with_handles(&mut session, &handles);

        // Assert
        assert_eq!(session.output, "xyzq");
        assert_eq!(session.status, Status::Review);
    }

    #[test]
    /// Verifies session git-status caching keeps only entries for active
    /// sessions after refresh.
    fn retain_session_git_statuses_drops_removed_sessions() {
        // Arrange
        let mut state = SessionState::new(
            HashMap::new(),
            Vec::new(),
            TableState::default(),
            Arc::new(FixedClock::new()),
            0,
            0,
        );
        state.replace_session_git_statuses(HashMap::from([
            (
                "session-1".to_string(),
                SessionGitStatus {
                    base_status: Some((1, 0)),
                    remote_status: Some((0, 1)),
                },
            ),
            (
                "session-2".to_string(),
                SessionGitStatus {
                    base_status: Some((0, 2)),
                    remote_status: None,
                },
            ),
        ]));
        let active_session_ids = HashSet::from(["session-2".to_string()]);

        // Act
        state.retain_session_git_statuses(&active_session_ids);

        // Assert
        assert_eq!(state.session_git_statuses.get("session-1"), None);
        assert_eq!(
            state.session_git_statuses.get("session-2"),
            Some(&SessionGitStatus {
                base_status: Some((0, 2)),
                remote_status: None,
            })
        );
    }

    #[test]
    /// Verifies cached follow-up-task selections are clamped for surviving
    /// sessions and dropped for removed or taskless sessions in one refresh
    /// pass.
    fn retain_follow_up_task_positions_clamps_and_drops_invalid_entries() {
        // Arrange
        let mut surviving_session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            follow_up_tasks: vec![crate::domain::session::SessionFollowUpTask {
                id: 1,
                launched_session_id: None,
                position: 0,
                text: "Document the behavior.".to_string(),
            }],
            id: "session-1".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Done,
            summary: None,
            title: None,
            updated_at: 0,
        };
        surviving_session
            .follow_up_tasks
            .push(crate::domain::session::SessionFollowUpTask {
                id: 2,
                launched_session_id: None,
                position: 1,
                text: "Add the regression test.".to_string(),
            });
        let taskless_session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            follow_up_tasks: Vec::new(),
            id: "session-2".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Done,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let mut state = SessionState::new(
            HashMap::new(),
            vec![surviving_session, taskless_session],
            TableState::default(),
            Arc::new(FixedClock::new()),
            0,
            0,
        );
        state
            .follow_up_task_positions
            .insert("session-1".to_string(), 9);
        state
            .follow_up_task_positions
            .insert("session-2".to_string(), 4);
        state
            .follow_up_task_positions
            .insert("session-3".to_string(), 2);
        let active_session_ids = HashSet::from(["session-1".to_string(), "session-2".to_string()]);

        // Act
        state.retain_follow_up_task_positions(&active_session_ids);

        // Assert
        assert_eq!(state.follow_up_task_positions.get("session-1"), Some(&1));
        assert_eq!(state.follow_up_task_positions.get("session-2"), None);
        assert_eq!(state.follow_up_task_positions.get("session-3"), None);
    }
}
