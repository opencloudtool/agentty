use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::{SettingsManager, Tab};
use crate::domain::permission::PlanFollowup;
use crate::domain::project::Project;
use crate::domain::session::{AllTimeModelUsage, CodexUsageLimits, DailyActivity, Session};
use crate::ui::overlays::SyncBlockedPopupRenderContext;
use crate::ui::state::app_mode::AppMode;
use crate::ui::{Component, Page, RenderContext, components, overlays, pages};

/// Shared borrowed data required to render list-page backgrounds.
pub(crate) struct ListBackgroundRenderContext<'a> {
    pub(crate) all_time_model_usage: &'a [AllTimeModelUsage],
    pub(crate) codex_usage_limits: Option<CodexUsageLimits>,
    pub(crate) current_tab: Tab,
    pub(crate) longest_session_duration_seconds: u64,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a mut SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) table_state: &'a mut TableState,
}

/// Shared mutable routing data reused across app modes in `route_frame`.
struct RouteSharedContext<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
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
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            current_tab: self.current_tab,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
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
    plan_followups: Option<&'a HashMap<String, PlanFollowup>>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
    sessions: &'a [Session],
    scroll_offset: Option<u16>,
}

/// Shared immutable routing inputs that are not part of list-background state.
#[derive(Clone, Copy)]
struct RouteAuxContext<'a> {
    active_project_id: i64,
    plan_followups: &'a HashMap<String, PlanFollowup>,
    projects: &'a [Project],
    session_progress_messages: &'a HashMap<String, String>,
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        longest_session_duration_seconds,
        mode,
        plan_followups,
        projects,
        session_progress_messages,
        settings,
        stats_activity,
        sessions,
        table_state,
        ..
    } = context;

    let mut shared = RouteSharedContext {
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        longest_session_duration_seconds,
        sessions,
        settings,
        stats_activity,
        table_state,
    };

    let aux = RouteAuxContext {
        active_project_id,
        plan_followups,
        projects,
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
        AppMode::Confirmation { .. } => {
            overlays::render_confirmation_overlay(f, area, mode, shared.list_background());
        }
        AppMode::CommandPalette {
            input,
            selected_index,
            focus,
        } => overlays::render_command_palette(
            f,
            area,
            shared.list_background(),
            *focus,
            input,
            *selected_index,
        ),
        AppMode::CommandOption {
            command,
            selected_index,
        } => overlays::render_command_options(
            f,
            area,
            shared.list_background(),
            *command,
            *selected_index,
            aux.projects,
            aux.active_project_id,
        ),
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
            aux.plan_followups,
            aux.session_progress_messages,
        ),
        AppMode::View { .. } | AppMode::Prompt { .. } | AppMode::Diff { .. } => {
            return false;
        }
    }

    true
}

/// Renders view, prompt, and diff modes that are tied to a selected session.
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
        } => render_session_chat(
            f,
            area,
            SessionChatRenderContext {
                mode,
                plan_followups: Some(aux.plan_followups),
                session_id,
                session_progress_messages: aux.session_progress_messages,
                sessions,
                scroll_offset: *scroll_offset,
            },
        ),
        AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        } => render_session_chat(
            f,
            area,
            SessionChatRenderContext {
                mode,
                plan_followups: None,
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
        | AppMode::CommandPalette { .. }
        | AppMode::CommandOption { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Help { .. } => {}
    }
}

/// Renders the session chat page for view and prompt modes.
fn render_session_chat(f: &mut Frame, area: Rect, context: SessionChatRenderContext<'_>) {
    let SessionChatRenderContext {
        mode,
        plan_followups,
        session_id,
        session_progress_messages,
        sessions,
        scroll_offset,
    } = context;

    let Some(session_index) = sessions.iter().position(|session| session.id == session_id) else {
        return;
    };

    let plan_followup = plan_followups.and_then(|followups| followups.get(session_id));
    let active_progress = session_progress_messages
        .get(session_id)
        .map(std::string::String::as_str);

    pages::session_chat::SessionChatPage::new(
        sessions,
        session_index,
        scroll_offset,
        mode,
        plan_followup,
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
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        longest_session_duration_seconds,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;

    let chunks = Layout::default()
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(content_area);

    components::tab::Tabs::new(current_tab).render(f, chunks[0]);

    match current_tab {
        Tab::Sessions => {
            pages::session_list::SessionListPage::new(sessions, table_state).render(f, chunks[1]);
        }
        Tab::Stats => {
            pages::stats::StatsPage::new(
                sessions,
                stats_activity,
                all_time_model_usage,
                longest_session_duration_seconds,
                codex_usage_limits,
            )
            .render(f, chunks[1]);
        }
        Tab::Settings => {
            pages::settings::SettingsPage::new(settings).render(f, chunks[1]);
        }
    }
}
