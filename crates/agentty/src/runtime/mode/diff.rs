use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::model::AppMode;
use crate::runtime::EventResult;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    if let AppMode::Diff {
        session_id,
        diff: _,
        scroll_offset,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                app.mode = AppMode::View {
                    session_id: session_id.clone(),
                    scroll_offset: None,
                };
            }
            KeyCode::Char('j') | KeyCode::Down => {
                *scroll_offset = scroll_offset.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *scroll_offset = scroll_offset.saturating_sub(1);
            }
            _ => {}
        }
    }

    EventResult::Continue
}
