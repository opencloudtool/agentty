//! Parses structured `**Questions**` sections from agent assistant messages
//! and formats correlated question-answer pairs for follow-up prompts.

use std::fmt::Write as _;

/// Maximum number of questions extracted from one response.
const MAX_QUESTIONS: usize = 3;

/// Extracts numbered clarification questions from an assistant response.
///
/// Looks for a `**Questions**` heading (case-insensitive, tolerant of
/// Markdown heading variants and optional colons) followed by numbered
/// items (`1.`, `2.`, `3.`). Returns an empty [`Vec`] when no valid
/// questions section is found, and caps results at [`MAX_QUESTIONS`].
pub(crate) fn parse_questions(assistant_message: &str) -> Vec<String> {
    let Some(heading_byte_end) = find_questions_heading_end(assistant_message) else {
        return Vec::new();
    };

    let after_heading = &assistant_message[heading_byte_end..];
    let mut questions = Vec::new();

    for line in after_heading.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if is_next_section_heading(trimmed) {
            break;
        }

        if let Some(question_text) = extract_numbered_item(trimmed) {
            if !question_text.is_empty() {
                questions.push(question_text);
            }

            if questions.len() >= MAX_QUESTIONS {
                break;
            }
        }
    }

    questions
}

/// Formats user answers correlated with the original questions.
///
/// Given the original questions and the user's raw answer text (which may
/// contain numbered lines like `1. Yes\n2. users table\n3. No`), produces
/// a structured prompt section that the agent can interpret.
///
/// Returns [`None`] if `questions` is empty.
pub(crate) fn format_question_answers(questions: &[String], raw_answer: &str) -> Option<String> {
    if questions.is_empty() {
        return None;
    }

    let answer_lines: Vec<&str> = raw_answer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    let mut formatted = String::from("Answers to your questions:\n\n");

    for (index, question) in questions.iter().enumerate() {
        let answer = answer_lines
            .get(index)
            .map_or("(no answer)", |line| strip_number_prefix(line));

        let _ = write!(
            formatted,
            "{}. Q: {}\n   A: {}\n\n",
            index + 1,
            question,
            answer,
        );
    }

    formatted.push_str("Please proceed based on these answers.");

    Some(formatted)
}

