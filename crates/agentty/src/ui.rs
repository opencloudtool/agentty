pub mod components;
pub mod icon;
pub mod markdown;
pub mod pages;
pub mod state;
pub mod util;

use std::collections::HashMap;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::session::session_branch;
use crate::app::{SettingsManager, Tab};
use crate::domain::permission::PlanFollowup;
use crate::domain::project::Project;
use crate::domain::session::{AllTimeModelUsage, CodexUsageLimits, DailyActivity, Session};
use crate::ui::state::app_mode::{AppMode, HelpContext};
use crate::ui::state::palette::{PaletteCommand, PaletteFocus};

/// A trait for UI pages that enforces a standard rendering interface.
pub trait Page {
    fn render(&mut self, f: &mut Frame, area: Rect);
}

/// A trait for UI components that enforces a standard rendering interface.
pub trait Component {
    fn render(&self, f: &mut Frame, area: Rect);
}

/// Immutable data required to draw a single UI frame.
pub struct RenderContext<'a> {
    pub active_project_id: i64,
    pub all_time_model_usage: &'a [AllTimeModelUsage],
    pub codex_usage_limits: Option<CodexUsageLimits>,
    pub current_tab: Tab,
    pub git_branch: Option<&'a str>,
    pub git_status: Option<(u32, u32)>,
    pub latest_available_version: Option<&'a str>,
    pub longest_session_duration_seconds: u64,
    pub mode: &'a AppMode,
    pub plan_followups: &'a HashMap<String, PlanFollowup>,
    pub projects: &'a [Project],
    pub session_progress_messages: &'a HashMap<String, String>,
    pub settings: &'a mut SettingsManager,
    pub show_onboarding: bool,
    pub stats_activity: &'a [DailyActivity],
    pub sessions: &'a [Session],
    pub table_state: &'a mut TableState,
    pub working_dir: &'a Path,
}

struct SessionChatRenderContext<'a> {
    mode: &'a AppMode,
    plan_followups: Option<&'a HashMap<String, PlanFollowup>>,
    scroll_offset: Option<u16>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
}

struct HelpBackgroundRenderContext<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    context: &'a HelpContext,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
    plan_followups: &'a HashMap<String, PlanFollowup>,
    session_progress_messages: &'a HashMap<String, String>,
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

struct CommandPaletteRenderContext<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    focus: PaletteFocus,
    input: &'a str,
    longest_session_duration_seconds: u64,
    selected_index: usize,
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

struct CommandOptionRenderContext<'a> {
    active_project_id: i64,
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    command: PaletteCommand,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
    projects: &'a [Project],
    selected_index: usize,
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

/// Shared borrowed data required to render list-page backgrounds.
struct ListBackgroundRenderContext<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

struct ListModeRenderContext<'a> {
    active_project_id: i64,
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
    mode: &'a AppMode,
    projects: &'a [Project],
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

/// Shared borrowed data required to render the sync popup overlay.
struct SyncPopupRenderContext<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    is_loading: bool,
    longest_session_duration_seconds: u64,
    message: &'a str,
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
    title: &'a str,
}

struct SessionModeRenderContext<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
    mode: &'a AppMode,
    plan_followups: &'a HashMap<String, PlanFollowup>,
    session_progress_messages: &'a HashMap<String, String>,
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

impl<'a> ListModeRenderContext<'a> {
    /// Converts list mode context into the shared list-background context.
    fn into_list_background(self) -> ListBackgroundRenderContext<'a> {
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

    /// Converts list mode context into command-palette rendering context.
    fn into_command_palette(
        self,
        focus: PaletteFocus,
        input: &'a str,
        selected_index: usize,
    ) -> CommandPaletteRenderContext<'a> {
        CommandPaletteRenderContext {
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            current_tab: self.current_tab,
            focus,
            input,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
            selected_index,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
        }
    }

    /// Converts list mode context into command-option rendering context.
    fn into_command_option(
        self,
        command: PaletteCommand,
        selected_index: usize,
    ) -> CommandOptionRenderContext<'a> {
        CommandOptionRenderContext {
            active_project_id: self.active_project_id,
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            command,
            current_tab: self.current_tab,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
            projects: self.projects,
            selected_index,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
        }
    }

    /// Converts list mode context into sync-popup rendering context.
    fn into_sync_popup(
        self,
        is_loading: bool,
        message: &'a str,
        title: &'a str,
    ) -> SyncPopupRenderContext<'a> {
        SyncPopupRenderContext {
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            current_tab: self.current_tab,
            is_loading,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
            message,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
            title,
        }
    }
}

