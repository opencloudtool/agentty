//! Immutable application view projection consumed by frontends.

use std::collections::HashMap;
use std::path::Path;

use crate::app::session_state::SessionGitStatus;
use crate::app::{App, ProjectSyncStatus, Tab, UpdateStatus, session};
use crate::domain::agent::{AgentCliInfo, ReasoningLevel};
use crate::domain::project::ProjectListItem;
use crate::domain::resource::SessionResources;
use crate::domain::session::{DailyActivity, Session, SessionId};
use crate::domain::theme::ColorTheme;
use crate::infra::clock;
use crate::presentation::app_mode::{AppMode, HelpContext};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::settings::SettingsScreenSnapshot;

/// Focused-review display state for the visible session.
pub(crate) struct SessionReviewView<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) text: Option<&'a str>,
}

/// Borrowed immutable application data required by one frontend frame.
pub(crate) struct AppViewSnapshot<'a> {
    pub(crate) active_project_id: i64,
    pub(crate) active_prompt_outputs: &'a HashMap<SessionId, String>,
    pub(crate) available_agent_clis: Vec<AgentCliInfo>,
    pub(crate) current_tab: Tab,
    pub(crate) current_version_display_text: &'a str,
    pub(crate) default_reasoning_level: ReasoningLevel,
    pub(crate) frame_time: FrameTime,
    pub(crate) git_branch: Option<&'a str>,
    pub(crate) git_status: Option<(u32, u32)>,
    pub(crate) git_upstream_ref: Option<&'a str>,
    pub(crate) is_tmux_session: bool,
    pub(crate) latest_available_version: Option<&'a str>,
    pub(crate) mode: &'a AppMode,
    pub(crate) mru_project_order: &'a [usize],
    pub(crate) project_selected_index: Option<usize>,
    pub(crate) project_sync_status: Option<&'a ProjectSyncStatus>,
    pub(crate) projects: &'a [ProjectListItem],
    pub(crate) session_branch_names: &'a HashMap<SessionId, String>,
    pub(crate) session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    pub(crate) session_index_by_id: &'a HashMap<SessionId, usize>,
    pub(crate) session_progress_messages: &'a HashMap<SessionId, String>,
    /// Latest tracked process-tree totals.
    pub(crate) session_resources: &'a HashMap<SessionId, SessionResources>,
    pub(crate) session_review: Option<SessionReviewView<'a>>,
    pub(crate) session_selected_index: Option<usize>,
    pub(crate) session_update_versions: &'a HashMap<SessionId, u64>,
    pub(crate) session_worktree_availability: &'a HashMap<SessionId, bool>,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings_screen: Option<SettingsScreenSnapshot>,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) status_bar_fyi_rotation_index: u64,
    pub(crate) theme: ColorTheme,
    pub(crate) update_status: Option<&'a UpdateStatus>,
    pub(crate) working_dir: &'a Path,
}

impl App {
    /// Projects immutable application state for one frontend frame.
    pub(crate) fn view_snapshot(&self) -> AppViewSnapshot<'_> {
        let clock_client = self.services.clock();
        let system_time = clock_client.now_system_time();
        let wall_clock_unix_seconds = session::unix_timestamp_from_system_time(system_time);
        let frame_time = FrameTime::new(
            wall_clock_unix_seconds,
            clock::unix_timestamp_millis(system_time),
            clock_client.local_utc_offset_seconds(wall_clock_unix_seconds),
        );
        let status_bar_fyi_rotation_index =
            u64::try_from(wall_clock_unix_seconds.div_euclid(60)).unwrap_or_default();
        let visible_session_id = visible_review_session_id(&self.mode);
        let session_review = visible_session_id.map(|session_id| {
            let (_, text) = self.review_view_state(session_id);

            SessionReviewView { session_id, text }
        });
        let project = self.projects.render_parts();
        let sessions = self.sessions.render_parts();
        let current_tab = self.tabs.current();
        let settings_screen = (current_tab == Tab::Settings)
            .then(|| self.settings_presentation.snapshot(&self.settings.view()));

