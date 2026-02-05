pub mod components;
pub mod pages;
pub mod util;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::agent::AgentKind;
use crate::model::{AppMode, Session, Tab};

/// A trait for UI pages that enforces a standard rendering interface.
pub trait Page {
    fn render(&mut self, f: &mut Frame, area: Rect);
}

/// A trait for UI components that enforces a standard rendering interface.
pub trait Component {
    fn render(&self, f: &mut Frame, area: Rect);
}

pub fn render(
    f: &mut Frame,
    mode: &AppMode,
    sessions: &[Session],
    table_state: &mut TableState,
    agent_kind: AgentKind,
    current_tab: Tab,
) {
    let area = f.area();

    // Top status bar (all modes)
    let outer_chunks = Layout::default()
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let status_bar_area = outer_chunks[0];
    let content_area = outer_chunks[1];

    components::status_bar::StatusBar::new(agent_kind).render(f, status_bar_area);

    match mode {
        AppMode::List => {
            // Split content area for tabs and main content
            let chunks = Layout::default()
                .constraints([Constraint::Length(2), Constraint::Min(0)])
                .split(content_area);

            let tabs_area = chunks[0];
            let main_area = chunks[1];

            components::tabs::Tabs::new(current_tab).render(f, tabs_area);

            match current_tab {
                Tab::Sessions => {
                    pages::sessions_list::SessionsListPage::new(sessions, table_state)
                        .render(f, main_area);
                }
                Tab::Roadmap => {
                    pages::roadmap::RoadmapPage.render(f, main_area);
                }
            }
        }
        AppMode::View {
            session_index,
            scroll_offset,
        }
        | AppMode::Reply {
            session_index,
            scroll_offset,
            ..
        } => {
            pages::session_chat::SessionChatPage::new(
                sessions,
                *session_index,
                *scroll_offset,
                mode,
            )
            .render(f, content_area);
        }
        AppMode::Prompt { input } => {
            pages::new_session::NewSessionPage::new(input).render(f, content_area);
        }
    }
}
