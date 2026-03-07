use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Padding};

use crate::ui::router::{ListBackgroundRenderContext, render_list_background};
use crate::ui::state::app_mode::{AppMode, HelpContext};
use crate::ui::style::palette;
use crate::ui::{Component, Page, component, page};

const OVERLAY_HORIZONTAL_PADDING: u16 = 2;
const OVERLAY_VERTICAL_PADDING: u16 = 1;

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
    render_overlay_backdrop(f, area);

    let AppMode::Confirmation {
        confirmation_message,
        confirmation_title,
        selected_confirmation_index,
        ..
    } = mode
    else {
        unreachable!("matched confirmation mode above");
    };

    component::confirmation_overlay::ConfirmationOverlay::new(
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
    render_overlay_backdrop(f, area);

    let popup_message = sync_popup_message(default_branch, message, project_name);

    component::info_overlay::InfoOverlay::new(title, &popup_message)
        .is_loading(is_loading)
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
    render_overlay_backdrop(f, area);

    component::help_overlay::HelpOverlay::new(help_context)
        .scroll_offset(scroll_offset)
        .render(f, area);
}

/// Renders a dimmed backdrop to emphasize a centered modal overlay.
pub(crate) fn render_overlay_backdrop(f: &mut Frame, area: Rect) {
    f.render_widget(Block::default().style(overlay_backdrop_style()), area);
}

/// Returns a centered popup rectangle constrained by bounds and minimum size.
pub(crate) fn centered_popup_area(
    area: Rect,
    width_percent: u16,
    height_percent: u16,
    min_width: u16,
    min_height: u16,
) -> Rect {
    let popup_width = (area.width * width_percent / 100)
        .max(min_width)
        .min(area.width);
    let popup_height = (area.height * height_percent / 100)
        .max(min_height)
        .min(area.height);

    Rect::new(
        area.x + (area.width.saturating_sub(popup_width)) / 2,
        area.y + (area.height.saturating_sub(popup_height)) / 2,
        popup_width,
        popup_height,
    )
}

/// Returns the inner text width for overlay content based on shared frame
/// chrome.
pub(crate) fn overlay_content_width(popup_width: u16) -> usize {
    let horizontal_chrome = 2 + (OVERLAY_HORIZONTAL_PADDING * 2);

    usize::from(popup_width.saturating_sub(horizontal_chrome).max(1))
}

/// Returns the total popup height required to render a given number of body
/// lines inside the shared overlay frame.
pub(crate) fn overlay_required_height(inner_line_count: usize) -> u16 {
    let vertical_chrome = 2 + (OVERLAY_VERTICAL_PADDING * 2);

    u16::try_from(inner_line_count.saturating_add(usize::from(vertical_chrome))).unwrap_or(u16::MAX)
}

/// Builds a shared rounded overlay frame block with centered styled title and
/// default body padding.
pub(crate) fn overlay_block(title: &str, border_color: Color) -> Block<'static> {
    let title_text = format!(" {title} ");

    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::new(
            OVERLAY_HORIZONTAL_PADDING,
            OVERLAY_HORIZONTAL_PADDING,
            OVERLAY_VERTICAL_PADDING,
            OVERLAY_VERTICAL_PADDING,
        ))
        .title(Span::styled(title_text, overlay_title_style(border_color)))
        .title_alignment(Alignment::Center)
}

/// Returns the dimmed backdrop style applied behind overlay popups.
fn overlay_backdrop_style() -> Style {
    Style::default()
        .bg(palette::SURFACE_OVERLAY)
        .fg(palette::TEXT_MUTED)
}

/// Returns the shared title text style for overlay frame headers.
fn overlay_title_style(border_color: Color) -> Style {
    Style::default()
        .fg(border_color)
        .add_modifier(Modifier::BOLD)
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
            focused_review_status_message,
            focused_review_text,
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
                    focused_review_status_message: focused_review_status_message.clone(),
                    focused_review_text: focused_review_text.clone(),
                    session_id: session_id.clone(),
                    scroll_offset: *view_scroll,
                };
                let active_progress = session_progress_messages
                    .get(session_id)
                    .map(std::string::String::as_str);
                page::session_chat::SessionChatPage::new(
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
                page::diff::DiffPage::new(
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
    use crate::ui::style::palette;

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

    #[test]
    fn test_centered_popup_area_centers_within_bounds() {
        // Arrange
        let area = Rect::new(0, 0, 100, 50);

        // Act
        let popup_area = centered_popup_area(area, 40, 20, 30, 7);

        // Assert
        assert_eq!(popup_area.width, 40);
        assert_eq!(popup_area.height, 10);
        assert_eq!(popup_area.x, 30);
        assert_eq!(popup_area.y, 20);
    }

    #[test]
    fn test_centered_popup_area_clamps_to_small_terminal() {
        // Arrange
        let area = Rect::new(0, 0, 20, 6);

        // Act
        let popup_area = centered_popup_area(area, 50, 50, 30, 10);

        // Assert
        assert_eq!(popup_area.width, 20);
        assert_eq!(popup_area.height, 6);
        assert_eq!(popup_area.x, 0);
        assert_eq!(popup_area.y, 0);
    }

    #[test]
    fn test_overlay_content_width_subtracts_shared_frame_chrome() {
        // Arrange
        let popup_width = 40;

        // Act
        let content_width = overlay_content_width(popup_width);

        // Assert
        assert_eq!(content_width, 34);
    }

    #[test]
    fn test_overlay_required_height_adds_shared_frame_chrome() {
        // Arrange
        let inner_line_count = 8;

        // Act
        let total_height = overlay_required_height(inner_line_count);

        // Assert
        assert_eq!(total_height, 12);
    }

    #[test]
    fn test_render_overlay_backdrop_applies_dimmed_style() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(8, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_overlay_backdrop(frame, area);
            })
            .expect("failed to draw");

        // Assert
        let first_cell = terminal
            .backend()
            .buffer()
            .content()
            .first()
            .expect("buffer should contain at least one cell");
        assert_eq!(first_cell.bg, palette::SURFACE_OVERLAY);
        assert_eq!(first_cell.fg, palette::TEXT_MUTED);
    }
}
