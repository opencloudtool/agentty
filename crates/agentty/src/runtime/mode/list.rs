use std::io;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::model::{AppMode, InputState, PaletteFocus, PromptSlashState};
use crate::runtime::EventResult;

pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Char('q') => return Ok(EventResult::Quit),
        KeyCode::Tab => {
            app.next_tab();
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::CommandPalette {
                input: String::new(),
                selected_index: 0,
                focus: PaletteFocus::Dropdown,
            };
        }
        KeyCode::Char('a') => {
            if let Ok(session_id) = app.create_session().await {
                app.mode = AppMode::Prompt {
                    slash_state: PromptSlashState::new(),
                    session_id,
                    input: InputState::new(),
                    scroll_offset: None,
                };
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.next();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.previous();
        }
        KeyCode::Enter => {
            if let Some(session_index) = app.session_state.table_state.selected()
                && let Some(session_id) = app.session_id_for_index(session_index)
            {
                app.mode = AppMode::View {
                    session_id,
                    scroll_offset: None,
                };
            }
        }
        KeyCode::Char('d') => {
            app.delete_selected_session().await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}
