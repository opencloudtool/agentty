use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::{SettingsManager, Tab};
use crate::domain::project::ProjectListItem;
use crate::domain::session::{DailyActivity, Session};
use crate::ui::overlays::SyncBlockedPopupRenderContext;
use crate::ui::state::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::ui::{Component, Page, RenderContext, components, overlays, pages};

/// Shared borrowed data required to render list-page backgrounds.
pub(crate) struct ListBackgroundRenderContext<'a> {
    /// Identifier for the currently active project in the project list tab.
    pub(crate) active_project_id: i64,
    pub(crate) current_tab: Tab,
    pub(crate) project_table_state: &'a mut TableState,
    pub(crate) projects: &'a [ProjectListItem],
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a mut SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) table_state: &'a mut TableState,
}

/// Shared mutable routing data reused across app modes in `route_frame`.
struct RouteSharedContext<'a> {
    /// Identifier for the active project shared across list-mode renders.
    active_project_id: i64,
    current_tab: Tab,
    project_table_state: &'a mut TableState,
    projects: &'a [ProjectListItem],
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

impl RouteSharedContext<'_> {
    /// Creates a list-background context for overlays/pages that render on top
    /// of the tabbed list content.
    fn list_background(&mut self) -> ListBackgroundRenderContext<'_> {
        ListBackgroundRenderContext {
            active_project_id: self.active_project_id,
            current_tab: self.current_tab,
            project_table_state: self.project_table_state,
            projects: self.projects,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
        }
    }
}

/// Borrowed inputs for rendering a session chat page.
#[derive(Clone, Copy)]
struct SessionChatRenderContext<'a> {
    mode: &'a AppMode,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
    sessions: &'a [Session],
    scroll_offset: Option<u16>,
}

/// Shared immutable routing inputs that are not part of list-background state.
#[derive(Clone, Copy)]
struct RouteAuxContext<'a> {
    session_progress_messages: &'a HashMap<String, String>,
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        current_tab,
        mode,
        project_table_state,
        projects,
        session_progress_messages,
        settings,
        stats_activity,
        sessions,
        table_state,
        ..
    } = context;

    let mut shared = RouteSharedContext {
        active_project_id,
        current_tab,
        project_table_state,
        projects,
        sessions,
        settings,
        stats_activity,
        table_state,
    };

    let aux = RouteAuxContext {
        session_progress_messages,
    };

    if render_list_or_overlay_mode(f, area, mode, &mut shared, aux) {
        return;
    }

    render_session_or_diff_mode(f, area, mode, shared.sessions, aux);
}

/// Renders all list/overlay-driven modes and returns whether it handled `mode`.
fn render_list_or_overlay_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    shared: &mut RouteSharedContext<'_>,
    aux: RouteAuxContext<'_>,
) -> bool {
    match mode {
        AppMode::List => render_list_background(f, area, shared.list_background()),
        AppMode::Confirmation {
            confirmation_intent,
            confirmation_message,
            confirmation_title,
            restore_view,
            selected_confirmation_index,
            ..
        } => {
            if *confirmation_intent == ConfirmationIntent::MergeSession
                && let Some(view_mode) = restore_view
            {
                render_merge_confirmation_overlay(
                    f,
                    area,
                    &MergeConfirmationContext {
                        confirmation_message,
                        confirmation_title,
                        selected_confirmation_index: *selected_confirmation_index,
                    },
                    view_mode,
                    shared.sessions,
                    aux.session_progress_messages,
                );
            } else {
                overlays::render_confirmation_overlay(f, area, mode, shared.list_background());
            }
        }

        AppMode::SyncBlockedPopup {
            default_branch,
            is_loading,
            message,
            project_name,
            title,
        } => overlays::render_sync_blocked_popup(
            f,
            area,
            shared.list_background(),
            SyncBlockedPopupRenderContext {
                default_branch: default_branch.as_deref(),
                is_loading: *is_loading,
                message,
                project_name: project_name.as_deref(),
                title,
            },
        ),
        AppMode::Help {
            context: help_context,
            scroll_offset,
        } => overlays::render_help(
            f,
            area,
            help_context,
            *scroll_offset,
            shared.list_background(),
            aux.session_progress_messages,
        ),
        AppMode::View { .. } | AppMode::Prompt { .. } | AppMode::Diff { .. } => {
            return false;
        }
    }

    true
}

