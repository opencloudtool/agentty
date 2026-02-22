use std::collections::VecDeque;

/// A single plan question with its text and selectable answer options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanQuestion {
    /// The possible answer choices the user can pick from.
    pub answers: Vec<String>,
    /// The question text presented to the user.
    pub text: String,
}

/// Extracts numbered questions (with optional sub-numbered answer options)
/// from a `Questions` heading in plan output.
///
/// The parser looks for either `## Questions` or `### Questions`, then reads
/// top-level numbered items as questions. Indented sub-numbered items beneath
/// each question are parsed as answer options.
///
/// Example input:
/// ```text
/// ### Questions
/// 1. What interval should the task use?
///    1. 30 seconds (recommended)
///    2. 60 seconds
/// 2. Should we add retry logic?
///    1. Yes
///    2. No
/// ```
pub fn extract_plan_questions(plan_output: &str) -> VecDeque<PlanQuestion> {
    let mut questions: Vec<PlanQuestion> = Vec::new();
    let mut is_in_questions_section = false;

    for raw_line in plan_output.lines() {
        let trimmed = raw_line.trim();

        if !is_in_questions_section {
            if is_questions_heading(trimmed) {
                is_in_questions_section = true;
            }

            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            break;
        }

        let is_indented = raw_line.starts_with("   ") || raw_line.starts_with('\t');

        if is_indented {
            if let Some(answer_text) = numbered_item(trimmed)
                && let Some(current_question) = questions.last_mut()
            {
                current_question.answers.push(answer_text.to_string());
            }

            continue;
        }

        if let Some(question_text) = numbered_item(trimmed) {
            questions.push(PlanQuestion {
                answers: Vec::new(),
                text: question_text.to_string(),
            });
        }
    }

    VecDeque::from(questions)
}

fn is_questions_heading(line: &str) -> bool {
    let normalized_heading = line.trim_end_matches(':').trim().to_ascii_lowercase();

    normalized_heading == "## questions" || normalized_heading == "### questions"
}

/// Extracts the text after a numbered prefix like `1.` or `2.`.
fn numbered_item(line: &str) -> Option<&str> {
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
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].text, "Use sqlite?");
        assert!(questions[0].answers.is_empty());
        assert_eq!(questions[1].text, "Add cache?");
        assert!(questions[1].answers.is_empty());
    }

    #[test]
    fn test_extract_plan_questions_stops_at_next_heading() {
        // Arrange
        let output = "## Questions\n1. First?\n### Files\n1. Not a question";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].text, "First?");
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

    #[test]
    fn test_extract_plan_questions_parses_answer_options_under_questions() {
        // Arrange
        let output = "\
### Questions
1. What interval should the task use?
   1. 30 seconds (recommended)
   2. 60 seconds
   3. 120 seconds
2. Should we add retry logic?
   1. Yes, with exponential backoff
   2. No
";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].text, "What interval should the task use?");
        assert_eq!(
            questions[0].answers,
            vec!["30 seconds (recommended)", "60 seconds", "120 seconds",]
        );
        assert_eq!(questions[1].text, "Should we add retry logic?");
        assert_eq!(
            questions[1].answers,
            vec!["Yes, with exponential backoff", "No"]
        );
    }

    #[test]
    fn test_extract_plan_questions_handles_mixed_questions_with_and_without_answers() {
        // Arrange
        let output = "\
### Questions
1. Use sqlite or postgres?
2. What cache strategy?
   1. In-memory (recommended)
   2. Redis
";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].text, "Use sqlite or postgres?");
        assert!(questions[0].answers.is_empty());
        assert_eq!(questions[1].text, "What cache strategy?");
        assert_eq!(
            questions[1].answers,
            vec!["In-memory (recommended)", "Redis"]
        );
    }

    #[test]
    fn test_extract_plan_questions_parses_tab_indented_answers() {
        // Arrange
        let output = "### Questions\n1. Pick a color?\n\t1. Red\n\t2. Blue\n";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].answers, vec!["Red", "Blue"]);
    }

    #[test]
    fn test_extract_plan_questions_ignores_indented_non_numbered_lines() {
        // Arrange
        let output = "### Questions\n1. Question?\n   Some extra context\n   1. Answer A\n";

        // Act
        let questions = extract_plan_questions(output);

        // Assert
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].text, "Question?");
        assert_eq!(questions[0].answers, vec!["Answer A"]);
    }
}
