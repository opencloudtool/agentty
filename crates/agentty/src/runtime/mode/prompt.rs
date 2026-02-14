use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::agent::AgentKind;
use crate::app::App;
use crate::model::{AppMode, PromptSlashStage};
use crate::runtime::{EventResult, TuiTerminal};
use crate::ui::util::{move_input_cursor_down, move_input_cursor_up};

struct PromptContext {
    is_new_session: bool,
    is_slash_command: bool,
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

pub(crate) async fn handle(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
) -> io::Result<EventResult> {
    let Some(prompt_context) = prompt_context(app) else {
        return Ok(EventResult::Continue);
    };

    if !prompt_context.is_slash_command {
        reset_prompt_slash_state(app);
    }

    match key.code {
        KeyCode::Enter if should_insert_newline(key) => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.insert_newline();
            }
        }
        KeyCode::Enter => {
            handle_prompt_submit_key(app, &prompt_context).await;
        }
        KeyCode::Esc | KeyCode::Char('c') if is_prompt_cancel_key(key) => {
            handle_prompt_cancel_key(app, &prompt_context).await;
        }
        KeyCode::Left => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_left();
            }
        }
        KeyCode::Right => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_right();
            }
        }
        KeyCode::Up => {
            handle_prompt_up_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Down => {
            handle_prompt_down_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Char('k') if prompt_context.is_slash_command && is_plain_char_key(key, 'k') => {
            handle_prompt_up_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Char('j') if prompt_context.is_slash_command && is_plain_char_key(key, 'j') => {
            handle_prompt_down_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Home => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_home();
            }
        }
        KeyCode::End => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_end();
            }
        }
        KeyCode::Backspace => {
            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.delete_backward();
                slash_state.reset();
            }
        }
        KeyCode::Delete => {
            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.delete_forward();
                slash_state.reset();
            }
        }
        KeyCode::Char(character) => {
            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.insert_char(character);
                slash_state.reset();
            }
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

fn prompt_context(app: &mut App) -> Option<PromptContext> {
    let (is_slash_command, scroll_offset, session_id) = match &app.mode {
        AppMode::Prompt {
            input,
            scroll_offset,
            session_id,
            ..
        } => (
            input.text().starts_with('/'),
            *scroll_offset,
            session_id.clone(),
        ),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    let is_new_session = app
        .session_state
        .sessions
        .get(session_index)
        .is_some_and(|session| session.prompt.is_empty());

    Some(PromptContext {
        is_new_session,
        is_slash_command,
        scroll_offset,
        session_id,
        session_index,
    })
}

fn reset_prompt_slash_state(app: &mut App) {
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.reset();
    }
}

fn is_prompt_cancel_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc || key.modifiers.contains(event::KeyModifiers::CONTROL)
}

fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == event::KeyModifiers::NONE
}

fn handle_prompt_up_key(
    app: &mut App,
    terminal: &TuiTerminal,
    prompt_context: &PromptContext,
) -> io::Result<()> {
    if prompt_context.is_slash_command {
        if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
            slash_state.selected_index = slash_state.selected_index.saturating_sub(1);
        }

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = move_input_cursor_up(input.text(), input_width, input.cursor);
    }

    Ok(())
}

fn handle_prompt_down_key(
    app: &mut App,
    terminal: &TuiTerminal,
    prompt_context: &PromptContext,
) -> io::Result<()> {
    if prompt_context.is_slash_command {
        advance_prompt_slash_selection(app);

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = move_input_cursor_down(input.text(), input_width, input.cursor);
    }

    Ok(())
}

fn advance_prompt_slash_selection(app: &mut App) {
    let (input_text, selected_agent, selected_index, stage) = match &app.mode {
        AppMode::Prompt {
            input, slash_state, ..
        } => (
            input.text().to_string(),
            slash_state.selected_agent,
            slash_state.selected_index,
            slash_state.stage,
        ),
        _ => return,
    };

    let option_count = prompt_slash_option_count(&input_text, stage, selected_agent);
    if option_count == 0 {
        return;
    }

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        let max_index = option_count.saturating_sub(1);
        slash_state.selected_index = (selected_index + 1).min(max_index);
    }
}

async fn handle_prompt_submit_key(app: &mut App, prompt_context: &PromptContext) {
    if prompt_context.is_slash_command {
        handle_prompt_slash_submit(app, prompt_context);

        return;
    }

    let prompt = match &mut app.mode {
        AppMode::Prompt { input, .. } => input.take_text(),
        _ => String::new(),
    };
    if prompt.is_empty() {
        return;
    }

    if prompt_context.is_new_session {
        if let Err(error) = app.start_session(&prompt_context.session_id, prompt).await {
            append_output_for_session(
                app,
                &prompt_context.session_id,
                &format!("\n[Error] {error}\n"),
            )
            .await;
        }
    } else {
        app.reply(&prompt_context.session_id, &prompt);
    }

    app.mode = AppMode::View {
        session_id: prompt_context.session_id.clone(),
        scroll_offset: None,
    };
}

