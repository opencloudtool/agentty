//! Shared input key handling utilities used by both prompt and question modes.
//!
//! Contains modifier predicates, cursor position queries, word-based cursor
//! movement, word deletion, and text normalization that are common across
//! text-input modes.

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::domain::input::InputState;

// ---------------------------------------------------------------------------
// Modifier predicates
// ---------------------------------------------------------------------------

/// Returns true when `Ctrl` is pressed without `Alt` or `Shift`.
///
/// macOS terminals send `Ctrl+a` (`\x01`) for `Cmd+Left` and `Ctrl+e`
/// (`\x05`) for `Cmd+Right` because the legacy terminal protocol cannot
/// encode the Super/Cmd modifier.
pub(crate) fn is_control_key(key: KeyEvent) -> bool {
    key.modifiers == event::KeyModifiers::CONTROL
}

/// Returns true when the `Alt` modifier is present.
///
/// macOS terminals report `Option`+key as `Alt`+key. `Option`+`Left` sends
/// `ESC b` (parsed as `Alt+b`) and `Option`+`Right` sends `ESC f` (parsed
/// as `Alt+f`).
pub(crate) fn is_alt_key(key: KeyEvent) -> bool {
    key.modifiers.contains(event::KeyModifiers::ALT)
}

/// Returns true when backspace should delete the previous word instead of a
/// single character.
///
/// `Option`+`Backspace` is reported as `Alt` on macOS terminals. `Shift` is
/// also accepted for backward compatibility with existing behavior.
/// `Cmd`+`Backspace` is handled separately as a whole-line deletion shortcut.
pub(crate) fn is_word_delete_backspace(key: KeyEvent) -> bool {
    key.modifiers
        .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT)
}

/// Returns true when backspace should delete the current line content.
///
/// On macOS terminals this is produced by pressing `Cmd`+`Backspace`.
pub(crate) fn is_line_delete_backspace(key: KeyEvent) -> bool {
    key.modifiers.contains(event::KeyModifiers::SUPER)
}

/// Returns whether one key event inserts its character into input.
///
/// Only plain keys (no modifier) and `Shift`+key produce insertable
/// characters.
pub(crate) fn is_insertable_char_key(key: KeyEvent) -> bool {
    matches!(
        key.modifiers,
        event::KeyModifiers::NONE | event::KeyModifiers::SHIFT
    )
}

// ---------------------------------------------------------------------------
// Enter / newline predicates
// ---------------------------------------------------------------------------

/// Returns whether an Enter-like key event should insert a newline into the
/// input.
///
/// Both `Alt+Enter` and `Shift+Enter` are accepted so newline entry remains
/// portable across terminals that emit either modifier for multiline editing.
pub(crate) fn should_insert_newline(key: KeyEvent) -> bool {
    is_enter_key(key.code)
        && key
            .modifiers
            .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT)
}

/// Returns true when the key code represents an Enter key press.
///
/// Some terminals encode Enter as `\r` or `\n` character events rather than
/// `KeyCode::Enter`.
pub(crate) fn is_enter_key(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Enter | KeyCode::Char('\r' | '\n'))
}

/// Returns true when the key event represents a control-key newline variant
/// such as `Ctrl+j` or `Ctrl+m`.
pub(crate) fn is_control_newline_key(key: KeyEvent, character: char) -> bool {
    key.modifiers == event::KeyModifiers::CONTROL && matches!(character, 'j' | 'm' | '\n' | '\r')
}

// ---------------------------------------------------------------------------
// Cursor position queries
// ---------------------------------------------------------------------------

/// Returns whether the input cursor is on the first line of text.
///
/// True when no newline characters appear before the cursor position,
/// including when the input is empty.
pub(crate) fn is_cursor_on_first_line(input: &InputState) -> bool {
    input.text().chars().take(input.cursor).all(|ch| ch != '\n')
}

/// Returns whether the input cursor is on the last line of text.
///
/// True when no newline characters appear after the cursor position,
/// including when the input is empty.
pub(crate) fn is_cursor_on_last_line(input: &InputState) -> bool {
    input.text().chars().skip(input.cursor).all(|ch| ch != '\n')
}

// ---------------------------------------------------------------------------
// Word-based cursor movement
// ---------------------------------------------------------------------------

/// Moves the cursor to the start of the previous word, skipping adjacent
/// whitespace separators.
pub(crate) fn move_cursor_word_left(input: &mut InputState) {
    if input.cursor == 0 {
        return;
    }

    let characters: Vec<char> = input.text().chars().collect();
    let mut cursor = input.cursor;

    while cursor > 0 && characters[cursor - 1].is_whitespace() {
        cursor -= 1;
    }

    while cursor > 0 && !characters[cursor - 1].is_whitespace() {
        cursor -= 1;
    }

    input.cursor = cursor;
}

