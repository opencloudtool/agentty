pub mod components;
pub mod pages;
pub mod util;

use std::path::Path;
use std::sync::{Arc, Mutex};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

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

pub struct RenderContext<'a> {
    pub active_project_id: i64,
    pub current_tab: Tab,
    pub git_branch: Option<&'a str>,
    pub git_status: Option<(u32, u32)>,
    pub health_checks: &'a Arc<Mutex<Vec<HealthEntry>>>,
    pub mode: &'a AppMode,
    pub projects: &'a [Project],
    pub sessions: &'a [Session],
    pub table_state: &'a mut TableState,
    pub working_dir: &'a Path,
}

pub fn render(f: &mut Frame, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        current_tab,
        git_branch,
        git_status,
        health_checks,
        mode,
        projects,
        sessions,
        table_state,
        working_dir,
    } = context;

    let area = f.area();

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

    components::footer_bar::FooterBar::new(
        working_dir.to_string_lossy().to_string(),
        git_branch.map(std::string::ToString::to_string),
        git_status,
    )
    .render(f, footer_bar_area);

    match mode {
        AppMode::List => {
            render_list_background(f, content_area, sessions, table_state, current_tab);
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
                .render(f, content_area);
            }
        }
        AppMode::Diff {
            session_id,
            diff,
            scroll_offset,
        } => {
            if let Some(session) = sessions.iter().find(|session| session.id == *session_id) {
                pages::diff::DiffPage::new(session, diff.clone(), *scroll_offset)
                    .render(f, content_area);
            }
        }
        AppMode::CommandPalette {
            input,
            selected_index,
            focus,
        } => {
            // Render List page as background
            render_list_background(f, content_area, sessions, table_state, current_tab);

            // Overlay command palette at the bottom
            components::command_palette::CommandPaletteInput::new(input, *selected_index, *focus)
                .render(f, content_area);
        }
        AppMode::CommandOption {
            command,
            selected_index,
        } => {
            // Render List page as background
            render_list_background(f, content_area, sessions, table_state, current_tab);

            // Overlay option list at the bottom
            components::command_palette::CommandOptionList::new(
                *command,
                *selected_index,
                projects,
                active_project_id,
            )
            .render(f, content_area);
        }
        AppMode::Health => {
            pages::health::HealthPage::new(health_checks).render(f, content_area);
        }
    }
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
        Tab::Roadmap => {
            pages::roadmap::RoadmapPage.render(f, chunks[1]);
        }
    }
}