pub fn render(f: &mut Frame, context: RenderContext<'_>) {
    let area = f.area();
    if should_render_onboarding(context.mode, context.show_onboarding) {
        let onboarding_chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        pages::onboarding::OnboardingPage::new(
            current_version_display_text(),
            context
                .latest_available_version
                .map(std::string::ToString::to_string),
        )
        .render(f, onboarding_chunks[0]);
        render_footer_bar(
            f,
            onboarding_chunks[1],
            context.mode,
            context.sessions,
            context.working_dir,
            context.git_branch,
            context.git_status,
        );

        return;
    }

    // Three-section layout: top status bar, content area, footer bar
    let outer_chunks = Layout::default()
        .constraints([
            Constraint::Length(1), // Top status bar
            Constraint::Min(0),    // Content area
            Constraint::Length(1), // Footer bar
        ])
        .split(area);

    let status_bar_area = outer_chunks[0];
    let content_area = outer_chunks[1];
    let footer_bar_area = outer_chunks[2];

    components::status_bar::StatusBar::new(
        current_version_display_text(),
        context
            .latest_available_version
            .map(std::string::ToString::to_string),
    )
    .render(f, status_bar_area);
    render_footer_bar(
        f,
        footer_bar_area,
        context.mode,
        context.sessions,
        context.working_dir,
        context.git_branch,
        context.git_status,
    );

    render_content(f, content_area, context);
}

/// Renders the main content area based on the current app mode.
fn render_content(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
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

    match mode {
        AppMode::List
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Confirmation { .. }
        | AppMode::CommandPalette { .. }
        | AppMode::CommandOption { .. } => render_list_mode_content(
            f,
            area,
            ListModeRenderContext {
                active_project_id,
                all_time_model_usage,
                codex_usage_limits,
                current_tab,
                longest_session_duration_seconds,
                mode,
                projects,
                sessions,
                settings,
                stats_activity,
                table_state,
            },
        ),
        AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Diff { .. }
        | AppMode::Help { .. } => render_session_mode_content(
            f,
            area,
            SessionModeRenderContext {
                all_time_model_usage,
                codex_usage_limits,
                current_tab,
                longest_session_duration_seconds,
                mode,
                plan_followups,
                session_progress_messages,
                sessions,
                settings,
                stats_activity,
                table_state,
            },
        ),
    }
}

/// Renders all list-oriented content, including overlays and command menus.
fn render_list_mode_content(f: &mut Frame, area: Rect, context: ListModeRenderContext<'_>) {
    match context.mode {
        AppMode::List => render_list_background(f, area, context.into_list_background()),
        AppMode::Confirmation { .. } => {
            render_confirmation_overlay(f, area, context.mode, context.into_list_background());
        }
        AppMode::CommandPalette {
            input,
            selected_index,
            focus,
        } => render_command_palette(
            f,
            area,
            context.into_command_palette(*focus, input, *selected_index),
        ),
        AppMode::CommandOption {
            command,
            selected_index,
        } => render_command_options(
            f,
            area,
            context.into_command_option(*command, *selected_index),
        ),
        AppMode::SyncBlockedPopup {
            is_loading,
            message,
            title,
        } => render_sync_blocked_popup(
            f,
            area,
            context.into_sync_popup(*is_loading, message, title),
        ),
        _ => {}
    }
}

/// Renders the list background and generic confirmation overlay.
fn render_confirmation_overlay(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
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

    render_list_background(
        f,
        area,
        ListBackgroundRenderContext {
            all_time_model_usage,
            codex_usage_limits,
            current_tab,
            longest_session_duration_seconds,
            sessions,
            settings,
            stats_activity,
            table_state,
        },
    );

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
        *selected_confirmation_index == 0,
    )
    .render(f, area);
}

/// Renders the list background and sync informational popup overlay.
fn render_sync_blocked_popup(f: &mut Frame, area: Rect, context: SyncPopupRenderContext<'_>) {
    let SyncPopupRenderContext {
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        is_loading,
        longest_session_duration_seconds,
        message,
        sessions,
        settings,
        stats_activity,
        table_state,
        title,
    } = context;

    render_list_background(
        f,
        area,
        ListBackgroundRenderContext {
            all_time_model_usage,
            codex_usage_limits,
            current_tab,
            longest_session_duration_seconds,
            sessions,
            settings,
            stats_activity,
            table_state,
        },
    );

    components::info_overlay::InfoOverlay::with_loading_state(title, message, is_loading)
        .render(f, area);
}

