use std::collections::HashMap;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::session::session_branch;
use crate::app::{ProjectSwitcherItem, SettingsManager, Tab};
use crate::domain::project::ProjectListItem;
use crate::domain::session::{AllTimeModelUsage, CodexUsageLimits, DailyActivity, Session};
use crate::ui::state::app_mode::{AppMode, HelpContext};
use crate::ui::{components, router};

/// A trait for UI pages that enforces a standard rendering interface.
pub trait Page {
    /// Renders a page in the provided frame and area.
    fn render(&mut self, f: &mut Frame, area: Rect);
}

/// A trait for UI components that enforces a standard rendering interface.
pub trait Component {
    /// Renders a component in the provided frame and area.
    fn render(&self, f: &mut Frame, area: Rect);
}

/// Immutable data required to draw a single UI frame.
pub struct RenderContext<'a> {
    pub all_time_model_usage: &'a [AllTimeModelUsage],
    pub codex_usage_limits: Option<CodexUsageLimits>,
    pub current_tab: Tab,
    pub git_branch: Option<&'a str>,
    pub git_status: Option<(u32, u32)>,
    pub latest_available_version: Option<&'a str>,
    pub longest_session_duration_seconds: u64,
    pub mode: &'a AppMode,
    pub project_table_state: &'a mut TableState,
    pub project_switcher_items: &'a [ProjectSwitcherItem],
    pub projects: &'a [ProjectListItem],
    pub session_progress_messages: &'a HashMap<String, String>,
    pub settings: &'a mut SettingsManager,
    pub stats_activity: &'a [DailyActivity],
    pub sessions: &'a [Session],
    pub table_state: &'a mut TableState,
    pub working_dir: &'a Path,
}

/// Renders a complete frame including status bar, content area, and footer.
pub fn render(f: &mut Frame, context: RenderContext<'_>) {
    let area = f.area();
    let outer_chunks = Layout::default()
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let status_bar_area = outer_chunks[0];
    let content_area = outer_chunks[1];
    let footer_bar_area = outer_chunks[2];

    components::status_bar::StatusBar::new(current_version_display_text())
        .latest_available_version(
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

    router::route_frame(f, content_area, context);
}

/// Returns the current app version as displayed in the status bar.
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
    let session_for_footer = session_id.and_then(|session_identifier| {
        sessions
            .iter()
            .find(|session| session.id == session_identifier)
    });

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

    components::footer_bar::FooterBar::new(footer_dir)
        .git_branch(footer_branch)
        .git_status(footer_status)
        .render(f, footer_bar_area);
}
