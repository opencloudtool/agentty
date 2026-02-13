pub mod components;
pub mod pages;
pub mod util;

use std::path::Path;
use std::sync::{Arc, Mutex};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::session::session_branch;
use crate::health::HealthEntry;
use crate::model::{AppMode, Project, Session, Tab};

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
    pub current_tab: Tab,
    pub git_branch: Option<&'a str>,
    pub git_status: Option<(u32, u32)>,
    pub health_checks: &'a Arc<Mutex<Vec<HealthEntry>>>,
    pub mode: &'a AppMode,
    pub projects: &'a [Project],
    pub show_onboarding: bool,
    pub sessions: &'a [Session],
    pub table_state: &'a mut TableState,
    pub working_dir: &'a Path,
}

pub fn render(f: &mut Frame, context: RenderContext<'_>) {
    let area = f.area();
    if should_render_onboarding(context.mode, context.show_onboarding) {
        let onboarding_chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        pages::onboarding::OnboardingPage.render(f, onboarding_chunks[0]);
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

    components::status_bar::StatusBar.render(f, status_bar_area);
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
        current_tab,
        health_checks,
        mode,
        projects,
        sessions,
        table_state,
        ..
    } = context;

    match mode {
        AppMode::List => {
            render_list_background(f, area, sessions, table_state, current_tab);
        }
        AppMode::View {
            session_id,
            scroll_offset,
        }
        | AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        } => {
            if let Some(session_index) = sessions
                .iter()
                .position(|session| session.id == *session_id)
            {
                pages::session_chat::SessionChatPage::new(
                    sessions,
                    session_index,
                    *scroll_offset,
                    mode,
                )
                .render(f, area);
            }
        }
        AppMode::Diff {
            session_id,
            diff,
            scroll_offset,
        } => {
            if let Some(session) = sessions.iter().find(|session| session.id == *session_id) {
                pages::diff::DiffPage::new(session, diff.clone(), *scroll_offset).render(f, area);
            }
        }
        AppMode::CommandPalette {
            input,
            selected_index,
            focus,
        } => {
            // Render List page as background
            render_list_background(f, area, sessions, table_state, current_tab);

            // Overlay command palette at the bottom
            components::command_palette::CommandPaletteInput::new(input, *selected_index, *focus)
                .render(f, area);
        }
        AppMode::CommandOption {
            command,
            selected_index,
        } => {
            // Render List page as background
            render_list_background(f, area, sessions, table_state, current_tab);

            // Overlay option list at the bottom
            components::command_palette::CommandOptionList::new(
                *command,
                *selected_index,
                projects,
                active_project_id,
            )
            .render(f, area);
        }
        AppMode::Health => {
            pages::health::HealthPage::new(health_checks).render(f, area);
        }
    }
}

/// Returns `true` when the onboarding page should replace the normal UI.
fn should_render_onboarding(mode: &AppMode, show_onboarding: bool) -> bool {
    matches!(mode, AppMode::List) && show_onboarding
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
        AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Diff { session_id, .. } => Some(session_id.as_str()),
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
    sessions: &[Session],
    table_state: &mut TableState,
    current_tab: Tab,
) {
    let chunks = Layout::default()
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(content_area);

    components::tab::Tabs::new(current_tab).render(f, chunks[0]);

    match current_tab {
        Tab::Sessions => {
            pages::session_list::SessionListPage::new(sessions, table_state).render(f, chunks[1]);
        }
        Tab::Stats => {
            pages::stats::StatsPage::new(sessions).render(f, chunks[1]);
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
        let mode = AppMode::Health;
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