        AppViewSnapshot {
            active_project_id: project.active_project_id,
            active_prompt_outputs: sessions.active_prompt_outputs,
            available_agent_clis: self.services.available_agent_clis(),
            current_tab,
            current_version_display_text: &self.current_version_display_text,
            default_reasoning_level: self.settings.default_smart_reasoning_level,
            frame_time,
            git_branch: project.git_branch,
            git_status: project.git_status,
            git_upstream_ref: project.git_upstream_ref,
            is_tmux_session: self.is_tmux_session(),
            latest_available_version: self.latest_available_version.as_deref(),
            mode: &self.mode,
            mru_project_order: project.mru_project_order,
            project_selected_index: project.selected_index,
            project_sync_status: self.project_sync_status.as_ref(),
            projects: project.project_items,
            session_branch_names: sessions.session_branch_names,
            session_git_statuses: sessions.session_git_statuses,
            session_index_by_id: sessions.session_index_by_id,
            session_resources: sessions.session_resources,
            session_progress_messages: &self.session_progress_messages,
            session_review,
            session_selected_index: sessions.selected_index,
            session_update_versions: &self.last_seen_session_update_versions,
            session_worktree_availability: sessions.session_worktree_availability,
            sessions: sessions.sessions,
            settings_screen,
            stats_activity: sessions.stats_activity,
            status_bar_fyi_rotation_index,
            theme: self.settings.theme,
            update_status: self.update_status.as_ref(),
            working_dir: project.working_dir,
        }
    }
}

/// Returns the session visible behind the active presentation mode.
fn visible_review_session_id(mode: &AppMode) -> Option<&str> {
    match mode {
        AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::DiffLoading { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::Help {
            context: HelpContext::View { session_id, .. } | HelpContext::Diff { session_id, .. },
            ..
        } => Some(session_id),
        AppMode::Confirmation {
            restore_view: Some(restore_view),
            ..
        }
        | AppMode::LaunchConfigurationSelector { restore_view, .. }
        | AppMode::PublishBranchInput { restore_view, .. }
        | AppMode::ViewInfoPopup { restore_view, .. } => Some(&restore_view.session_id),
        AppMode::List
        | AppMode::SessionCreation { .. }
        | AppMode::StackAppendParentSelection { .. }
        | AppMode::PreCommitHookWarning { .. }
        | AppMode::ProjectSwitcher { .. }
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Help { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::app_mode::{DiffFocus, DiffLineComments};

    #[test]
    fn visible_review_session_id_includes_diff_comments() {
        // Arrange
        let mode = AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: crate::presentation::app_mode::DiffPreview::default(),
            review_comments: Some(crate::presentation::app_mode::DiffReviewComments::loading(
                1,
            )),
            restore: None,
            scroll_cache: None,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };

        // Act
        let session_id = visible_review_session_id(&mode);

        // Assert
        assert_eq!(session_id, Some("session-id"));
    }

    #[test]
    fn visible_review_session_id_includes_loading_diff() {
        // Arrange
        let mode = AppMode::DiffLoading {
            fallback_view_scroll_offset: None,
            request_id: 1,
            restore: None,
            session_id: "loading-session".into(),
            sidebar_focus: crate::presentation::app_mode::DiffSidebarFocus::Files,
        };

        // Act
        let session_id = visible_review_session_id(&mode);

        // Assert
        assert_eq!(session_id, Some("loading-session"));
    }

    #[tokio::test]
    async fn view_snapshot_builds_settings_screen_only_for_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;

        // Act
        app.tabs.set(Tab::Sessions);
        let sessions_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();
        app.tabs.set(Tab::Settings);
        let settings_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();

        // Assert
        assert!(!sessions_tab_has_settings_screen);
        assert!(settings_tab_has_settings_screen);
    }
}
