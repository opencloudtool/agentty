use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::fmt::Write;
use std::str::FromStr;

use crate::domain::plan::PlanQuestion;

pub const PLAN_MODE_INSTRUCTIONS: &str = include_str!("../../resources/plan_mode.md");
const PLAN_MODE_PROMPT_TEMPLATE: &str = include_str!("../../resources/plan_mode_prompt.md");

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PermissionMode {
    #[default]
    AutoEdit,
    Autonomous,
    Plan,
}

impl PermissionMode {
    /// Returns the wire label used for persistence and display.
    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::AutoEdit => "auto_edit",
            PermissionMode::Autonomous => "autonomous",
            PermissionMode::Plan => "plan",
        }
    }

    /// Returns the user-facing label shown in the UI.
    pub fn display_label(self) -> &'static str {
        match self {
            PermissionMode::AutoEdit => "Auto Edit",
            PermissionMode::Autonomous => "Autonomous",
            PermissionMode::Plan => "Plan",
        }
    }

    /// Cycles to the next permission mode.
    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            PermissionMode::AutoEdit => PermissionMode::Autonomous,
            PermissionMode::Autonomous => PermissionMode::Plan,
            PermissionMode::Plan => PermissionMode::AutoEdit,
        }
    }

    /// Transforms a prompt for the active permission mode.
    ///
    /// In `Plan` mode a concise instruction prefix and a labeled prompt
    /// delimiter are added for both initial and follow-up prompts so
    /// replies continue producing plan output instead of implementation.
    /// Other modes return the prompt unchanged.
    pub fn apply_to_prompt(self, prompt: &str, _is_initial_plan_prompt: bool) -> Cow<'_, str> {
        if self == PermissionMode::Plan {
            return Cow::Owned(Self::plan_mode_prompt(prompt));
        }

        Cow::Borrowed(prompt)
    }

    fn plan_mode_prompt(prompt: &str) -> String {
        PLAN_MODE_PROMPT_TEMPLATE
            .trim_end()
            .replace("{plan_mode_instructions}", PLAN_MODE_INSTRUCTIONS)
            .replace("{prompt}", prompt)
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl FromStr for PermissionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto_edit" => Ok(PermissionMode::AutoEdit),
            "autonomous" => Ok(PermissionMode::Autonomous),
            "plan" => Ok(PermissionMode::Plan),
            _ => Err(format!("Unknown permission mode: {s}")),
        }
    }
}

/// One selectable post-plan option shown in chat view.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PlanFollowupOption {
    /// Switches to `AutoEdit` and submits implementation prompt.
    ImplementPlan,
    /// Stores the selected answer for the current question.
    AnswerQuestion {
        /// The answer text selected by the user.
        answer: String,
        /// The question text this answer belongs to.
        question: String,
    },
    /// Opens prompt mode so the user can type custom feedback.
    TypeFeedback,
}

impl PlanFollowupOption {
    /// Returns the display label used by follow-up menus.
    pub fn label(&self) -> String {
        match self {
            PlanFollowupOption::ImplementPlan => "Implement the plan".to_string(),
            PlanFollowupOption::AnswerQuestion { answer, .. } => answer.clone(),
            PlanFollowupOption::TypeFeedback => "Type feedback".to_string(),
        }
    }
}

/// Post-plan follow-up state shown after a plan response finishes.
///
/// Operates as a two-phase state machine:
/// 1. **Question phase**: presents one question at a time with its answer
///    options, while still allowing `ImplementPlan` and `TypeFeedback`.
///    Selecting an answer stores it and advances to the next question.
/// 2. **Final phase**: shows `ImplementPlan` and `TypeFeedback` when no
///    questions exist or all have been answered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlanFollowup {
    pub options: Vec<PlanFollowupOption>,
    collected_answers: Vec<(String, String)>,
    current_question: Option<PlanQuestion>,
    pending_questions: VecDeque<PlanQuestion>,
    selected_index: usize,
}

impl PlanFollowup {
    /// Creates follow-up state with optional clarifying questions.
    ///
    /// When questions with answers exist, the first question is presented
    /// immediately and options include all answers plus `ImplementPlan` and
    /// `TypeFeedback`. Questions without answer options are treated as
    /// free-text-only and show `ImplementPlan` plus `TypeFeedback`.
    /// When no questions exist, shows `ImplementPlan` and `TypeFeedback`.
    pub fn new(mut questions: VecDeque<PlanQuestion>) -> Self {
        let current_question = questions.pop_front();
        let options = Self::build_options_for_question(current_question.as_ref());

        Self {
            options,
            collected_answers: Vec::new(),
            current_question,
            pending_questions: questions,
            selected_index: 0,
        }
    }

    /// Returns the currently selected follow-up option.
    #[must_use]
    pub fn selected_option(&self) -> Option<&PlanFollowupOption> {
        self.options.get(self.selected_index)
    }