fn render_session_mode_content(f: &mut Frame, area: Rect, context: SessionModeRenderContext<'_>) {
    let SessionModeRenderContext {
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        longest_session_duration_seconds,
        mode,
        plan_followups,
        session_progress_messages,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;

    match mode {
        AppMode::View {
            session_id,
            scroll_offset,
        } => render_session_chat_mode(
            f,
            area,
            sessions,
            &view_session_chat_context(
                mode,
                plan_followups,
                *scroll_offset,
                session_id,
                session_progress_messages,
            ),
        ),
        AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        } => render_session_chat_mode(
            f,
            area,
            sessions,
            &prompt_session_chat_context(
                mode,
                *scroll_offset,
                session_id,
                session_progress_messages,
            ),
        ),
        AppMode::Diff {
            session_id,
            diff,
            scroll_offset,
            file_explorer_selected_index,
        } => render_diff_mode(
            f,
            area,
            sessions,
            session_id,
            diff,
            *scroll_offset,
            *file_explorer_selected_index,
        ),
        AppMode::Help {
            context,
            scroll_offset,
        } => render_help(
            f,
            area,
            context,
            *scroll_offset,
            HelpBackgroundRenderContext {
                all_time_model_usage,
                codex_usage_limits,
                context,
                current_tab,
                longest_session_duration_seconds,
                plan_followups,
                session_progress_messages,
                sessions,
                settings,
                stats_activity,
                table_state,
            },
        ),
        _ => {}
    }
}

fn view_session_chat_context<'a>(
    mode: &'a AppMode,
    plan_followups: &'a HashMap<String, PlanFollowup>,
    scroll_offset: Option<u16>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
) -> SessionChatRenderContext<'a> {
    SessionChatRenderContext {
        mode,
        plan_followups: Some(plan_followups),
        scroll_offset,
        session_id,
        session_progress_messages,
    }
}

fn prompt_session_chat_context<'a>(
    mode: &'a AppMode,
    scroll_offset: Option<u16>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
) -> SessionChatRenderContext<'a> {
    SessionChatRenderContext {
        mode,
        plan_followups: None,
        scroll_offset,
        session_id,
        session_progress_messages,
    }
}

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

