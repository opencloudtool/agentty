use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

use crate::agent::AgentKind;
use crate::icon::Icon;

pub const SESSION_DATA_DIR: &str = ".agentty";

pub struct Project {
    pub git_branch: Option<String>,
    pub id: i64,
    pub path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Sessions,
    Stats,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    New,
    InProgress,
    Review,
    CreatingPullRequest,
    PullRequest,
    Done,
    Canceled,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::New => write!(f, "New"),
            Status::InProgress => write!(f, "InProgress"),
            Status::Review => write!(f, "Review"),
            Status::CreatingPullRequest => write!(f, "CreatingPullRequest"),
            Status::PullRequest => write!(f, "PullRequest"),
            Status::Done => write!(f, "Done"),
            Status::Canceled => write!(f, "Canceled"),
        }
    }
}

impl std::str::FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "New" => Ok(Status::New),
            "InProgress" | "Committing" => Ok(Status::InProgress),
            "Review" => Ok(Status::Review),
            "CreatingPullRequest" => Ok(Status::CreatingPullRequest),
            "PullRequest" | "Processing" => Ok(Status::PullRequest),
            "Done" => Ok(Status::Done),
            "Canceled" | "Cancelled" => Ok(Status::Canceled),
            _ => Err(format!("Unknown status: {s}")),
        }
    }
}

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

pub enum AppMode {
    List,
    Prompt {
        slash_state: PromptSlashState,
        session_id: String,
        input: InputState,
        scroll_offset: Option<u16>,
    },
    View {
        session_id: String,
        scroll_offset: Option<u16>,
    },
    Diff {
        session_id: String,
        diff: String,
        scroll_offset: u16,
    },
    CommandPalette {
        input: String,
        selected_index: usize,
        focus: PaletteFocus,
    },
    CommandOption {
        command: PaletteCommand,
        selected_index: usize,
    },
    Help {
        context: HelpContext,
        scroll_offset: u16,
    },
    Health,
}

/// Captures which page opened the help overlay so it can be restored on close.
pub enum HelpContext {
    List,
    View {
        session_id: String,
        scroll_offset: Option<u16>,
    },
    Diff {
        session_id: String,
        diff: String,
        scroll_offset: u16,
    },
    Health,
}

impl HelpContext {
    /// Returns the keybinding pairs `(key, description)` for the originating
    /// page.
    pub fn keybindings(&self) -> &[(&str, &str)] {
        match self {
            HelpContext::List => &[
                ("q", "Quit"),
                ("a", "Add session"),
                ("d", "Delete session"),
                ("Enter", "Open session"),
                ("Tab", "Switch tab"),
                ("/", "Command palette"),
                ("?", "Help"),
            ],
            HelpContext::View { .. } => &[
                ("q", "Back to list"),
                ("Enter", "Reply"),
                ("d", "Show diff"),
                ("p", "Create PR"),
                ("m", "Merge"),
                ("g", "Scroll to top"),
                ("G", "Scroll to bottom"),
                ("Ctrl+d", "Half page down"),
                ("Ctrl+u", "Half page up"),
                ("?", "Help"),
            ],
            HelpContext::Diff { .. } => &[("q / Esc", "Back to session"), ("?", "Help")],
            HelpContext::Health => &[
                ("q / Esc", "Back to list"),
                ("Ctrl+c", "Back to list"),
                ("r", "Rerun checks"),
                ("?", "Help"),
            ],
        }
    }

    /// Reconstructs the `AppMode` that was active before help was opened.
    pub fn restore_mode(self) -> AppMode {
        match self {
            HelpContext::List => AppMode::List,
            HelpContext::View {
                session_id,
                scroll_offset,
            } => AppMode::View {
                session_id,
                scroll_offset,
            },
            HelpContext::Diff {
                session_id,
                diff,
                scroll_offset,
            } => AppMode::Diff {
                session_id,
                diff,
                scroll_offset,
            },
            HelpContext::Health => AppMode::Health,
        }
    }

