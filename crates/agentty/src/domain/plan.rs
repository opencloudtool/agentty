/// Extracts numbered questions from a `Questions` heading in plan output.
///
/// The parser looks for either `## Questions` or `### Questions`, then reads
/// numbered list items such as `1. Question text` until a blank line or the
/// next markdown heading.
pub fn extract_plan_questions(plan_output: &str) -> Vec<String> {
    let mut questions = Vec::new();
    let mut is_in_questions_section = false;

    for raw_line in plan_output.lines() {
        let line = raw_line.trim();

        if !is_in_questions_section {
            if is_questions_heading(line) {
                is_in_questions_section = true;
            }

            continue;
        }

        if line.is_empty() || line.starts_with('#') {
            break;
        }

        if let Some(question) = numbered_question(line) {
            questions.push(question.to_string());
        }
    }

    questions
}

fn is_questions_heading(line: &str) -> bool {
    let normalized_heading = line.trim_end_matches(':').trim().to_ascii_lowercase();

    normalized_heading == "## questions" || normalized_heading == "### questions"
}

fn numbered_question(line: &str) -> Option<&str> {
    let numeric_prefix_length = line.chars().take_while(char::is_ascii_digit).count();
    if numeric_prefix_length == 0 {
        return None;
    }

    let suffix = line.get(numeric_prefix_length..)?;
    let suffix = suffix.strip_prefix('.')?.trim_start();
    if suffix.is_empty() {
        return None;
    }

    Some(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_plan_questions_reads_numbered_items_from_questions_section() {
        // Arrange
        let output = "### Plan\n- Step one\n\n### Questions\n1. Use sqlite?\n2. Add cache?\n";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert_eq!(
            questions,
            vec!["Use sqlite?".to_string(), "Add cache?".to_string()]
        );
    }

    #[test]
    fn test_extract_plan_questions_stops_at_next_heading() {
        // Arrange
        let output = "## Questions\n1. First?\n### Files\n1. Not a question";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert_eq!(questions, vec!["First?".to_string()]);
    }

    #[test]
    fn test_extract_plan_questions_returns_empty_when_section_missing() {
        // Arrange
        let output = "### Plan\n1. Implement\n";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert!(questions.is_empty());
    }
}
