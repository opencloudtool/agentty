pub struct InputState {
    pub cursor: usize,
    text: String,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            text: String::new(),
        }
    }

    pub fn with_text(text: String) -> Self {
        let cursor = text.chars().count();

        Self { cursor, text }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn take_text(&mut self) -> String {
        self.cursor = 0;

        std::mem::take(&mut self.text)
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert_char(&mut self, ch: char) {
        let byte_offset = self.byte_offset();
        self.text.insert(byte_offset, ch);
        self.cursor += 1;
    }

    /// Inserts `text` at the cursor and moves the cursor to the end of the
    /// inserted content.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let byte_offset = self.byte_offset();
        self.text.insert_str(byte_offset, text);
        self.cursor += text.chars().count();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let start = self.byte_offset_at(self.cursor - 1);
        let end = self.byte_offset();
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete_forward(&mut self) {
        let char_count = self.text.chars().count();
        if self.cursor >= char_count {
            return;
        }

        let start = self.byte_offset();
        let end = self.byte_offset_at(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let char_count = self.text.chars().count();
        if self.cursor < char_count {
            self.cursor += 1;
        }
    }

    pub fn move_up(&mut self) {
        let (line, column) = self.line_column();
        if line == 0 {
            self.cursor = 0;

            return;
        }

        let mut current_line = 0;
        let mut line_start = 0;

        for (char_index, ch) in self.text.chars().enumerate() {
            if current_line == line - 1 {
                break;
            }
            if ch == '\n' {
                current_line += 1;
                line_start = char_index + 1;
            }
        }

        let prev_line_start = line_start;
        let prev_line_len = self
            .text
            .chars()
            .skip(prev_line_start)
            .take_while(|&c| c != '\n')
            .count();
        self.cursor = prev_line_start + column.min(prev_line_len);
    }

    pub fn move_down(&mut self) {
        let (line, column) = self.line_column();
        let line_count = self.text.chars().filter(|&c| c == '\n').count() + 1;

        if line >= line_count - 1 {
            self.cursor = self.text.chars().count();

            return;
        }

        let mut char_index = 0;
        let mut current_line = 0;

        for ch in self.text.chars() {
            char_index += 1;
            if ch == '\n' {
                current_line += 1;
                if current_line == line + 1 {
                    break;
                }
            }
        }

        let next_line_start = char_index;
        let next_line_len = self
            .text
            .chars()
            .skip(next_line_start)
            .take_while(|&c| c != '\n')
            .count();
        self.cursor = next_line_start + column.min(next_line_len);
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// Extracts the `@query` text at the current cursor position.
    ///
    /// Returns `Some((at_char_index, query))` if the cursor sits inside an
    /// `@query` token where `@` is preceded by whitespace or is at position 0.
    pub fn at_mention_query(&self) -> Option<(usize, String)> {
        extract_at_mention_query(&self.text, self.cursor)
    }

    /// Replaces characters in `[start_char..end_char)` with `replacement`
    /// and moves the cursor to the end of the inserted text.
    pub fn replace_range(&mut self, start_char: usize, end_char: usize, replacement: &str) {
        let start_byte = self.byte_offset_at(start_char);
        let end_byte = self.byte_offset_at(end_char);
        self.text.replace_range(start_byte..end_byte, replacement);
        self.cursor = start_char + replacement.chars().count();
    }

    fn byte_offset(&self) -> usize {
        self.byte_offset_at(self.cursor)
    }

    fn byte_offset_at(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(index, _)| index)
    }

    fn line_column(&self) -> (usize, usize) {
        let mut line = 0;
        let mut column = 0;

        for (index, ch) in self.text.chars().enumerate() {
            if index == self.cursor {
                break;
            }
            if ch == '\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }

        (line, column)
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

/// Extracts an `@query` pattern ending at `cursor` from `text`.
///
/// Returns `Some((at_char_index, query_string))` if the cursor sits inside
/// an `@query` token where `@` is at a word boundary (position 0 or preceded
/// by whitespace). Returns `None` if no active at-mention is detected.
pub fn extract_at_mention_query(text: &str, cursor: usize) -> Option<(usize, String)> {
    if cursor == 0 {
        return None;
    }

    let chars: Vec<char> = text.chars().collect();
    let mut scan = cursor;

    while scan > 0 {
        scan -= 1;
        let ch = *chars.get(scan)?;

        if ch == '@' {
            if scan == 0 || chars.get(scan - 1).is_some_and(|ch| ch.is_whitespace()) {
                let query: String = chars[scan + 1..cursor].iter().collect();

                return Some((scan, query));
            }

            return None;
        }

        if ch.is_whitespace() {
            return None;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_text_at_end_updates_text_and_cursor() {
        // Arrange
        let mut state = InputState::with_text("hello".to_string());

        // Act
        state.insert_text(" world");

        // Assert
        assert_eq!(state.text(), "hello world");
        assert_eq!(state.cursor, "hello world".chars().count());
    }

    #[test]
    fn test_insert_text_in_middle_preserves_surrounding_content() {
        // Arrange
        let mut state = InputState::with_text("hllo".to_string());
        state.cursor = 1;

        // Act
        state.insert_text("e");

        // Assert
        assert_eq!(state.text(), "hello");
        assert_eq!(state.cursor, 2);
    }
}