    /// Returns the selected option index.
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Returns the current question text for display above the answer
    /// options, if in the question phase.
    #[must_use]
    pub fn current_question_text(&self) -> Option<&str> {
        self.current_question
            .as_ref()
            .filter(|question| !question.answers.is_empty())
            .map(|question| question.text.as_str())
    }

    /// Returns whether any answers have been collected during question
    /// iteration.
    #[must_use]
    pub fn has_collected_answers(&self) -> bool {
        !self.collected_answers.is_empty()
    }

    /// Stores the selected answer and advances to the next question.
    ///
    /// Returns `true` if more questions remain (the menu refreshes for
    /// the next question). Returns `false` if all questions are done
    /// (switches to `ImplementPlan` / `TypeFeedback`).
    pub fn advance_to_next_question(&mut self, question: String, answer: String) -> bool {
        self.collected_answers.push((question, answer));
        self.current_question = self.pending_questions.pop_front();
        self.options = Self::build_options_for_question(self.current_question.as_ref());
        self.selected_index = 0;

        self.current_question.is_some()
    }

    /// Builds a consolidated prompt containing all collected Q&A pairs.
    ///
    /// The prompt asks the agent to update the plan reflecting the user's
    /// answers.
    #[must_use]
    pub fn build_consolidated_answer_prompt(&self) -> String {
        let mut prompt = String::from("User answers to plan questions:\n");

        for (index, (question, answer)) in self.collected_answers.iter().enumerate() {
            let _ = writeln!(prompt, "{}. {} \u{2192} {}", index + 1, question, answer);
        }

        prompt.push_str(
            "\nTreat these answers as final decisions. Do not repeat already answered questions. \
             Please update the plan to reflect these answers. Ask new clarifying questions only \
             if a new blocking ambiguity remains.",
        );

        prompt
    }

    /// Selects the previous option, wrapping to the end.
    pub fn select_previous(&mut self) {
        if self.options.is_empty() {
            return;
        }

        self.selected_index = if self.selected_index == 0 {
            self.options.len() - 1
        } else {
            self.selected_index - 1
        };
    }

    /// Selects the next option, wrapping to the beginning.
    pub fn select_next(&mut self) {
        if self.options.is_empty() {
            return;
        }

        self.selected_index = (self.selected_index + 1) % self.options.len();
    }