/// Builds a numbered answer scaffold for pre-filling the prompt input.
///
/// Returns a multi-line string like `"1. \n2. \n3. "` so the user can type
/// answers inline next to each number.
pub(crate) fn build_answer_scaffold(question_count: usize) -> String {
    (1..=question_count)
        .map(|index| format!("{index}. "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Finds the byte offset immediately after the questions heading line.
///
/// Recognises `**Questions**`, `**Questions:**`, `## Questions`, and
/// `### Questions` (case-insensitive, with optional trailing colon and
/// closing bold markers).
fn find_questions_heading_end(message: &str) -> Option<usize> {
    let lower = message.to_lowercase();

    for pattern in ["**questions", "## questions", "### questions"] {
        if let Some(start) = lower.find(pattern) {
            let after_pattern = start + pattern.len();
            let heading_line_end = message[after_pattern..]
                .find('\n')
                .map_or(message.len(), |offset| after_pattern + offset + 1);

            return Some(heading_line_end);
        }
    }

    None
}

/// Returns whether a trimmed line represents the start of a new Markdown
/// section, which terminates question extraction.
fn is_next_section_heading(trimmed_line: &str) -> bool {
    if trimmed_line.starts_with('#') {
        return true;
    }

    if trimmed_line.starts_with("**") && !trimmed_line.to_lowercase().starts_with("**questions") {
        return true;
    }

    false
}

/// Extracts the text after a leading number prefix (`N.` or `N)`).
fn extract_numbered_item(trimmed_line: &str) -> Option<String> {
    let first_char = trimmed_line.chars().next()?;

    if !first_char.is_ascii_digit() {
        return None;
    }

    let rest = &trimmed_line[first_char.len_utf8()..];

    if let Some(text) = rest.strip_prefix(". ") {
        return Some(text.trim().to_string());
    }

    if let Some(text) = rest.strip_prefix(") ") {
        return Some(text.trim().to_string());
    }

    if let Some(text) = rest.strip_prefix('.') {
        return Some(text.trim().to_string());
    }

    if let Some(text) = rest.strip_prefix(')') {
        return Some(text.trim().to_string());
    }

    None
}

/// Strips a leading `N.` or `N)` prefix from an answer line.
fn strip_number_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    let Some(first_char) = trimmed.chars().next() else {
        return line;
    };

    if !first_char.is_ascii_digit() {
        return line;
    }

    let rest = &trimmed[first_char.len_utf8()..];

    if let Some(text) = rest.strip_prefix(". ") {
        return text.trim();
    }

    if let Some(text) = rest.strip_prefix(") ") {
        return text.trim();
    }

    if let Some(text) = rest.strip_prefix('.') {
        return text.trim();
    }

    if let Some(text) = rest.strip_prefix(')') {
        return text.trim();
    }

    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Extracts numbered questions from a standard `**Questions**` section.
    fn test_parse_questions_extracts_numbered_items() {
        // Arrange
        let message = "\
Here is my analysis of the codebase.

**Questions**
1. Should the new endpoint require authentication?
2. Which database table stores user preferences?
3. Is backwards compatibility required for the v1 API?";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert_eq!(questions.len(), 3);
        assert_eq!(
            questions[0],
            "Should the new endpoint require authentication?"
        );
        assert_eq!(
            questions[1],
            "Which database table stores user preferences?"
        );
        assert_eq!(
            questions[2],
            "Is backwards compatibility required for the v1 API?"
        );
    }

    #[test]
    /// Returns an empty vec when no questions section is present.
    fn test_parse_questions_returns_empty_when_no_heading() {
        // Arrange
        let message = "Here is my analysis. No questions needed.";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert!(questions.is_empty());
    }

    #[test]
    /// Caps extracted questions at `MAX_QUESTIONS` (3).
    fn test_parse_questions_caps_at_three() {
        // Arrange
        let message = "\
**Questions**
1. First question?
2. Second question?
3. Third question?
4. Fourth question?
5. Fifth question?";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert_eq!(questions.len(), 3);
        assert_eq!(questions[2], "Third question?");
    }

    #[test]
    /// Tolerates `## Questions` Markdown heading format.
    fn test_parse_questions_tolerates_markdown_heading() {
        // Arrange
        let message = "\
Some analysis.

## Questions
1. What framework version?
2. Should we add tests?";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0], "What framework version?");
        assert_eq!(questions[1], "Should we add tests?");
    }

    #[test]
    /// Stops extraction at the next Markdown section heading.
    fn test_parse_questions_stops_at_next_section() {
        // Arrange
        let message = "\
**Questions**
1. First question?

**Next Steps**
Here is what I will do next.";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0], "First question?");
    }

    #[test]
    /// Handles empty lines between numbered items gracefully.
    fn test_parse_questions_handles_empty_lines_between_items() {
        // Arrange
        let message = "\
**Questions**

1. First question?

2. Second question?";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0], "First question?");
        assert_eq!(questions[1], "Second question?");
    }

    #[test]
    /// Handles `**Questions:**` heading with trailing colon.
    fn test_parse_questions_handles_colon_in_heading() {
        // Arrange
        let message = "\
**Questions:**
1. Should we use JWT or session cookies?";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0], "Should we use JWT or session cookies?");
    }

    #[test]
    /// Handles parenthesis-style numbering (`1)`).
    fn test_parse_questions_handles_parenthesis_numbering() {
        // Arrange
        let message = "\
**Questions**
1) Should we refactor first?
2) Is there a migration guide?";

        // Act
        let questions = parse_questions(message);

        // Assert
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0], "Should we refactor first?");
        assert_eq!(questions[1], "Is there a migration guide?");
    }

    #[test]
    /// Correlates questions and numbered answers into a structured prompt.
    fn test_format_question_answers_correlates_pairs() {
        // Arrange
        let questions = vec!["Should we use JWT?".to_string(), "Which table?".to_string()];
        let raw_answer = "1. Yes, use JWT\n2. The users table";

        // Act
        let formatted = format_question_answers(&questions, raw_answer);

        // Assert
        let formatted = formatted.expect("should produce formatted output");
        assert!(formatted.contains("Q: Should we use JWT?"));
        assert!(formatted.contains("A: Yes, use JWT"));
        assert!(formatted.contains("Q: Which table?"));
        assert!(formatted.contains("A: The users table"));
        assert!(formatted.contains("Please proceed based on these answers."));
    }

    #[test]
    /// Handles fewer answers than questions by filling `(no answer)`.
    fn test_format_question_answers_handles_fewer_answers() {
        // Arrange
        let questions = vec![
            "First?".to_string(),
            "Second?".to_string(),
            "Third?".to_string(),
        ];
        let raw_answer = "1. Only this";

        // Act
        let formatted =
            format_question_answers(&questions, raw_answer).expect("should produce output");

        // Assert
        assert!(formatted.contains("A: Only this"));
        assert!(formatted.contains("A: (no answer)"));
    }

    #[test]
    /// Returns `None` for empty questions slice.
    fn test_format_question_answers_returns_none_for_empty_questions() {
        // Arrange
        let questions: Vec<String> = vec![];

        // Act
        let formatted = format_question_answers(&questions, "some text");

        // Assert
        assert!(formatted.is_none());
    }

    #[test]
    /// Builds a numbered scaffold matching the question count.
    fn test_build_answer_scaffold_creates_numbered_lines() {
        // Arrange
        let question_count = 3;

        // Act
        let scaffold = build_answer_scaffold(question_count);

        // Assert
        assert_eq!(scaffold, "1. \n2. \n3. ");
    }

    #[test]
    /// Builds a single-line scaffold for one question.
    fn test_build_answer_scaffold_single_question() {
        // Arrange
        let question_count = 1;

        // Act
        let scaffold = build_answer_scaffold(question_count);

        // Assert
        assert_eq!(scaffold, "1. ");
    }
}
