//! Session refresh scheduling and post-reload view state restoration.

use std::collections::HashSet;
use std::time::Instant;

use super::SESSION_REFRESH_INTERVAL;
use crate::app::{AppServices, ProjectManager, SessionManager};
use crate::ui::state::app_mode::AppMode;

impl SessionManager {
    /// Reloads session rows when the metadata cache indicates a change.
    ///
    /// This is a low-frequency fallback safety poll; primary refreshes should
    /// come from explicit `RefreshSessions` events.
    pub async fn refresh_sessions_if_needed(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        if !self.is_session_refresh_due() {
            return;
        }

        self.refresh_deadline = self.next_refresh_deadline();

        let Ok(sessions_metadata) = services.db().load_sessions_metadata().await else {
            return;
        };
        let (sessions_row_count, sessions_updated_at_max) = sessions_metadata;
        if sessions_row_count == self.row_count && sessions_updated_at_max == self.updated_at_max {
            return;
        }

        self.reload_sessions(mode, projects, services, Some(sessions_metadata))
            .await;
    }

    /// Reloads sessions immediately, bypassing refresh deadline checks.
    pub(crate) async fn refresh_sessions_now(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        let sessions_metadata = services.db().load_sessions_metadata().await.ok();
        self.reload_sessions(mode, projects, services, sessions_metadata)
            .await;
        self.refresh_deadline = self.next_refresh_deadline();
    }

    /// Reloads sessions and derived statistics, then restores UI state.
    async fn reload_sessions(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
        sessions_metadata: Option<(i64, i64)>,
    ) {
        let selected_index = self.table_state.selected();
        let selected_session_id = selected_index
            .and_then(|index| self.sessions.get(index))
            .map(|session| session.id.clone());

        let (sessions, stats_activity) = Self::load_sessions(
            services.base_path(),
            services.db(),
            projects.active_project_id(),
            projects.working_dir(),
            &mut self.handles,
        )
        .await;
        self.sessions = sessions;
        self.stats_activity = stats_activity;
        self.restore_table_selection(selected_session_id.as_deref(), selected_index);
        self.ensure_mode_session_exists(mode);

        let active_session_ids: HashSet<String> = self
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        self.workers
            .retain(|session_id, _| active_session_ids.contains(session_id));

        if let Some((sessions_row_count, sessions_updated_at_max)) = sessions_metadata {
            self.row_count = sessions_row_count;
            self.updated_at_max = sessions_updated_at_max;
        } else {
            self.update_sessions_metadata_cache(services).await;
        }
    }

    /// Returns `true` when periodic session refresh should run.
    fn is_session_refresh_due(&self) -> bool {
        self.clock.now_instant() >= self.refresh_deadline
    }

    /// Computes the next refresh deadline from the injected clock.
    fn next_refresh_deadline(&self) -> Instant {
        self.clock.now_instant() + SESSION_REFRESH_INTERVAL
    }

    /// Restores table selection after session list reload.
    fn restore_table_selection(
        &mut self,
        selected_session_id: Option<&str>,
        selected_index: Option<usize>,
    ) {
        if self.sessions.is_empty() {
            self.table_state.select(None);

            return;
        }

        if let Some(session_id) = selected_session_id
            && let Some(index) = self
                .sessions
                .iter()
                .position(|session| session.id == session_id)
        {
            self.table_state.select(Some(index));

            return;
        }

        let restored_index = selected_index.map(|index| index.min(self.sessions.len() - 1));
        self.table_state.select(restored_index);
    }

    /// Switches back to list mode if the currently viewed session is missing.
    fn ensure_mode_session_exists(&self, mode: &mut AppMode) {
        let mode_session_id = match &*mode {
            AppMode::Confirmation {
                session_id: Some(session_id),
                ..
            }
            | AppMode::Prompt { session_id, .. }
            | AppMode::Question { session_id, .. }
            | AppMode::View { session_id, .. }
            | AppMode::Diff { session_id, .. } => Some(session_id),
            _ => None,
        };
        let Some(session_id) = mode_session_id else {
            return;
        };
        if self.session_index_for_id(session_id).is_none() {
            *mode = AppMode::List;
        }
    }

    /// Refreshes cached session metadata used by incremental list reloads.
    pub(crate) async fn update_sessions_metadata_cache(&mut self, services: &AppServices) {
        if let Ok((sessions_row_count, sessions_updated_at_max)) =
            services.db().load_sessions_metadata().await
        {
            self.row_count = sessions_row_count;
            self.updated_at_max = sessions_updated_at_max;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::{Instant, SystemTime};

    use ratatui::widgets::TableState;

    use super::*;
    use crate::app::SessionState;
    use crate::domain::agent::AgentKind;
    #[test]
    fn test_is_session_refresh_due_returns_false_before_deadline() {
        // Arrange
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn crate::app::session::Clock> = fake_clock;
        let session_manager = session_manager_fixture(clock);

        // Act
        let refresh_due = session_manager.is_session_refresh_due();
        let wall_clock = session_manager.clock.now_system_time();

        // Assert
        assert!(!refresh_due);
        assert_eq!(wall_clock, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn test_is_session_refresh_due_returns_true_at_deadline() {
        // Arrange
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn crate::app::session::Clock> = fake_clock.clone();
        let session_manager = session_manager_fixture(clock);
        fake_clock.set_now_instant(now + SESSION_REFRESH_INTERVAL);

        // Act
        let refresh_due = session_manager.is_session_refresh_due();

        // Assert
        assert!(refresh_due);
    }

    /// Builds a session manager with deterministic time and empty state.
    fn session_manager_fixture(clock: Arc<dyn crate::app::session::Clock>) -> SessionManager {
        let git_client: Arc<dyn crate::infra::git::GitClient> =
            Arc::new(crate::infra::git::MockGitClient::new());

        SessionManager::new(
            crate::app::session::SessionDefaults {
                model: AgentKind::Gemini.default_model(),
            },
            git_client,
            SessionState::new(
                HashMap::new(),
                Vec::new(),
                TableState::default(),
                clock,
                0,
                0,
            ),
            Vec::new(),
        )
    }

    /// Test clock implementation with mutable `Instant` and `SystemTime`.
    struct FakeClock {
        instant: Mutex<Instant>,
        system_time: Mutex<SystemTime>,
    }

    impl FakeClock {
        /// Creates a fake clock seeded with deterministic wall-clock values.
        fn new(instant: Instant, system_time: SystemTime) -> Self {
            Self {
                instant: Mutex::new(instant),
                system_time: Mutex::new(system_time),
            }
        }

        /// Overrides the fake monotonic instant used by refresh checks.
        fn set_now_instant(&self, instant: Instant) {
            let mut current_instant = self
                .instant
                .lock()
                .expect("fake clock instant lock should not be poisoned");
            *current_instant = instant;
        }
    }

    impl crate::app::session::Clock for FakeClock {
        fn now_instant(&self) -> Instant {
            *self
                .instant
                .lock()
                .expect("fake clock instant lock should not be poisoned")
        }

        fn now_system_time(&self) -> SystemTime {
            *self
                .system_time
                .lock()
                .expect("fake clock system-time lock should not be poisoned")
        }
    }
}
