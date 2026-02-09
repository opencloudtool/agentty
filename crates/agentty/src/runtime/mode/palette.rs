use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::model::{AppMode, PaletteCommand, PaletteFocus};
use crate::runtime::EventResult;

enum PaletteAction {
    None,
    Open(PaletteCommand),
}

pub(crate) fn handle_palette(app: &mut App, key: KeyEvent) -> EventResult {
    let mut palette_action = PaletteAction::None;
    if let AppMode::CommandPalette {
        input,
        selected_index,
        focus,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                app.mode = AppMode::List;
            }
            KeyCode::Char(character) => {
                input.push(character);
                update_palette_focus(input, selected_index, focus);
            }
            KeyCode::Backspace => {
                input.pop();
                update_palette_focus(input, selected_index, focus);
            }
            KeyCode::Up if *focus == PaletteFocus::Dropdown => {
                *selected_index = selected_index.saturating_sub(1);
            }
            KeyCode::Down if *focus == PaletteFocus::Dropdown => {
                move_palette_selection_down(input, selected_index, focus);
            }
            KeyCode::Enter if *focus == PaletteFocus::Dropdown => {
                let filtered = PaletteCommand::filter(input);
                if let Some(&command) = filtered.get(*selected_index) {
                    palette_action = PaletteAction::Open(command);
                }
            }
            KeyCode::Esc => {
                if *focus == PaletteFocus::Dropdown {
                    *focus = PaletteFocus::Input;
                } else {
                    app.mode = AppMode::List;
                }
            }
            _ => {}
        }
    }

    match palette_action {
        PaletteAction::None => {}
        PaletteAction::Open(PaletteCommand::Projects) => {
            app.mode = AppMode::CommandOption {
                command: PaletteCommand::Projects,
                selected_index: 0,
            };
        }
        PaletteAction::Open(PaletteCommand::Health) => {
            app.start_health_checks();
            app.mode = AppMode::Health;
        }
    }

    EventResult::Continue
}

fn update_palette_focus(input: &str, selected_index: &mut usize, focus: &mut PaletteFocus) {
    let filtered = PaletteCommand::filter(input);
    *selected_index = 0;
    *focus = if filtered.is_empty() {
        PaletteFocus::Input
    } else {
        PaletteFocus::Dropdown
    };
}

fn move_palette_selection_down(input: &str, selected_index: &mut usize, focus: &mut PaletteFocus) {
    let filtered = PaletteCommand::filter(input);
    if filtered.is_empty() {
        *focus = PaletteFocus::Input;

        return;
    }
    if *selected_index >= filtered.len().saturating_sub(1) {
        *focus = PaletteFocus::Input;
    } else {
        *selected_index += 1;
    }
}

pub(crate) async fn handle_option(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    let (command, mut selected_index) = match &app.mode {
        AppMode::CommandOption {
            command,
            selected_index,
        } => (*command, *selected_index),
        _ => return Ok(EventResult::Continue),
    };

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let option_count = command_option_count(app, command);
            if option_count > 0 {
                selected_index = (selected_index + 1).min(option_count - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            selected_index = selected_index.saturating_sub(1);
        }
        KeyCode::Enter => {
            handle_command_option_enter(app, command, selected_index).await;
            app.mode = AppMode::List;
        }
        KeyCode::Esc => {
            app.mode = AppMode::CommandPalette {
                input: String::new(),
                selected_index: 0,
                focus: PaletteFocus::Dropdown,
            };
        }
        _ => {}
    }

    if let AppMode::CommandOption {
        selected_index: mode_selected_index,
        ..
    } = &mut app.mode
    {
        *mode_selected_index = selected_index;
    }

    Ok(EventResult::Continue)
}

fn command_option_count(app: &App, command: PaletteCommand) -> usize {
    match command {
        PaletteCommand::Health => 0,
        PaletteCommand::Projects => app.projects.len(),
    }
}

async fn handle_command_option_enter(
    app: &mut App,
    command: PaletteCommand,
    selected_index: usize,
) {
    if command != PaletteCommand::Projects {
        return;
    }
    let Some(project) = app.projects.get(selected_index) else {
        return;
    };

    let _ = app.switch_project(project.id).await;
}