fn render_session_chat_mode(
    f: &mut Frame,
    area: Rect,
    sessions: &[Session],
    context: &SessionChatRenderContext<'_>,
) {
    let mode = context.mode;
    let plan_followups = context.plan_followups;
    let scroll_offset = context.scroll_offset;
    let session_id = context.session_id;
    let session_progress_messages = context.session_progress_messages;

    if let Some(session_index) = sessions.iter().position(|session| session.id == session_id) {
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
}

/// Renders the background page behind the help overlay based on `HelpContext`.
fn render_help_background(f: &mut Frame, area: Rect, context: HelpBackgroundRenderContext<'_>) {
    let HelpBackgroundRenderContext {
        all_time_model_usage,
        codex_usage_limits,
        context,
        current_tab,
        longest_session_duration_seconds,
        plan_followups,
        session_progress_messages,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;
    match context {
        HelpContext::List { .. } => {
            render_list_background(
                f,
                area,
                ListBackgroundRenderContext {
                    all_time_model_usage,
                    codex_usage_limits,
                    current_tab,
                    longest_session_duration_seconds,
                    sessions,
                    settings,
                    stats_activity,
                    table_state,
                },
            );
        }
        HelpContext::View {
            session_id,
            scroll_offset: view_scroll,
            ..
        } => {
            if let Some(session_index) = sessions
                .iter()
                .position(|session| session.id == *session_id)
            {
                let bg_mode = AppMode::View {
                    session_id: session_id.clone(),
                    scroll_offset: *view_scroll,
                };
                let plan_followup = plan_followups.get(session_id);
                let active_progress = session_progress_messages
                    .get(session_id)
                    .map(std::string::String::as_str);
                pages::session_chat::SessionChatPage::new(
                    sessions,
                    session_index,
                    *view_scroll,
                    &bg_mode,
                    plan_followup,
                    active_progress,
                )
                .render(f, area);
            }
        }
        HelpContext::Diff {
            session_id,
            diff,
            scroll_offset: diff_scroll,
            file_explorer_selected_index,
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

fn render_command_palette(f: &mut Frame, area: Rect, context: CommandPaletteRenderContext<'_>) {
    let CommandPaletteRenderContext {
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        focus,
        input,
        longest_session_duration_seconds,
        selected_index,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;
    render_list_background(
        f,
        area,
        ListBackgroundRenderContext {
            all_time_model_usage,
            codex_usage_limits,
            current_tab,
            longest_session_duration_seconds,
            sessions,
            settings,
            stats_activity,
            table_state,
        },
    );
    components::command_palette::CommandPaletteInput::new(input, selected_index, focus)
        .render(f, area);
}

fn render_command_options(f: &mut Frame, area: Rect, context: CommandOptionRenderContext<'_>) {
    let CommandOptionRenderContext {
        active_project_id,
        all_time_model_usage,
        codex_usage_limits,
        command,
        current_tab,
        longest_session_duration_seconds,
        projects,
        selected_index,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;
    render_list_background(
        f,
        area,
        ListBackgroundRenderContext {
            all_time_model_usage,
            codex_usage_limits,
            current_tab,
            longest_session_duration_seconds,
            sessions,
            settings,
            stats_activity,
            table_state,
        },
    );
    components::command_palette::CommandOptionList::new(
        command,
        selected_index,
        projects,
        active_project_id,
    )
    .render(f, area);
}

fn render_help(
    f: &mut Frame,
    area: Rect,
    context: &HelpContext,
    scroll_offset: u16,
    background_context: HelpBackgroundRenderContext<'_>,
) {
    render_help_background(f, area, background_context);
    components::help_overlay::HelpOverlay::new(context, scroll_offset).render(f, area);
}

/// Returns `true` when the onboarding page should replace the normal UI.
fn should_render_onboarding(mode: &AppMode, show_onboarding: bool) -> bool {
    matches!(mode, AppMode::List) && show_onboarding
}

fn current_version_display_text() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Renders the footer bar with directory, branch, and git status info.
fn render_footer_bar(
    f: &mut Frame,
    footer_bar_area: Rect,
    mode: &AppMode,
    sessions: &[Session],
    working_dir: &Path,
    git_branch: Option<&str>,
    git_status: Option<(u32, u32)>,
) {
    let session_id = match mode {
        AppMode::Confirmation {
            session_id: Some(session_id),
            ..
        }
        | AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::Help {
            context: HelpContext::View { session_id, .. } | HelpContext::Diff { session_id, .. },
            ..
        } => Some(session_id.as_str()),
        _ => None,
    };
    let session_for_footer =
        session_id.and_then(|sid| sessions.iter().find(|session| session.id == sid));

    let (footer_dir, footer_branch, footer_status) = match session_for_footer {
        Some(session) => (
            session.folder.to_string_lossy().to_string(),
            Some(session_branch(&session.id)),
            None,
        ),
        None => (
            working_dir.to_string_lossy().to_string(),
            git_branch.map(std::string::ToString::to_string),
            git_status,
        ),
    };

    components::footer_bar::FooterBar::new(footer_dir, footer_branch, footer_status)
        .render(f, footer_bar_area);
}

fn render_list_background(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_render_onboarding_returns_true_for_list_mode() {
        // Arrange
        let mode = AppMode::List;
        let show_onboarding = true;

        // Act
        let should_render = should_render_onboarding(&mode, show_onboarding);

        // Assert
        assert!(should_render);
    }

    #[test]
    fn test_should_render_onboarding_returns_false_for_non_list_mode() {
        // Arrange
        let mode = AppMode::Help {
            context: HelpContext::List {
                keybindings: vec![crate::ui::state::help_action::HelpAction::new(
                    "quit", "q", "Quit",
                )],
            },
            scroll_offset: 0,
        };
        let show_onboarding = true;

        // Act
        let should_render = should_render_onboarding(&mode, show_onboarding);

        // Assert
        assert!(!should_render);
    }

    #[test]
    fn test_should_render_onboarding_returns_false_when_disabled() {
        // Arrange
        let mode = AppMode::List;
        let show_onboarding = false;

        // Act
        let should_render = should_render_onboarding(&mode, show_onboarding);

        // Assert
        assert!(!should_render);
    }
}