/// Moves the cursor to the start of the next word, skipping adjacent
/// whitespace separators.
pub(crate) fn move_cursor_word_right(input: &mut InputState) {
    let characters: Vec<char> = input.text().chars().collect();
    let mut cursor = input.cursor;

    while cursor < characters.len() && !characters[cursor].is_whitespace() {
        cursor += 1;
    }

    while cursor < characters.len() && characters[cursor].is_whitespace() {
        cursor += 1;
    }

    input.cursor = cursor;
}

// ---------------------------------------------------------------------------
// Word-based deletion
// ---------------------------------------------------------------------------

/// Returns the character range for deleting the previous word plus adjacent
/// separator whitespace.
///
/// The three-step backward scan skips trailing whitespace, then the word body,
/// then leading whitespace before the word. Returns `None` when the cursor is
/// already at position zero.
pub(crate) fn word_delete_range(text: &str, cursor: usize) -> Option<(usize, usize)> {
    if cursor == 0 {
        return None;
    }

    let characters: Vec<char> = text.chars().collect();
    let mut start = cursor;

    while start > 0 && characters[start - 1].is_whitespace() {
        start -= 1;
    }

    while start > 0 && !characters[start - 1].is_whitespace() {
        start -= 1;
    }

    while start > 0 && characters[start - 1].is_whitespace() {
        start -= 1;
    }

    Some((start, cursor))
}

/// Deletes the previous word and any preceding whitespace from the input.
///
/// Matches the `Ctrl+w` / `Alt+Backspace` behavior: skip trailing whitespace,
/// skip the word body, then skip leading whitespace before the word.
pub(crate) fn delete_word_backward(input: &mut InputState) {
    if let Some((start, end)) = word_delete_range(input.text(), input.cursor) {
        input.replace_range(start, end, "");
    }
}

// ---------------------------------------------------------------------------
// Text normalization
// ---------------------------------------------------------------------------