    /// Builds the option list for the given question phase.
    ///
    /// If a question with answers is active, options are its answers plus
    /// `ImplementPlan` and `TypeFeedback`. If the question has no answers
    /// (free-text only), or there is no question, options are
    /// `ImplementPlan` plus `TypeFeedback`.
    fn build_options_for_question(question: Option<&PlanQuestion>) -> Vec<PlanFollowupOption> {
        let Some(question) = question else {
            return vec![
                PlanFollowupOption::ImplementPlan,
                PlanFollowupOption::TypeFeedback,
            ];
        };

        if question.answers.is_empty() {
            return vec![
                PlanFollowupOption::ImplementPlan,
                PlanFollowupOption::TypeFeedback,
            ];
        }

        let mut options: Vec<PlanFollowupOption> = question
            .answers
            .iter()
            .map(|answer| PlanFollowupOption::AnswerQuestion {
                answer: answer.clone(),
                question: question.text.clone(),
            })
            .collect();
        options.push(PlanFollowupOption::ImplementPlan);
        options.push(PlanFollowupOption::TypeFeedback);

        options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_to_prompt_wraps_initial_plan_prompt() {
        // Arrange
        let prompt = "Create a migration";

        // Act
        let transformed = PermissionMode::Plan.apply_to_prompt(prompt, true);

        // Assert
        assert!(transformed.contains("[PLAN MODE]"));
        assert!(transformed.contains(prompt));
    }

    #[test]
    fn test_apply_to_prompt_wraps_followup_plan_prompt() {
        // Arrange
        let prompt = "Refine section 3";

        // Act
        let transformed = PermissionMode::Plan.apply_to_prompt(prompt, false);

        // Assert
        assert!(transformed.contains("[PLAN MODE]"));
        assert!(transformed.contains(prompt));
    }

    #[test]
    fn test_plan_followup_new_without_questions_shows_implement_and_feedback() {
        // Arrange & Act
        let followup = PlanFollowup::new(VecDeque::new());

        // Assert
        assert_eq!(followup.selected_index(), 0);
        assert_eq!(followup.options.len(), 2);
        assert_eq!(followup.options[0], PlanFollowupOption::ImplementPlan);
        assert_eq!(followup.options[1], PlanFollowupOption::TypeFeedback);
        assert!(followup.current_question_text().is_none());
    }

    #[test]
    fn test_plan_followup_new_with_question_answers_shows_answers_implement_and_feedback() {
        // Arrange
        let questions = VecDeque::from(vec![PlanQuestion {
            answers: vec!["30 seconds".to_string(), "60 seconds".to_string()],
            text: "What interval?".to_string(),
        }]);

        // Act
        let followup = PlanFollowup::new(questions);

        // Assert
        assert_eq!(followup.options.len(), 4);
        assert_eq!(
            followup.options[0],
            PlanFollowupOption::AnswerQuestion {
                answer: "30 seconds".to_string(),
                question: "What interval?".to_string(),
            }
        );
        assert_eq!(
            followup.options[1],
            PlanFollowupOption::AnswerQuestion {
                answer: "60 seconds".to_string(),
                question: "What interval?".to_string(),
            }
        );
        assert_eq!(followup.options[2], PlanFollowupOption::ImplementPlan);
        assert_eq!(followup.options[3], PlanFollowupOption::TypeFeedback);
        assert_eq!(followup.current_question_text(), Some("What interval?"));
    }

    #[test]
    fn test_plan_followup_new_with_no_answer_question_shows_implement_and_feedback() {
        // Arrange
        let questions = VecDeque::from(vec![PlanQuestion {
            answers: Vec::new(),
            text: "Use sqlite?".to_string(),
        }]);

        // Act
        let followup = PlanFollowup::new(questions);

        // Assert
        assert_eq!(followup.options.len(), 2);
        assert_eq!(followup.options[0], PlanFollowupOption::ImplementPlan);
        assert_eq!(followup.options[1], PlanFollowupOption::TypeFeedback);
        assert!(followup.current_question_text().is_none());
    }

    #[test]
    fn test_plan_followup_advance_stores_answer_and_moves_to_next_question() {
        // Arrange
        let questions = VecDeque::from(vec![
            PlanQuestion {
                answers: vec!["Yes".to_string(), "No".to_string()],
                text: "Add cache?".to_string(),
            },
            PlanQuestion {
                answers: vec!["Redis".to_string(), "Memcached".to_string()],
                text: "Which cache?".to_string(),
            },
        ]);
        let mut followup = PlanFollowup::new(questions);

        // Act
        let has_more =
            followup.advance_to_next_question("Add cache?".to_string(), "Yes".to_string());

        // Assert
        assert!(has_more);
        assert_eq!(followup.current_question_text(), Some("Which cache?"));
        assert!(followup.has_collected_answers());
        assert_eq!(followup.options.len(), 4);
        assert_eq!(followup.options[2], PlanFollowupOption::ImplementPlan);
        assert_eq!(followup.options[3], PlanFollowupOption::TypeFeedback);
    }

    #[test]
    fn test_plan_followup_advance_after_last_question_switches_to_final_phase() {
        // Arrange
        let questions = VecDeque::from(vec![PlanQuestion {
            answers: vec!["Yes".to_string(), "No".to_string()],
            text: "Add cache?".to_string(),
        }]);
        let mut followup = PlanFollowup::new(questions);

        // Act
        let has_more =
            followup.advance_to_next_question("Add cache?".to_string(), "Yes".to_string());

        // Assert
        assert!(!has_more);
        assert!(followup.current_question_text().is_none());
        assert_eq!(followup.options.len(), 2);
        assert_eq!(followup.options[0], PlanFollowupOption::ImplementPlan);
        assert_eq!(followup.options[1], PlanFollowupOption::TypeFeedback);
    }

    #[test]
    fn test_plan_followup_build_consolidated_answer_prompt() {
        // Arrange
        let questions = VecDeque::from(vec![
            PlanQuestion {
                answers: vec!["30s".to_string()],
                text: "Interval?".to_string(),
            },
            PlanQuestion {
                answers: vec!["Yes".to_string()],
                text: "Retry?".to_string(),
            },
        ]);
        let mut followup = PlanFollowup::new(questions);
        followup.advance_to_next_question("Interval?".to_string(), "30s".to_string());
        followup.advance_to_next_question("Retry?".to_string(), "Yes".to_string());

        // Act
        let prompt = followup.build_consolidated_answer_prompt();

        // Assert
        assert!(prompt.contains("1. Interval? \u{2192} 30s"));
        assert!(prompt.contains("2. Retry? \u{2192} Yes"));
        assert!(prompt.contains("update the plan"));
    }

    #[test]
    fn test_plan_followup_selection_wraps_in_both_directions() {
        // Arrange
        let mut followup = PlanFollowup::new(VecDeque::new());

        // Act
        followup.select_previous();
        let first_selected = followup.selected_option().cloned();
        followup.select_next();
        let second_selected = followup.selected_option().cloned();

        // Assert
        assert_eq!(first_selected, Some(PlanFollowupOption::TypeFeedback));
        assert_eq!(second_selected, Some(PlanFollowupOption::ImplementPlan));
    }

    #[test]
    fn test_plan_followup_option_label_for_answer_shows_answer_text() {
        // Arrange
        let option = PlanFollowupOption::AnswerQuestion {
            answer: "60 seconds".to_string(),
            question: "What interval?".to_string(),
        };

        // Act
        let label = option.label();

        // Assert
        assert_eq!(label, "60 seconds");
    }
}
