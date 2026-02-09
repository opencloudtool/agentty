use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::model::AppMode;
use crate::runtime::EventResult;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('r') => {
            app.start_health_checks();
        }
        _ => {}
    }

    EventResult::Continue
}
