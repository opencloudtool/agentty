use crossterm::event::{KeyCode, KeyEvent};

const YES_OPTION_INDEX: usize = 0;
const NO_OPTION_INDEX: usize = 1;

/// Describes how a confirmation selector should react to a pressed key.
pub(crate) enum ConfirmationDecision {
    Confirm,
    Cancel,
    Continue,
}

/// Handles shared confirmation keys (`y/n/q`, arrows, `h/l`, `Esc`, `Enter`)
/// for a yes/no confirmation selector.
pub(crate) fn handle(
    selected_confirmation_index: &mut usize,
    key: KeyEvent,
) -> ConfirmationDecision {
    match key.code {
        KeyCode::Char(character) if is_yes_shortcut(character) => ConfirmationDecision::Confirm,
        KeyCode::Char(character) if is_no_shortcut(character) => ConfirmationDecision::Cancel,
        KeyCode::Esc => ConfirmationDecision::Cancel,
        KeyCode::Left => {
            *selected_confirmation_index = selected_confirmation_index.saturating_sub(1);

            ConfirmationDecision::Continue
        }
        KeyCode::Char(character) if is_left_shortcut(character) => {
            *selected_confirmation_index = selected_confirmation_index.saturating_sub(1);

            ConfirmationDecision::Continue
        }
        KeyCode::Right => {
            *selected_confirmation_index = (*selected_confirmation_index + 1).min(NO_OPTION_INDEX);

            ConfirmationDecision::Continue
        }
        KeyCode::Char(character) if is_right_shortcut(character) => {
            *selected_confirmation_index = (*selected_confirmation_index + 1).min(NO_OPTION_INDEX);

            ConfirmationDecision::Continue
        }
        KeyCode::Enter => {
            if *selected_confirmation_index == YES_OPTION_INDEX {
                ConfirmationDecision::Confirm
            } else {
                ConfirmationDecision::Cancel
            }
        }
        _ => ConfirmationDecision::Continue,
    }
}

/// Returns whether the pressed key should confirm the action.
fn is_yes_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'y')
}

/// Returns whether the pressed key should cancel the action.
fn is_no_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'n') || character.eq_ignore_ascii_case(&'q')
}

/// Returns whether the pressed key should move selection to the left option.
fn is_left_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'h')
}

/// Returns whether the pressed key should move selection to the right option.
fn is_right_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'l')
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;

    #[test]
    fn test_handle_returns_confirm_for_yes_shortcut() {
        // Arrange
        let mut selected_confirmation_index = NO_OPTION_INDEX;

        // Act
        let decision = handle(
            &mut selected_confirmation_index,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(decision, ConfirmationDecision::Confirm));
    }

    #[test]
    fn test_handle_returns_cancel_for_no_shortcut() {
        // Arrange
        let mut selected_confirmation_index = YES_OPTION_INDEX;

        // Act
        let decision = handle(
            &mut selected_confirmation_index,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(decision, ConfirmationDecision::Cancel));
    }

    #[test]
    fn test_handle_returns_cancel_for_escape() {
        // Arrange
        let mut selected_confirmation_index = YES_OPTION_INDEX;

        // Act
        let decision = handle(
            &mut selected_confirmation_index,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(decision, ConfirmationDecision::Cancel));
    }

    #[test]
    fn test_handle_updates_selection_with_arrow_keys() {
        // Arrange
        let mut selected_confirmation_index = YES_OPTION_INDEX;

        // Act
        let move_right_decision = handle(
            &mut selected_confirmation_index,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        );
        let move_left_decision = handle(
            &mut selected_confirmation_index,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            move_right_decision,
            ConfirmationDecision::Continue
        ));
        assert!(matches!(move_left_decision, ConfirmationDecision::Continue));
        assert_eq!(selected_confirmation_index, YES_OPTION_INDEX);
    }

    #[test]
    fn test_handle_updates_selection_with_h_and_l_shortcuts() {
        // Arrange
        let mut selected_confirmation_index = YES_OPTION_INDEX;

        // Act
        let move_right_decision = handle(
            &mut selected_confirmation_index,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        let move_left_decision = handle(
            &mut selected_confirmation_index,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            move_right_decision,
            ConfirmationDecision::Continue
        ));
        assert!(matches!(move_left_decision, ConfirmationDecision::Continue));
        assert_eq!(selected_confirmation_index, YES_OPTION_INDEX);
    }

    #[test]
    fn test_handle_enter_uses_selected_option() {
        // Arrange
        let mut yes_selected_index = YES_OPTION_INDEX;
        let mut no_selected_index = NO_OPTION_INDEX;

        // Act
        let yes_decision = handle(
            &mut yes_selected_index,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        let no_decision = handle(
            &mut no_selected_index,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(yes_decision, ConfirmationDecision::Confirm));
        assert!(matches!(no_decision, ConfirmationDecision::Cancel));
    }
}
