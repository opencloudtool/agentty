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
    pub handles: HashMap<String, SessionHandles>,
    /// Cached detected branch names keyed by session id.
    pub(crate) session_branch_names: HashMap<String, String>,
    /// Cached worktree-directory availability keyed by session id.
    pub(crate) session_worktree_availability: HashMap<String, bool>,
    /// Cached session list positions keyed by stable session id.
    pub(crate) session_index_by_id: HashMap<String, usize>,
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
        let mut state = Self {
            handles,
            session_branch_names: HashMap::new(),
            session_worktree_availability: HashMap::new(),
            session_index_by_id: HashMap::new(),
            sessions: Vec::new(),
            table_state,
            clock,
            refresh_deadline,
            row_count,
            session_git_statuses: HashMap::new(),
            updated_at_max,
        };

        for session in sessions {
            state.push_session(session);
        }

        state
    }

    /// Returns the current list position for one stable session identifier.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.session_index_by_id.get(session_id).copied()
    }

    /// Returns one immutable session snapshot by identifier.
    pub fn session_for_id(&self, session_id: &str) -> Option<&Session> {
        self.session_index_for_id(session_id)
            .and_then(|session_index| self.sessions.get(session_index))
    }

    /// Returns the cached session-id lookup map used by render and workflow
    /// code.
    pub(crate) fn session_index_by_id(&self) -> &HashMap<String, usize> {
        &self.session_index_by_id
    }

    /// Replaces the full session snapshot list and rebuilds the cached
    /// identifier index in one step.
    pub(crate) fn replace_sessions(&mut self, sessions: Vec<Session>) {
        self.session_index_by_id = Self::build_session_index_by_id(&sessions);
        self.sessions = sessions;
    }

    /// Appends one session snapshot and records its new stable identifier
    /// lookup entry.
    pub(crate) fn push_session(&mut self, session: Session) {
        let session_index = self.sessions.len();
        self.session_index_by_id
            .insert(session.id.clone(), session_index);
        self.sessions.push(session);
    }

    /// Removes one session snapshot by list index and rebuilds the cached
    /// identifier index when removal succeeds.
    pub(crate) fn remove_session_at(&mut self, session_index: usize) -> Option<Session> {
        if session_index >= self.sessions.len() {
            return None;
        }

        let session = self.sessions.remove(session_index);
        self.rebuild_session_index_by_id();

        Some(session)
    }

    /// Copies current values from one runtime handle into its `Session`
    /// snapshot.
    pub fn sync_session_from_handle(&mut self, session_id: &str) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session_handles) = self.handles.get(session_id) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };

        Self::sync_session_with_handles(session, session_handles);
    }

    /// Copies current values from runtime handles into plain `Session` fields.
    ///
    /// The runtime uses targeted `sync_session_from_handle()` calls for
    /// queued `SessionUpdated` events. This full sweep remains for explicit
    /// catch-up paths and focused tests.
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
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };

        session.stats.added_lines = added_lines;
        session.stats.deleted_lines = deleted_lines;
        session.size = session_size;
    }

    /// Replaces all cached session git-status snapshots with one fresh poll
    /// result.
    pub(crate) fn replace_session_git_statuses(
        &mut self,
        session_git_statuses: HashMap<String, SessionGitStatus>,
    ) {
        self.session_git_statuses = session_git_statuses;
    }

    /// Replaces cached worktree-availability snapshots with one fresh reload
    /// result.
    pub(crate) fn replace_session_worktree_availability(
        &mut self,
        session_worktree_availability: HashMap<String, bool>,
    ) {
        self.session_worktree_availability = session_worktree_availability;
    }

    /// Replaces cached detected session branch names with one fresh reload
    /// result.
    pub(crate) fn replace_session_branch_names(
        &mut self,
        session_branch_names: HashMap<String, String>,
    ) {
        self.session_branch_names = session_branch_names;
    }

    /// Updates cached worktree availability for one session after a lifecycle
    /// transition materializes or removes its worktree.
    pub(crate) fn set_session_worktree_available(&mut self, session_id: &str, is_available: bool) {
        self.session_worktree_availability
            .insert(session_id.to_string(), is_available);
    }

    /// Drops cached worktree availability for one removed session.
    pub(crate) fn remove_session_worktree_availability(&mut self, session_id: &str) {
        self.session_worktree_availability.remove(session_id);
    }

    /// Drops cached branch-name entries for sessions that are no longer active
    /// in memory.
    pub(crate) fn retain_session_branch_names(&mut self, active_session_ids: &HashSet<String>) {
        self.session_branch_names
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Drops cached git-status entries for sessions that are no longer active
    /// in memory.
    pub(crate) fn retain_session_git_statuses(&mut self, active_session_ids: &HashSet<String>) {
        self.session_git_statuses
            .retain(|session_id, _| active_session_ids.contains(session_id));
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

    /// Rebuilds the cached session-id lookup map from the current session
    /// ordering.
    fn rebuild_session_index_by_id(&mut self) {
        self.session_index_by_id = Self::build_session_index_by_id(&self.sessions);
    }

    /// Builds a session-id lookup map from one ordered session slice.
    fn build_session_index_by_id(sessions: &[Session]) -> HashMap<String, usize> {
        sessions
            .iter()
            .enumerate()
            .map(|(session_index, session)| (session.id.clone(), session_index))
            .collect()
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
            id: session_id.clone(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "old".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
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
            id: "session-2".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "Old".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
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
            id: "session-3".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "first line\n".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
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
            id: session_id.clone(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
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
    /// Verifies replacing the session list rebuilds identifier lookups.
    fn replace_sessions_rebuilds_session_id_index() {
        // Arrange
        let initial_session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            id: "session-1".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let replacement_session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            id: "session-2".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
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
            vec![initial_session],
            TableState::default(),
            Arc::new(FixedClock::new()),
            0,
            0,
        );

        // Act
        state.replace_sessions(vec![replacement_session]);

        // Assert
        assert_eq!(state.session_index_for_id("session-1"), None);
        assert_eq!(state.session_index_for_id("session-2"), Some(0));
    }

    #[test]
    /// Verifies removing a session keeps identifier lookups aligned with the
    /// remaining list order.
    fn remove_session_at_rebuilds_session_id_index() {
        // Arrange
        let first_session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            id: "session-1".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        };
        let second_session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: std::env::temp_dir(),
            id: "session-2".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
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
            vec![first_session, second_session],
            TableState::default(),
            Arc::new(FixedClock::new()),
            0,
            0,
        );

        // Act
        let removed_session = state.remove_session_at(0);

        // Assert
        assert_eq!(
            removed_session.map(|session| session.id),
            Some("session-1".to_string())
        );
        assert_eq!(state.session_index_for_id("session-1"), None);
        assert_eq!(state.session_index_for_id("session-2"), Some(0));
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
            id: "session-4".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentKind::Gemini.default_model(),
            output: "abc".to_string(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
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
}