    /// Display title for the help overlay header.
    pub fn title(&self) -> &'static str {
        "Keybindings"
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteFocus {
    Input,
    Dropdown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteCommand {
    Health,
    Projects,
}

impl PaletteCommand {
    pub const ALL: &[PaletteCommand] = &[PaletteCommand::Health, PaletteCommand::Projects];

    pub fn label(self) -> &'static str {
        match self {
            PaletteCommand::Health => "health",
            PaletteCommand::Projects => "projects",
        }
    }

    pub fn filter(query: &str) -> Vec<PaletteCommand> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<PaletteCommand> = Self::ALL
            .iter()
            .filter(|cmd| cmd.label().contains(&query_lower))
            .copied()
            .collect();
        results.sort_by_key(|cmd| cmd.label());
        results
    }
}

/// Per-session token statistics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionStats {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

pub struct Session {
    pub agent: String,
    pub base_branch: String,
    pub commit_count: Arc<Mutex<i64>>,
    pub folder: PathBuf,
    pub id: String,
    pub model: String,
    pub output: Arc<Mutex<String>>,
    pub project_name: String,
    pub prompt: String,
    pub stats: SessionStats,
    pub status: Arc<Mutex<Status>>,
    pub title: Option<String>,
}

impl Session {
    /// Returns the display title for this session.
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("No title")
    }

    /// Returns the current commit count for this session.
    pub fn commit_count(&self) -> i64 {
        *self
            .commit_count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn status(&self) -> Status {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn append_output(&self, message: &str) {
        Self::write_output(&self.output, &self.folder, message);
    }

    pub(crate) fn write_output(output: &Mutex<String>, _folder: &Path, message: &str) {
        if let Ok(mut buf) = output.lock() {
            buf.push_str(message);
        }
    }
}

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Sessions => "Sessions",
            Tab::Stats => "Stats",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Tab::Sessions => Tab::Stats,
            Tab::Stats => Tab::Sessions,
        }
    }
}

