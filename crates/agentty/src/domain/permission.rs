use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

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
    /// delimiter are added only for the initial planning prompt, so
    /// follow-up replies can stay concise.
    /// Other modes return the prompt unchanged.
    pub fn apply_to_prompt(self, prompt: &str, is_initial_plan_prompt: bool) -> Cow<'_, str> {
        if self == PermissionMode::Plan && is_initial_plan_prompt {
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
    /// Sends a clarifying question back to the agent in `Plan` mode.
    AnswerQuestion(String),
    /// Opens prompt mode so the user can type custom feedback.
    TypeFeedback,
}

impl PlanFollowupOption {
    /// Returns the display label used by follow-up menus.
    pub fn label(&self) -> String {
        match self {
            PlanFollowupOption::ImplementPlan => "Implement the plan".to_string(),
            PlanFollowupOption::AnswerQuestion(question) => question.clone(),
            PlanFollowupOption::TypeFeedback => "Type feedback".to_string(),
        }
    }
}

/// Post-plan follow-up state shown after a plan response finishes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlanFollowup {
    pub options: Vec<PlanFollowupOption>,
    selected_index: usize,
}

impl PlanFollowup {
    /// Creates follow-up state with optional clarifying questions.
    ///
    /// Option order is always:
    /// `ImplementPlan`, extracted question answers, then `TypeFeedback`.
    pub fn new(questions: Vec<String>) -> Self {
        let mut options = vec![PlanFollowupOption::ImplementPlan];
        options.extend(
            questions
                .into_iter()
                .map(PlanFollowupOption::AnswerQuestion),
        );
        options.push(PlanFollowupOption::TypeFeedback);

        Self {
            options,
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
    fn test_apply_to_prompt_keeps_followup_plan_prompt_unchanged() {
        // Arrange
        let prompt = "Refine section 3";

        // Act
        let transformed = PermissionMode::Plan.apply_to_prompt(prompt, false);

        // Assert
        assert_eq!(transformed, prompt);
    }

    #[test]
    fn test_plan_followup_new_adds_question_options_between_defaults() {
        // Arrange
        let questions = vec!["Should we support Windows?".to_string()];

        // Act
        let followup = PlanFollowup::new(questions);

        // Assert
        assert_eq!(followup.selected_index(), 0);
        assert_eq!(followup.options.len(), 3);
        assert_eq!(followup.options[0], PlanFollowupOption::ImplementPlan);
        assert_eq!(
            followup.options[1],
            PlanFollowupOption::AnswerQuestion("Should we support Windows?".to_string())
        );
        assert_eq!(followup.options[2], PlanFollowupOption::TypeFeedback);
    }

    #[test]
    fn test_plan_followup_selection_wraps_in_both_directions() {
        // Arrange
        let mut followup = PlanFollowup::new(Vec::new());

        // Act
        followup.select_previous();
        let first_selected = followup.selected_option().cloned();
        followup.select_next();
        let second_selected = followup.selected_option().cloned();

        // Assert
        assert_eq!(first_selected, Some(PlanFollowupOption::TypeFeedback));
        assert_eq!(second_selected, Some(PlanFollowupOption::ImplementPlan));
    }
}
