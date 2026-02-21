use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::runtime::EventResult;
use crate::ui::state::app_mode::AppMode;
use crate::ui::state::palette::{PaletteCommand, PaletteFocus};

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
        PaletteCommand::Projects => app.projects.len(),
    }
}

async fn handle_command_option_enter(
    app: &mut App,
    _command: PaletteCommand,
    selected_index: usize,
) {
    let Some(project) = app.projects.get(selected_index) else {
        return;
    };

    let _ = app.switch_project(project.id).await;
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::db::Database;

    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(base_path.clone(), base_path, None, database).await;

        (app, base_dir)
    }

    #[tokio::test]
    async fn test_handle_palette_character_updates_input_and_focus() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::CommandPalette {
            input: String::new(),
            selected_index: 3,
            focus: PaletteFocus::Input,
        };

        // Act
        let event_result = handle_palette(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::CommandPalette {
                ref input,
                selected_index: 0,
                focus: PaletteFocus::Dropdown
            } if input == "p"
        ));
    }

    #[tokio::test]
    async fn test_handle_palette_enter_opens_project_options() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::CommandPalette {
            input: "pro".to_string(),
            selected_index: 0,
            focus: PaletteFocus::Dropdown,
        };

        // Act
        let event_result =
            handle_palette(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::CommandOption {
                command: PaletteCommand::Projects,
                selected_index: 0
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_palette_escape_switches_focus_before_exiting() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::CommandPalette {
            input: String::new(),
            selected_index: 0,
            focus: PaletteFocus::Dropdown,
        };

        // Act
        let first_result =
            handle_palette(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let second_result =
            handle_palette(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Assert
        assert!(matches!(first_result, EventResult::Continue));
        assert!(matches!(second_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[test]
    fn test_update_palette_focus_sets_input_focus_when_no_match() {
        // Arrange
        let mut selected_index = 5;
        let mut focus = PaletteFocus::Dropdown;

        // Act
        update_palette_focus("zzz", &mut selected_index, &mut focus);

        // Assert
        assert_eq!(selected_index, 0);
        assert_eq!(focus, PaletteFocus::Input);
    }

    #[test]
    fn test_move_palette_selection_down_moves_index_until_last_entry() {
        // Arrange
        let mut selected_index = 0;
        let mut focus = PaletteFocus::Dropdown;

        // Act
        move_palette_selection_down("", &mut selected_index, &mut focus);

        // Assert
        assert_eq!(selected_index, 0);
        assert_eq!(focus, PaletteFocus::Input);
    }

    #[tokio::test]
    async fn test_handle_option_escape_returns_to_command_palette() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::CommandOption {
            command: PaletteCommand::Projects,
            selected_index: 0,
        };

        // Act
        let event_result = handle_option(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::CommandPalette {
                ref input,
                selected_index: 0,
                focus: PaletteFocus::Dropdown
            } if input.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_option_enter_projects_returns_to_list_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::CommandOption {
            command: PaletteCommand::Projects,
            selected_index: 0,
        };

        // Act
        let event_result =
            handle_option(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .await
                .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_command_option_count_uses_project_count_for_projects_command() {
        // Arrange
        let (app, _base_dir) = new_test_app().await;

        // Act
        let option_count = command_option_count(&app, PaletteCommand::Projects);

        // Assert
        assert_eq!(option_count, app.projects.len());
    }
}