impl Status {
    pub fn can_transition_to(self, next: Status) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (Status::New, Status::InProgress)
                | (
                    Status::Review,
                    Status::InProgress
                        | Status::PullRequest
                        | Status::Done
                        | Status::Canceled
                        | Status::CreatingPullRequest
                )
                | (Status::InProgress, Status::Review)
                | (
                    Status::CreatingPullRequest,
                    Status::PullRequest | Status::Review
                )
                | (Status::PullRequest, Status::Done | Status::Canceled)
        )
    }

    pub fn icon(self) -> Icon {
        match self {
            Status::New | Status::Review => Icon::Pending,
            Status::InProgress | Status::PullRequest | Status::CreatingPullRequest => {
                Icon::current_spinner()
            }
            Status::Done => Icon::Check,
            Status::Canceled => Icon::Cross,
        }
    }

    pub fn color(self) -> Color {
        match self {
            Status::New => Color::DarkGray,
            Status::InProgress => Color::Yellow,
            Status::Review => Color::LightBlue,
            Status::PullRequest | Status::CreatingPullRequest => Color::Cyan,
            Status::Done => Color::Green,
            Status::Canceled => Color::Red,
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_display_title() {
        // Arrange
        let session = Session {
            agent: "gemini".to_string(),
            base_branch: String::new(),
            commit_count: Arc::new(Mutex::new(0)),
            folder: PathBuf::new(),
            id: "abc123".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: String::new(),
            prompt: String::new(),
            stats: SessionStats::default(),
            status: Arc::new(Mutex::new(Status::New)),
            title: Some("Fix the login bug".to_string()),
        };

        // Act & Assert
        assert_eq!(session.display_title(), "Fix the login bug");
    }

    #[test]
    fn test_display_title_none() {
        // Arrange
        let session = Session {
            agent: "gemini".to_string(),
            base_branch: String::new(),
            commit_count: Arc::new(Mutex::new(0)),
            folder: PathBuf::new(),
            id: "abc123".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: String::new(),
            prompt: String::new(),
            stats: SessionStats::default(),
            status: Arc::new(Mutex::new(Status::New)),
            title: None,
        };

        // Act & Assert
        assert_eq!(session.display_title(), "No title");
    }

    #[test]
    fn test_session_status() {
        // Arrange
        let session = Session {
            agent: "gemini".to_string(),
            base_branch: String::new(),
            commit_count: Arc::new(Mutex::new(0)),
            folder: PathBuf::new(),
            id: "test".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: String::new(),
            prompt: "prompt".to_string(),
            stats: SessionStats::default(),
            status: Arc::new(Mutex::new(Status::Review)),
            title: None,
        };

        // Act & Assert (Review)
        assert_eq!(session.status(), Status::Review);

        // Act
        if let Ok(mut status) = session.status.lock() {
            *status = Status::InProgress;
        }

        // Assert (InProgress)
        assert_eq!(session.status(), Status::InProgress);

        // Act
        if let Ok(mut status) = session.status.lock() {
            *status = Status::Review;
        }

        // Assert (Review)
        assert_eq!(session.status(), Status::Review);
    }

    #[test]
    fn test_append_output() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let session = Session {
            agent: "gemini".to_string(),
            base_branch: String::new(),
            commit_count: Arc::new(Mutex::new(0)),
            folder: dir.path().to_path_buf(),
            id: "test".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: String::new(),
            prompt: "prompt".to_string(),
            stats: SessionStats::default(),
            status: Arc::new(Mutex::new(Status::Done)),
            title: None,
        };

        // Act
        session.append_output("[Test] Hello\n");

        // Assert — in-memory buffer
        let buf = session.output.lock().expect("lock failed");
        assert_eq!(*buf, "[Test] Hello\n");
    }

    #[test]
    fn test_status_icon() {
        // Arrange & Act & Assert
        assert_eq!(Status::New.icon(), Icon::Pending);
        assert!(matches!(Status::InProgress.icon(), Icon::Spinner(_)));
        assert_eq!(Status::Review.icon(), Icon::Pending);
        assert!(matches!(
            Status::CreatingPullRequest.icon(),
            Icon::Spinner(_)
        ));
        assert!(matches!(Status::PullRequest.icon(), Icon::Spinner(_)));
        assert_eq!(Status::Done.icon(), Icon::Check);
        assert_eq!(Status::Canceled.icon(), Icon::Cross);
    }

    #[test]
    fn test_status_color() {
        // Arrange & Act & Assert
        assert_eq!(Status::New.color(), Color::DarkGray);
        assert_eq!(Status::InProgress.color(), Color::Yellow);
        assert_eq!(Status::Review.color(), Color::LightBlue);
        assert_eq!(Status::CreatingPullRequest.color(), Color::Cyan);
        assert_eq!(Status::PullRequest.color(), Color::Cyan);
        assert_eq!(Status::Done.color(), Color::Green);
        assert_eq!(Status::Canceled.color(), Color::Red);
    }

    #[test]
    fn test_status_from_str_legacy_processing_maps_to_pull_request() {
        // Arrange & Act
        let status = "Processing".parse::<Status>().expect("failed to parse");

        // Assert
        assert_eq!(status, Status::PullRequest);
    }

    #[test]
    fn test_status_from_str_canceled() {
        // Arrange & Act
        let status = "Canceled".parse::<Status>().expect("failed to parse");

        // Assert
        assert_eq!(status, Status::Canceled);
    }

    #[test]
    fn test_status_transition_rules() {
        // Arrange & Act & Assert
        assert!(Status::New.can_transition_to(Status::InProgress));
        assert!(!Status::New.can_transition_to(Status::Review));
        assert!(!Status::New.can_transition_to(Status::Done));
        assert!(Status::Review.can_transition_to(Status::InProgress));
        assert!(Status::Review.can_transition_to(Status::PullRequest));
        assert!(Status::Review.can_transition_to(Status::CreatingPullRequest));
        assert!(Status::Review.can_transition_to(Status::Done));
        assert!(Status::Review.can_transition_to(Status::Canceled));
        assert!(Status::CreatingPullRequest.can_transition_to(Status::PullRequest));
        assert!(Status::CreatingPullRequest.can_transition_to(Status::Review));
        assert!(!Status::CreatingPullRequest.can_transition_to(Status::Canceled));
        assert!(Status::InProgress.can_transition_to(Status::Review));
        assert!(!Status::InProgress.can_transition_to(Status::Canceled));
        assert!(Status::PullRequest.can_transition_to(Status::Done));
        assert!(Status::PullRequest.can_transition_to(Status::Canceled));
        assert!(!Status::New.can_transition_to(Status::Canceled));
        assert!(!Status::InProgress.can_transition_to(Status::Done));
        assert!(!Status::Done.can_transition_to(Status::InProgress));
        assert!(!Status::Done.can_transition_to(Status::Canceled));
        assert!(!Status::Canceled.can_transition_to(Status::InProgress));
    }

    #[test]
    fn test_palette_command_label() {
        // Arrange & Act & Assert
        assert_eq!(PaletteCommand::Health.label(), "health");
        assert_eq!(PaletteCommand::Projects.label(), "projects");
    }

    #[test]
    fn test_palette_command_all() {
        // Arrange & Act & Assert
        assert_eq!(PaletteCommand::ALL.len(), 2);
        assert_eq!(PaletteCommand::ALL[0], PaletteCommand::Health);
        assert_eq!(PaletteCommand::ALL[1], PaletteCommand::Projects);
    }

    #[test]
    fn test_palette_command_filter() {
        // Arrange & Act
        let results = PaletteCommand::filter("heal");

        // Assert
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], PaletteCommand::Health);
    }

    #[test]
    fn test_palette_command_filter_case_insensitive() {
        // Arrange & Act
        let results = PaletteCommand::filter("HEAL");

        // Assert
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], PaletteCommand::Health);
    }

    #[test]
    fn test_palette_command_filter_no_match() {
        // Arrange & Act
        let results = PaletteCommand::filter("xyz");

        // Assert
        assert!(results.is_empty());
    }

    #[test]
    fn test_palette_command_filter_projects() {
        // Arrange & Act
        let results = PaletteCommand::filter("proj");

        // Assert
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], PaletteCommand::Projects);
    }

    #[test]
    fn test_palette_command_filter_empty_query() {
        // Arrange & Act
        let results = PaletteCommand::filter("");

        // Assert — empty query matches all commands
        assert_eq!(results.len(), PaletteCommand::ALL.len());
    }

    #[test]
    fn test_prompt_slash_state_new() {
        // Arrange & Act
        let state = PromptSlashState::new();

        // Assert
        assert_eq!(state.stage, PromptSlashStage::Command);
        assert_eq!(state.selected_index, 0);
        assert_eq!(state.selected_agent, None);
    }

    #[test]
    fn test_prompt_slash_state_reset() {
        // Arrange
        let mut state = PromptSlashState {
            selected_agent: Some(AgentKind::Claude),
            selected_index: 2,
            stage: PromptSlashStage::Model,
        };

        // Act
        state.reset();

        // Assert
        assert_eq!(state.stage, PromptSlashStage::Command);
        assert_eq!(state.selected_index, 0);
        assert_eq!(state.selected_agent, None);
    }

    #[test]
    fn test_tab_title() {
        // Arrange & Act & Assert
        assert_eq!(Tab::Sessions.title(), "Sessions");
        assert_eq!(Tab::Stats.title(), "Stats");
    }

    #[test]
    fn test_tab_next() {
        // Arrange & Act & Assert
        assert_eq!(Tab::Sessions.next(), Tab::Stats);
        assert_eq!(Tab::Stats.next(), Tab::Sessions);
    }

    #[test]
    fn test_input_state_new() {
        // Arrange & Act
        let state = InputState::new();

        // Assert
        assert_eq!(state.text(), "");
        assert_eq!(state.cursor, 0);
        assert!(state.is_empty());
    }

    #[test]
    fn test_input_state_with_text() {
        // Arrange & Act
        let state = InputState::with_text("hello".to_string());

        // Assert
        assert_eq!(state.text(), "hello");
        assert_eq!(state.cursor, 5);
    }

    #[test]
    fn test_input_state_insert_char_at_end() {
        // Arrange
        let mut state = InputState::with_text("ab".to_string());

        // Act
        state.insert_char('c');

        // Assert
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor, 3);
    }

    #[test]
    fn test_input_state_insert_char_at_start() {
        // Arrange
        let mut state = InputState::with_text("bc".to_string());
        state.cursor = 0;

        // Act
        state.insert_char('a');

        // Assert
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn test_input_state_insert_char_in_middle() {
        // Arrange
        let mut state = InputState::with_text("ac".to_string());
        state.cursor = 1;

        // Act
        state.insert_char('b');

        // Assert
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_input_state_delete_backward() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());

        // Act
        state.delete_backward();

        // Assert
        assert_eq!(state.text(), "ab");
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_input_state_delete_backward_at_start() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());
        state.cursor = 0;

        // Act
        state.delete_backward();

        // Assert
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_input_state_delete_backward_in_middle() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());
        state.cursor = 2;

        // Act
        state.delete_backward();

        // Assert
        assert_eq!(state.text(), "ac");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn test_input_state_delete_forward() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());
        state.cursor = 1;

        // Act
        state.delete_forward();

        // Assert
        assert_eq!(state.text(), "ac");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn test_input_state_delete_forward_at_end() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());

        // Act
        state.delete_forward();

        // Assert
        assert_eq!(state.text(), "abc");
        assert_eq!(state.cursor, 3);
    }

    #[test]
    fn test_input_state_move_left() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());

        // Act
        state.move_left();

        // Assert
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_input_state_move_left_at_start() {
        // Arrange
        let mut state = InputState::new();

        // Act
        state.move_left();

        // Assert
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_input_state_move_right() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());
        state.cursor = 1;

        // Act
        state.move_right();

        // Assert
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_input_state_move_right_at_end() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());

        // Act
        state.move_right();

        // Assert
        assert_eq!(state.cursor, 3);
    }

    #[test]
    fn test_input_state_move_home() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());

        // Act
        state.move_home();

        // Assert
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_input_state_move_end() {
        // Arrange
        let mut state = InputState::with_text("abc".to_string());
        state.cursor = 0;

        // Act
        state.move_end();

        // Assert
        assert_eq!(state.cursor, 3);
    }

    #[test]
    fn test_input_state_insert_newline() {
        // Arrange
        let mut state = InputState::with_text("ab".to_string());
        state.cursor = 1;

        // Act
        state.insert_newline();

        // Assert
        assert_eq!(state.text(), "a\nb");
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_input_state_take_text() {
        // Arrange
        let mut state = InputState::with_text("hello".to_string());

        // Act
        let taken = state.take_text();

        // Assert
        assert_eq!(taken, "hello");
        assert_eq!(state.text(), "");
        assert_eq!(state.cursor, 0);
        assert!(state.is_empty());
    }

    #[test]
    fn test_input_state_unicode() {
        // Arrange
        let mut state = InputState::with_text("héllo".to_string());
        assert_eq!(state.cursor, 5);

        // Act — delete the 'é'
        state.cursor = 2;
        state.delete_backward();

        // Assert
        assert_eq!(state.text(), "hllo");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn test_input_state_unicode_insert() {
        // Arrange
        let mut state = InputState::with_text("hllo".to_string());
        state.cursor = 1;

        // Act
        state.insert_char('é');

        // Assert
        assert_eq!(state.text(), "héllo");
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_input_state_default() {
        // Arrange & Act
        let state = InputState::default();

        // Assert
        assert_eq!(state.text(), "");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_input_state_move_up_first_line() {
        // Arrange
        let mut state = InputState::with_text("abc\ndef".to_string());
        state.cursor = 2; // on 'c' of first line

        // Act
        state.move_up();

        // Assert — already on first line, moves to start
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_input_state_move_up_second_to_first() {
        // Arrange — "abc\ndef", cursor on 'e' (index 5)
        let mut state = InputState::with_text("abc\ndef".to_string());
        state.cursor = 5;

        // Act
        state.move_up();

        // Assert — column 1 on first line = 'b'
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn test_input_state_move_up_clamps_column() {
        // Arrange — "ab\ncdef", cursor at end of second line (index 7)
        let mut state = InputState::with_text("ab\ncdef".to_string());

        // Act — column 4 on second line, but first line only has 2 chars
        state.move_up();

        // Assert — clamped to end of first line
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_input_state_move_down_last_line() {
        // Arrange
        let mut state = InputState::with_text("abc\ndef".to_string());

        // Act — already at end of last line
        state.move_down();

        // Assert — stays at end
        assert_eq!(state.cursor, 7);
    }

    #[test]
    fn test_input_state_move_down_first_to_second() {
        // Arrange — "abc\ndef", cursor on 'b' (index 1)
        let mut state = InputState::with_text("abc\ndef".to_string());
        state.cursor = 1;

        // Act
        state.move_down();

        // Assert — column 1 on second line = 'e' (index 5)
        assert_eq!(state.cursor, 5);
    }

    #[test]
    fn test_input_state_move_down_clamps_column() {
        // Arrange — "abcd\nef", cursor on 'd' (index 3, column 3)
        let mut state = InputState::with_text("abcd\nef".to_string());
        state.cursor = 3;

        // Act — column 3, but second line only has 2 chars
        state.move_down();

        // Assert — clamped to end of second line
        assert_eq!(state.cursor, 7);
    }

    #[test]
    fn test_input_state_move_up_down_three_lines() {
        // Arrange — "ab\ncd\nef", cursor on 'e' (index 6)
        let mut state = InputState::with_text("ab\ncd\nef".to_string());
        state.cursor = 6;

        // Act — move up to second line
        state.move_up();

        // Assert — column 0 on second line = 'c' (index 3)
        assert_eq!(state.cursor, 3);

        // Act — move up to first line
        state.move_up();

        // Assert — column 0 on first line = 'a' (index 0)
        assert_eq!(state.cursor, 0);

        // Act — move down to second line
        state.move_down();

        // Assert — column 0 on second line = 'c' (index 3)
        assert_eq!(state.cursor, 3);

        // Act — move down to third line
        state.move_down();

        // Assert — column 0 on third line = 'e' (index 6)
        assert_eq!(state.cursor, 6);
    }

    #[test]
    fn test_help_context_list_keybindings() {
        // Arrange
        let context = HelpContext::List;

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(!bindings.is_empty());
        assert!(bindings.iter().any(|(key, _)| *key == "q"));
        assert!(bindings.iter().any(|(key, _)| *key == "?"));
    }

    #[test]
    fn test_help_context_view_keybindings() {
        // Arrange
        let context = HelpContext::View {
            session_id: "s1".to_string(),
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|(key, _)| *key == "d"));
        assert!(bindings.iter().any(|(key, _)| *key == "p"));
    }

    #[test]
    fn test_help_context_diff_keybindings() {
        // Arrange
        let context = HelpContext::Diff {
            session_id: "s1".to_string(),
            diff: "diff".to_string(),
            scroll_offset: 5,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|(key, _)| *key == "q / Esc"));
    }

    #[test]
    fn test_help_context_health_keybindings() {
        // Arrange
        let context = HelpContext::Health;

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|(key, _)| *key == "r"));
    }

    #[test]
    fn test_help_context_restore_mode_list() {
        // Arrange
        let context = HelpContext::List;

        // Act
        let mode = context.restore_mode();

        // Assert
        assert!(matches!(mode, AppMode::List));
    }

    #[test]
    fn test_help_context_restore_mode_view() {
        // Arrange
        let context = HelpContext::View {
            session_id: "s1".to_string(),
            scroll_offset: Some(10),
        };

        // Act
        let mode = context.restore_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(10),
            } if session_id == "s1"
        ));
    }

    #[test]
    fn test_help_context_restore_mode_diff() {
        // Arrange
        let context = HelpContext::Diff {
            session_id: "s1".to_string(),
            diff: "diff content".to_string(),
            scroll_offset: 3,
        };

        // Act
        let mode = context.restore_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::Diff {
                ref session_id,
                ref diff,
                scroll_offset: 3,
            } if session_id == "s1" && diff == "diff content"
        ));
    }

    #[test]
    fn test_help_context_restore_mode_health() {
        // Arrange
        let context = HelpContext::Health;

        // Act
        let mode = context.restore_mode();

        // Assert
        assert!(matches!(mode, AppMode::Health));
    }

    #[test]
    fn test_help_context_title_returns_keybindings() {
        // Arrange & Act & Assert
        assert_eq!(HelpContext::List.title(), "Keybindings");
        assert_eq!(HelpContext::Health.title(), "Keybindings");
    }
}
