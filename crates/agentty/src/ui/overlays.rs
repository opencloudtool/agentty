use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::ProjectSwitcherItem;
use crate::ui::router::{ListBackgroundRenderContext, render_list_background};
use crate::ui::state::app_mode::{AppMode, HelpContext};
use crate::ui::{Component, Page, components, pages};

/// Borrowed parameters for rendering the sync-blocked popup overlay.
#[derive(Clone, Copy)]
pub(crate) struct SyncBlockedPopupRenderContext<'a> {
    pub(crate) default_branch: Option<&'a str>,
    pub(crate) is_loading: bool,
    pub(crate) message: &'a str,
    pub(crate) project_name: Option<&'a str>,
    pub(crate) title: &'a str,
}

/// Renders the list background and generic confirmation overlay.
pub(crate) fn render_confirmation_overlay(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    list_background: ListBackgroundRenderContext<'_>,
) {
    render_list_background(f, area, list_background);

    let AppMode::Confirmation {
        confirmation_message,
        confirmation_title,
        selected_confirmation_index,
        ..
    } = mode
    else {
        unreachable!("matched confirmation mode above");
    };

    components::confirmation_overlay::ConfirmationOverlay::new(
        confirmation_title,
        confirmation_message,
    )
    .selected_yes(*selected_confirmation_index == 0)
    .render(f, area);
}

/// Renders the list background and sync informational popup overlay.
pub(crate) fn render_sync_blocked_popup(
    f: &mut Frame,
    area: Rect,
    list_background: ListBackgroundRenderContext<'_>,
    context: SyncBlockedPopupRenderContext<'_>,
) {
    let SyncBlockedPopupRenderContext {
        default_branch,
        is_loading,
        message,
        project_name,
        title,
    } = context;

    render_list_background(f, area, list_background);

    let popup_message = sync_popup_message(default_branch, message, project_name);

    components::info_overlay::InfoOverlay::new(title, &popup_message)
        .is_loading(is_loading)
        .render(f, area);
}

/// Renders the list background and the project quick-switch overlay.
pub(crate) fn render_project_switcher_overlay(
    f: &mut Frame,
    area: Rect,
    list_background: ListBackgroundRenderContext<'_>,
    projects: &[ProjectSwitcherItem],
    selected_index: usize,
) {
    render_list_background(f, area, list_background);

    components::project_switcher_overlay::ProjectSwitcherOverlay::new(projects, selected_index)
        .render(f, area);
}

/// Composes sync popup body with optional project and branch context.
pub(crate) fn sync_popup_message(
    default_branch: Option<&str>,
    detail_message: &str,
    project_name: Option<&str>,
) -> String {
    match (project_name, default_branch) {
        (Some(project_name), Some(default_branch)) => format!(
            "Project `{project_name}` on main branch `{default_branch}`.\n\n{detail_message}"
        ),
        (Some(project_name), None) => format!("Project `{project_name}`.\n\n{detail_message}"),
        (None, Some(default_branch)) => {
            format!("Main branch `{default_branch}`.\n\n{detail_message}")
        }
        (None, None) => detail_message.to_string(),
    }
}

/// Renders help overlay above the context-specific background page.
pub(crate) fn render_help(
    f: &mut Frame,
    area: Rect,
    help_context: &HelpContext,
    scroll_offset: u16,
    list_background: ListBackgroundRenderContext<'_>,
    session_progress_messages: &HashMap<String, String>,
) {
    render_help_background(
        f,
        area,
        help_context,
        list_background,
        session_progress_messages,
    );

    components::help_overlay::HelpOverlay::new(help_context)
        .scroll_offset(scroll_offset)
        .render(f, area);
}

/// Renders background content behind help based on the source `HelpContext`.
fn render_help_background(
    f: &mut Frame,
    area: Rect,
    help_context: &HelpContext,
    list_background: ListBackgroundRenderContext<'_>,
    session_progress_messages: &HashMap<String, String>,
) {
    let sessions = list_background.sessions;

    match help_context {
        HelpContext::List { .. } => {
            render_list_background(f, area, list_background);
        }
        HelpContext::View {
            done_session_output_mode,
            session_id,
            scroll_offset: view_scroll,
            ..
        } => {
            if let Some(session_index) = sessions
                .iter()
                .position(|session| session.id == *session_id)
            {
                let bg_mode = AppMode::View {
                    done_session_output_mode: *done_session_output_mode,
                    session_id: session_id.clone(),
                    scroll_offset: *view_scroll,
                };
                let active_progress = session_progress_messages
                    .get(session_id)
                    .map(std::string::String::as_str);
                pages::session_chat::SessionChatPage::new(
                    sessions,
                    session_index,
                    *view_scroll,
                    &bg_mode,
                    active_progress,
                )
                .render(f, area);
            }
        }
        HelpContext::Diff {
            diff,
            file_explorer_selected_index,
            scroll_offset: diff_scroll,
            session_id,
        } => {
            if let Some(session) = sessions.iter().find(|session| session.id == *session_id) {
                pages::diff::DiffPage::new(
                    session,
                    diff.clone(),
                    *diff_scroll,
                    *file_explorer_selected_index,
                )
                .render(f, area);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_popup_message_with_project_and_branch() {
        // Arrange
        let default_branch = Some("develop");
        let detail_message = "Synchronizing with its upstream.";
        let project_name = Some("agentty");

        // Act
        let message = sync_popup_message(default_branch, detail_message, project_name);

        // Assert
        assert_eq!(
            message,
            "Project `agentty` on main branch `develop`.\n\nSynchronizing with its upstream."
        );
    }

    #[test]
    fn test_sync_popup_message_with_project_only() {
        // Arrange
        let default_branch = None;
        let detail_message = "Synchronization is blocked.";
        let project_name = Some("agentty");

        // Act
        let message = sync_popup_message(default_branch, detail_message, project_name);

        // Assert
        assert_eq!(message, "Project `agentty`.\n\nSynchronization is blocked.");
    }

    #[test]
    fn test_sync_popup_message_with_branch_only() {
        // Arrange
        let default_branch = Some("main");
        let detail_message = "Synchronization is blocked.";
        let project_name = None;

        // Act
        let message = sync_popup_message(default_branch, detail_message, project_name);

        // Assert
        assert_eq!(
            message,
            "Main branch `main`.\n\nSynchronization is blocked."
        );
    }

    #[test]
    fn test_sync_popup_message_without_project_or_branch() {
        // Arrange
        let default_branch = None;
        let detail_message = "Synchronization is blocked.";
        let project_name = None;

        // Act
        let message = sync_popup_message(default_branch, detail_message, project_name);

        // Assert
        assert_eq!(message, "Synchronization is blocked.");
    }
}