fn handle_prompt_slash_submit(app: &mut App, prompt_context: &PromptContext) {
    let (input_text, selected_agent, selected_index, stage) = match &app.mode {
        AppMode::Prompt {
            input, slash_state, ..
        } => (
            input.text().to_string(),
            slash_state.selected_agent,
            slash_state.selected_index,
            slash_state.stage,
        ),
        _ => return,
    };

    match stage {
        PromptSlashStage::Command => {
            let commands = prompt_slash_commands(&input_text);
            if commands.is_empty() {
                return;
            }

            if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                slash_state.stage = PromptSlashStage::Agent;
                slash_state.selected_agent = None;
                slash_state.selected_index = 0;
            }
        }
        PromptSlashStage::Agent => {
            let Some(selected_agent) = AgentKind::ALL.get(selected_index).copied() else {
                return;
            };

            if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                slash_state.selected_agent = Some(selected_agent);
                slash_state.stage = PromptSlashStage::Model;
                slash_state.selected_index = 0;
            }
        }
        PromptSlashStage::Model => {
            let fallback_agent = app
                .session_state
                .sessions
                .get(prompt_context.session_index)
                .and_then(|session| session.agent.parse::<AgentKind>().ok())
                .unwrap_or(AgentKind::Gemini);
            let selected_agent = selected_agent.unwrap_or(fallback_agent);
            let Some(selected_model) = selected_agent.models().get(selected_index).copied() else {
                return;
            };

            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.take_text();
                slash_state.reset();
            }

            let _ = app.set_session_agent_and_model(
                &prompt_context.session_id,
                selected_agent,
                selected_model,
            );
        }
    }
}

async fn handle_prompt_cancel_key(app: &mut App, prompt_context: &PromptContext) {
    if prompt_context.is_slash_command {
        if let AppMode::Prompt {
            input, slash_state, ..
        } = &mut app.mode
        {
            input.take_text();
            slash_state.reset();
        }

        return;
    }

    if prompt_context.is_new_session {
        app.delete_selected_session().await;
        app.mode = AppMode::List;

        return;
    }

    app.mode = AppMode::View {
        session_id: prompt_context.session_id.clone(),
        scroll_offset: prompt_context.scroll_offset,
    };
}

async fn append_output_for_session(app: &App, session_id: &str, output: &str) {
    app.append_output_for_session(session_id, output).await;
}

fn prompt_slash_commands(input: &str) -> Vec<&'static str> {
    let lowered = input.to_lowercase();
    let mut commands = vec!["/model"];
    commands.retain(|command| command.starts_with(&lowered));

    commands
}

fn prompt_slash_option_count(
    input: &str,
    stage: PromptSlashStage,
    selected_agent: Option<AgentKind>,
) -> usize {
    match stage {
        PromptSlashStage::Command => prompt_slash_commands(input).len(),
        PromptSlashStage::Agent => AgentKind::ALL.len(),
        PromptSlashStage::Model => selected_agent.unwrap_or(AgentKind::Gemini).models().len(),
    }
}

fn should_insert_newline(key: KeyEvent) -> bool {
    is_enter_key(key.code) && key.modifiers.contains(event::KeyModifiers::ALT)
}

fn is_enter_key(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Enter | KeyCode::Char('\r' | '\n'))
}

fn prompt_input_width(terminal: &TuiTerminal) -> io::Result<u16> {
    let terminal_width = terminal.size()?.width;

    Ok(terminal_width.saturating_sub(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_should_insert_newline_for_alt_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::ALT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_alt_shift_enter() {
        // Arrange
        let key = KeyEvent::new(
            KeyCode::Enter,
            event::KeyModifiers::ALT | event::KeyModifiers::SHIFT,
        );

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_alt_carriage_return() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\r'), event::KeyModifiers::ALT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_alt_line_feed() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\n'), event::KeyModifiers::ALT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_not_insert_newline_for_plain_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::NONE);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_shift_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_shift_carriage_return() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\r'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_shift_line_feed() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\n'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_control_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::CONTROL);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_non_enter_key() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('x'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_enter_key_for_enter() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Enter);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_enter_key_for_carriage_return() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Char('\r'));

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_enter_key_for_line_feed() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Char('\n'));

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_enter_key_for_other_key() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Char('x'));

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_plain_char_key_for_plain_character() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

        // Act
        let result = is_plain_char_key(key, 'j');

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_plain_char_key_rejects_modifier_keys() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('k'), event::KeyModifiers::SHIFT);

        // Act
        let result = is_plain_char_key(key, 'k');

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_plain_char_key_rejects_other_character() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

        // Act
        let result = is_plain_char_key(key, 'k');

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_prompt_slash_commands_match_model() {
        // Arrange & Act
        let commands = prompt_slash_commands("/m");

        // Assert
        assert_eq!(commands, vec!["/model"]);
    }

    #[test]
    fn test_prompt_slash_commands_no_match() {
        // Arrange & Act
        let commands = prompt_slash_commands("/x");

        // Assert
        assert!(commands.is_empty());
    }

    #[test]
    fn test_prompt_slash_option_count_for_agent_stage() {
        // Arrange & Act
        let count = prompt_slash_option_count("/model", PromptSlashStage::Agent, None);

        // Assert
        assert_eq!(count, AgentKind::ALL.len());
    }

    #[test]
    fn test_prompt_slash_option_count_for_model_stage() {
        // Arrange & Act
        let count =
            prompt_slash_option_count("/model", PromptSlashStage::Model, Some(AgentKind::Claude));

        // Assert
        assert_eq!(count, AgentKind::Claude.models().len());
    }
}
