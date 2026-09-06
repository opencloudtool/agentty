//! Application snapshot projection at the UI boundary.

use ratatui::Frame;
use ratatui::widgets::TableState;

use crate::app::AppViewSnapshot;
use crate::ui::{RenderCacheStore, RenderContext, SessionReviewSnapshot, style};

/// Projects application data into one terminal frame.
pub(crate) fn render_app(
    snapshot: &AppViewSnapshot<'_>,
    frame: &mut Frame,
    project_table_state: &mut TableState,
    render_cache_store: &RenderCacheStore,
    session_table_state: &mut TableState,
) {
    project_table_state.select(snapshot.project_selected_index);
    session_table_state.select(snapshot.session_selected_index);
    let session_review_snapshot =
        snapshot
            .session_review
            .as_ref()
            .map(|review| SessionReviewSnapshot {
                session_id: review.session_id,
                text: review.text,
            });
    let _theme_scope = style::scoped_active_theme(snapshot.theme);

    super::render(
        frame,
        RenderContext {
            active_project_id: snapshot.active_project_id,
            available_agent_clis: &snapshot.available_agent_clis,
            current_tab: snapshot.current_tab,
            current_version_display_text: snapshot.current_version_display_text,
            default_reasoning_level: snapshot.default_reasoning_level,
            frame_time: snapshot.frame_time,
            git_branch: snapshot.git_branch,
            git_upstream_ref: snapshot.git_upstream_ref,
            git_status: snapshot.git_status,
            is_tmux_session: snapshot.is_tmux_session,
            latest_available_version: snapshot.latest_available_version,
            update_status: snapshot.update_status,
            mode: snapshot.mode,
            mru_project_order: snapshot.mru_project_order,
            render_cache_store,
            project_table_state,
            projects: snapshot.projects,
            project_sync_status: snapshot.project_sync_status,
            session_review_snapshot: session_review_snapshot.as_ref(),
            active_prompt_outputs: snapshot.active_prompt_outputs,
            session_branch_names: snapshot.session_branch_names,
            session_git_statuses: snapshot.session_git_statuses,
            session_index_by_id: snapshot.session_index_by_id,
            session_resources: snapshot.session_resources,
            session_progress_messages: snapshot.session_progress_messages,
            session_update_versions: snapshot.session_update_versions,
            session_worktree_availability: snapshot.session_worktree_availability,
            settings_screen: snapshot.settings_screen.as_ref(),
            stats_activity: snapshot.stats_activity,
            sessions: snapshot.sessions,
            status_bar_fyi_rotation_index: snapshot.status_bar_fyi_rotation_index,
            table_state: session_table_state,
            working_dir: snapshot.working_dir,
        },
    );
}