/// Normalizes pasted text line endings to `\n`.
///
/// Replaces `\r\n` (Windows) and standalone `\r` (classic Mac) with `\n`.
pub(crate) fn normalize_pasted_text(pasted_text: &str) -> String {
    let mut normalized_text = String::with_capacity(pasted_text.len());
    let mut characters = pasted_text.chars().peekable();

    while let Some(character) = characters.next() {
        if character == '\r' {
            if matches!(characters.peek(), Some(&'\n')) {
                // Consume the trailing `\n` from a `\r\n` sequence.
                let _ = characters.next();
            }

            normalized_text.push('\n');

            continue;
        }

        normalized_text.push(character);
    }

    normalized_text
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // should_insert_newline
    // -----------------------------------------------------------------------

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
    fn test_should_insert_newline_for_shift_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_shift_carriage_return() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\r'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_shift_line_feed() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\n'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
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

    // -----------------------------------------------------------------------
    // is_enter_key
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // is_control_newline_key
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_control_newline_key_accepts_ctrl_j() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::CONTROL);

        // Act
        let result = is_control_newline_key(key, 'j');

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_control_newline_key_accepts_ctrl_m() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('m'), event::KeyModifiers::CONTROL);

        // Act
        let result = is_control_newline_key(key, 'm');

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_control_newline_key_rejects_plain_j() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

        // Act
        let result = is_control_newline_key(key, 'j');

        // Assert
        assert!(!result);
    }

    // -----------------------------------------------------------------------
    // is_word_delete_backspace / is_line_delete_backspace
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_word_delete_backspace_accepts_alt_modifier() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::ALT);

        // Act
        let result = is_word_delete_backspace(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_word_delete_backspace_accepts_shift_modifier() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::SHIFT);

        // Act
        let result = is_word_delete_backspace(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_word_delete_backspace_rejects_plain_backspace() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::NONE);

        // Act
        let result = is_word_delete_backspace(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_line_delete_backspace_accepts_super_modifier() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::SUPER);

        // Act
        let result = is_line_delete_backspace(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_line_delete_backspace_rejects_plain_backspace() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::NONE);

        // Act
        let result = is_line_delete_backspace(key);

        // Assert
        assert!(!result);
    }

    // -----------------------------------------------------------------------
    // is_control_key
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_control_key_accepts_ctrl() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('u'), event::KeyModifiers::CONTROL);

        // Act & Assert
        assert!(is_control_key(key));
    }

    #[test]
    fn test_is_control_key_rejects_plain() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('u'), event::KeyModifiers::NONE);

        // Act & Assert
        assert!(!is_control_key(key));
    }

    // -----------------------------------------------------------------------
    // is_insertable_char_key
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_insertable_char_key_accepts_none() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('a'), event::KeyModifiers::NONE);

        // Act & Assert
        assert!(is_insertable_char_key(key));
    }

    #[test]
    fn test_is_insertable_char_key_accepts_shift() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('A'), event::KeyModifiers::SHIFT);

        // Act & Assert
        assert!(is_insertable_char_key(key));
    }

    #[test]
    fn test_is_insertable_char_key_rejects_control() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('a'), event::KeyModifiers::CONTROL);

        // Act & Assert
        assert!(!is_insertable_char_key(key));
    }

    // -----------------------------------------------------------------------
    // is_cursor_on_first_line / is_cursor_on_last_line
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_cursor_on_first_line_at_start() {
        // Arrange
        let mut input = InputState::with_text("hello\nworld".to_string());
        input.cursor = 0;

        // Act & Assert
        assert!(is_cursor_on_first_line(&input));
    }

    #[test]
    fn test_is_cursor_on_first_line_after_newline() {
        // Arrange
        let mut input = InputState::with_text("hello\nworld".to_string());
        input.cursor = "hello\nw".chars().count();

        // Act & Assert
        assert!(!is_cursor_on_first_line(&input));
    }

    #[test]
    fn test_is_cursor_on_last_line_at_end() {
        // Arrange
        let mut input = InputState::with_text("hello\nworld".to_string());
        input.cursor = "hello\nworld".chars().count();

        // Act & Assert
        assert!(is_cursor_on_last_line(&input));
    }

    #[test]
    fn test_is_cursor_on_last_line_before_newline() {
        // Arrange
        let mut input = InputState::with_text("hello\nworld".to_string());
        input.cursor = 3;

        // Act & Assert
        assert!(!is_cursor_on_last_line(&input));
    }

    // -----------------------------------------------------------------------
    // move_cursor_word_left / move_cursor_word_right
    // -----------------------------------------------------------------------

    #[test]
    fn test_move_cursor_word_left_skips_whitespace_and_word() {
        // Arrange
        let mut input = InputState::with_text("hello world".to_string());
        input.cursor = "hello world".chars().count();

        // Act
        move_cursor_word_left(&mut input);

        // Assert
        assert_eq!(input.cursor, "hello ".chars().count());
    }

    #[test]
    fn test_move_cursor_word_left_at_zero_is_noop() {
        // Arrange
        let mut input = InputState::with_text("hello".to_string());
        input.cursor = 0;

        // Act
        move_cursor_word_left(&mut input);

        // Assert
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn test_move_cursor_word_right_skips_word_and_whitespace() {
        // Arrange
        let mut input = InputState::with_text("hello world".to_string());
        input.cursor = 0;

        // Act
        move_cursor_word_right(&mut input);

        // Assert
        assert_eq!(input.cursor, "hello ".chars().count());
    }

    #[test]
    fn test_move_cursor_word_right_at_end_is_noop() {
        // Arrange
        let mut input = InputState::with_text("hello".to_string());
        input.cursor = "hello".chars().count();

        // Act
        move_cursor_word_right(&mut input);

        // Assert
        assert_eq!(input.cursor, "hello".chars().count());
    }

    // -----------------------------------------------------------------------
    // word_delete_range / delete_word_backward
    // -----------------------------------------------------------------------

    #[test]
    fn test_word_delete_range_returns_none_at_zero() {
        // Arrange & Act
        let result = word_delete_range("hello world", 0);

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_word_delete_range_deletes_last_word_and_separator() {
        // Arrange & Act
        let result = word_delete_range("hello brave world", "hello brave world".chars().count());

        // Assert — range covers " world" (the trailing word and its preceding
        // whitespace).
        assert_eq!(result, Some((11, "hello brave world".chars().count())));
    }

    #[test]
    fn test_delete_word_backward_removes_last_word_and_separator() {
        // Arrange
        let mut input = InputState::with_text("hello brave world".to_string());
        input.cursor = "hello brave world".chars().count();

        // Act
        delete_word_backward(&mut input);

        // Assert
        assert_eq!(input.text(), "hello brave");
    }

    #[test]
    fn test_delete_word_backward_noop_at_zero() {
        // Arrange
        let mut input = InputState::with_text("hello".to_string());
        input.cursor = 0;

        // Act
        delete_word_backward(&mut input);

        // Assert
        assert_eq!(input.text(), "hello");
    }

    // -----------------------------------------------------------------------
    // normalize_pasted_text
    // -----------------------------------------------------------------------

    #[test]
    fn test_normalize_pasted_text_replaces_carriage_returns() {
        // Arrange
        let pasted_text = "line 1\r\nline 2\rline 3\nline 4";

        // Act
        let normalized = normalize_pasted_text(pasted_text);

        // Assert
        assert_eq!(normalized, "line 1\nline 2\nline 3\nline 4");
    }

    #[test]
    fn test_normalize_pasted_text_preserves_plain_newlines() {
        // Arrange
        let pasted_text = "line 1\nline 2\nline 3";

        // Act
        let normalized = normalize_pasted_text(pasted_text);

        // Assert
        assert_eq!(normalized, pasted_text);
    }
}