/// Borrowed context for the confirmation overlay portion of a merge
/// confirmation render.
struct MergeConfirmationContext<'a> {
    /// The body text displayed inside the confirmation dialog.
    confirmation_message: &'a str,
    /// The header title of the confirmation dialog.
    confirmation_title: &'a str,
    /// Index of the currently highlighted confirmation option.
    selected_confirmation_index: usize,
}

/// Renders merge confirmation above the originating session chat page.
fn render_merge_confirmation_overlay(
    f: &mut Frame,
    area: Rect,
    context: &MergeConfirmationContext<'_>,
    view_mode: &ConfirmationViewMode,
    sessions: &[Session],
    session_progress_messages: &HashMap<String, String>,
) {
    let background_mode = view_mode.clone().into_view_mode();

    render_session_chat(
        f,
        area,
        SessionChatRenderContext {
            mode: &background_mode,
            session_id: &view_mode.session_id,
            session_progress_messages,
            sessions,
            scroll_offset: view_mode.scroll_offset,
        },
    );

    components::confirmation_overlay::ConfirmationOverlay::new(
        context.confirmation_title,
        context.confirmation_message,
    )
    .selected_yes(context.selected_confirmation_index == 0)
    .render(f, area);
}

/// Renders session-scoped modes tied to one selected session.
fn render_session_or_diff_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    sessions: &[Session],
    aux: RouteAuxContext<'_>,
) {
    match mode {
        AppMode::View {
            session_id,
            scroll_offset,
            ..
        }
        | AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        } => render_session_chat(
            f,
            area,
            SessionChatRenderContext {
                mode,
                session_id,
                session_progress_messages: aux.session_progress_messages,
                sessions,
                scroll_offset: *scroll_offset,
            },
        ),
        AppMode::Diff {
            diff,
            file_explorer_selected_index,
            scroll_offset,
            session_id,
        } => render_diff_mode(
            f,
            area,
            sessions,
            session_id,
            diff,
            *scroll_offset,
            *file_explorer_selected_index,
        ),
        AppMode::List
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Help { .. } => {}
    }
}

/// Renders the session chat page for view and prompt modes.
fn render_session_chat(f: &mut Frame, area: Rect, context: SessionChatRenderContext<'_>) {
    let SessionChatRenderContext {
        mode,
        session_id,
        session_progress_messages,
        sessions,
        scroll_offset,
    } = context;

    let Some(session_index) = sessions.iter().position(|session| session.id == session_id) else {
        return;
    };

    let active_progress = session_progress_messages
        .get(session_id)
        .map(std::string::String::as_str);

    pages::session_chat::SessionChatPage::new(
        sessions,
        session_index,
        scroll_offset,
        mode,
        active_progress,
    )
    .render(f, area);
}

/// Renders the diff page for a specific session when present.
fn render_diff_mode(
    f: &mut Frame,
    area: Rect,
    sessions: &[Session],
    session_id: &str,
    diff: &str,
    scroll_offset: u16,
    file_explorer_selected_index: usize,
) {
    if let Some(session) = sessions.iter().find(|session| session.id == session_id) {
        pages::diff::DiffPage::new(
            session,
            diff.to_string(),
            scroll_offset,
            file_explorer_selected_index,
        )
        .render(f, area);
    }
}

/// Renders base list tabs and the currently selected list tab content.
pub(crate) fn render_list_background(
    f: &mut Frame,
    content_area: Rect,
    context: ListBackgroundRenderContext<'_>,
) {
    let ListBackgroundRenderContext {
        active_project_id,
        current_tab,
        project_table_state,
        projects,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;

    let chunks = Layout::default()
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(content_area);

    components::tab::Tabs::new(current_tab, active_project_id, projects).render(f, chunks[0]);

    match current_tab {
        Tab::Projects => {
            pages::project_list::ProjectListPage::new(
                projects,
                project_table_state,
                active_project_id,
            )
            .render(f, chunks[1]);
        }
        Tab::Sessions => {
            pages::session_list::SessionListPage::new(sessions, table_state).render(f, chunks[1]);
        }
        Tab::Stats => {
            pages::stats::StatsPage::new(sessions, stats_activity).render(f, chunks[1]);
        }
        Tab::Settings => {
            pages::settings::SettingsPage::new(settings).render(f, chunks[1]);
        }
    }
}
