pub mod components;
pub mod list;
pub mod prompt;
pub mod util;
pub mod view;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::TableState;

use crate::agent::AgentKind;
use crate::model::{AppMode, Session};

pub fn render(
    f: &mut Frame,
    mode: &AppMode,
    sessions: &[Session],
    table_state: &mut TableState,
    agent_kind: AgentKind,
) {
    let area = f.area();

    // Top status bar (all modes)
    let outer_chunks = Layout::default()
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let status_bar_area = outer_chunks[0];
    let content_area = outer_chunks[1];

    components::render_status_bar(f, status_bar_area, agent_kind);

    match mode {
        AppMode::List => list::render(f, content_area, sessions, table_state),
        AppMode::View {
            session_index,
            scroll_offset,
        }
        | AppMode::Reply {
            session_index,
            scroll_offset,
            ..
        } => {
            view::render(
                f,
                content_area,
                sessions,
                *session_index,
                *scroll_offset,
                mode,
            );
        }
        AppMode::Prompt { input } => {
            prompt::render(f, content_area, input);
        }
    }
}
