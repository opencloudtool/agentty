//! Session refresh scheduling and post-reload view state restoration.

use std::collections::HashSet;
use std::time::Instant;

use super::SESSION_REFRESH_INTERVAL;
use crate::app::{AppServices, PrManager, ProjectManager, SessionManager};
use crate::model::AppMode;

impl SessionManager {
    /// Reloads session rows when the metadata cache indicates a change.
    ///
    /// This is a low-frequency fallback safety poll; primary refreshes should
    /// come from explicit `RefreshSessions` events.
    pub async fn refresh_sessions_if_needed(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        prs: &PrManager,
        services: &AppServices,
    ) {
        if !self.is_session_refresh_due() {
            return;
        }

        self.refresh_deadline = Instant::now() + SESSION_REFRESH_INTERVAL;

        let Ok(sessions_metadata) = services.db().load_sessions_metadata().await else {
            return;
        };
        let (sessions_row_count, sessions_updated_at_max) = sessions_metadata;
        if sessions_row_count == self.row_count && sessions_updated_at_max == self.updated_at_max {
            return;
        }

        self.reload_sessions(mode, projects, prs, services, Some(sessions_metadata))
            .await;
    }

    /// Reloads sessions immediately, bypassing refresh deadline checks.
    pub(crate) async fn refresh_sessions_now(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        prs: &PrManager,
        services: &AppServices,
    ) {
        let sessions_metadata = services.db().load_sessions_metadata().await.ok();
        self.reload_sessions(mode, projects, prs, services, sessions_metadata)
            .await;
        self.refresh_deadline = Instant::now() + SESSION_REFRESH_INTERVAL;
    }

    async fn reload_sessions(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        prs: &PrManager,
        services: &AppServices,
        sessions_metadata: Option<(i64, i64)>,
    ) {
        let selected_index = self.table_state.selected();
        let selected_session_id = selected_index
            .and_then(|index| self.sessions.get(index))
            .map(|session| session.id.clone());

        self.sessions = Self::load_sessions(
            services.base_path(),
            services.db(),
            projects,
            &mut self.handles,
        )
        .await;
        prs.start_pr_polling_for_pull_request_sessions(services, self);
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
        Instant::now() >= self.refresh_deadline
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
            AppMode::ConfirmDeleteSession { session_id, .. }
            | AppMode::Prompt { session_id, .. }
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
