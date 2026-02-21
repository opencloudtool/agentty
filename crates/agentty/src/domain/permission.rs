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
    /// delimiter are added so the agent can clearly distinguish instructions
    /// from the user task.
    /// Other modes return the prompt unchanged.
    pub fn apply_to_prompt(self, prompt: &str) -> Cow<'_, str> {
        match self {
            PermissionMode::Plan => Cow::Owned(Self::plan_mode_prompt(prompt)),
            _ => Cow::Borrowed(prompt),
        }
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

/// Post-plan actions shown after a plan response finishes in chat view.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlanFollowupAction {
    #[default]
    ImplementPlan,
    TypeFeedback,
}

impl PlanFollowupAction {
    /// Returns the display label used by the inline action bar.
    pub fn label(self) -> &'static str {
        match self {
            PlanFollowupAction::ImplementPlan => "Implement the plan",
            PlanFollowupAction::TypeFeedback => "Type feedback",
        }
    }

    /// Cycles selection to the previous action.
    #[must_use]
    pub fn previous(self) -> Self {
        match self {
            PlanFollowupAction::ImplementPlan => PlanFollowupAction::TypeFeedback,
            PlanFollowupAction::TypeFeedback => PlanFollowupAction::ImplementPlan,
        }
    }

    /// Cycles selection to the next action.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            PlanFollowupAction::ImplementPlan => PlanFollowupAction::TypeFeedback,
            PlanFollowupAction::TypeFeedback => PlanFollowupAction::ImplementPlan,
        }
    }
}
