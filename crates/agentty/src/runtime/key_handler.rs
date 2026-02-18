use std::io;

use crossterm::event::KeyEvent;

use crate::app::App;
use crate::model::AppMode;
use crate::runtime::{EventResult, TuiTerminal, mode};

pub(crate) async fn handle_key_event(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
) -> io::Result<EventResult> {
    match &app.mode {
        AppMode::List => mode::list::handle(app, key).await,
        AppMode::ConfirmDeleteSession { .. } => mode::delete_confirmation::handle(app, key).await,
        AppMode::View { .. } => mode::view::handle(app, terminal, key).await,
        AppMode::Prompt { .. } => mode::prompt::handle(app, terminal, key).await,
        AppMode::Diff { .. } => Ok(mode::diff::handle(app, key)),
        AppMode::CommandPalette { .. } => Ok(mode::palette::handle_palette(app, key)),
        AppMode::CommandOption { .. } => mode::palette::handle_option(app, key).await,
        AppMode::Help { .. } => Ok(mode::help::handle(app, key)),
    }
}
