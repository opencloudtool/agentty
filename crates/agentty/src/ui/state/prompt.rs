use crate::domain::file::FileEntry;
use crate::domain::agent::AgentKind;

/// UI state for prompt `@` file and directory mention selection.
#[derive(Clone, Debug)]
pub struct PromptAtMentionState {
    /// Cached list of all files and directories in the session directory.
    pub all_entries: Vec<FileEntry>,
    /// Currently selected index in the filtered dropdown.
    pub selected_index: usize,
}

impl PromptAtMentionState {
    /// Creates a new at-mention state with the given file entries.
    pub fn new(all_entries: Vec<FileEntry>) -> Self {
        Self {
            all_entries,
            selected_index: 0,
        }
    }
}

/// UI state for navigating previously sent prompts with `Up` and `Down`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptHistoryState {
    /// Draft input captured before entering history navigation.
    pub draft_text: Option<String>,
    /// Previously sent user prompts in chronological order.
    pub entries: Vec<String>,
    /// Currently selected history entry index, if any.
    pub selected_index: Option<usize>,
}

impl PromptHistoryState {
    /// Creates history state from prior prompt entries.
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            draft_text: None,
            entries,
            selected_index: None,
        }
    }

    /// Clears active history navigation and stored draft text.
    pub fn reset_navigation(&mut self) {
        self.draft_text = None;
        self.selected_index = None;
    }
}

/// Steps in prompt slash command selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptSlashStage {
    Agent,
    Command,
    Model,
}

/// UI state for prompt-only slash command selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PromptSlashState {
    pub selected_agent: Option<AgentKind>,
    pub selected_index: usize,
    pub stage: PromptSlashStage,
}

impl PromptSlashState {
    /// Creates a new slash state at command selection.
    pub fn new() -> Self {
        Self {
            selected_agent: None,
            selected_index: 0,
            stage: PromptSlashStage::Command,
        }
    }

    /// Resets slash state back to command selection.
    pub fn reset(&mut self) {
        self.selected_agent = None;
        self.selected_index = 0;
        self.stage = PromptSlashStage::Command;
    }
}

impl Default for PromptSlashState {
    fn default() -> Self {
        Self::new()
    }
}
