use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

use crate::agent::AgentKind;
use crate::file_list::FileEntry;
use crate::icon::Icon;

pub const PLAN_MODE_INSTRUCTIONS: &str = include_str!("../resources/plan_mode.md");

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
            PermissionMode::Plan => Cow::Owned(format!(
                "[PLAN MODE] {PLAN_MODE_INSTRUCTIONS} Prompt: {prompt}"
            )),
            _ => Cow::Borrowed(prompt),
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl std::str::FromStr for PermissionMode {
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    New,
    InProgress,
    Review,
    Rebasing,
    Merging,
    CreatingPullRequest,
    PullRequest,
    Done,
    Canceled,
}

/// Size bucket derived from a session's git diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SessionSize {
    #[default]
    Xs,
    S,
    M,
    L,
    Xl,
    Xxl,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::New => write!(f, "New"),
            Status::InProgress => write!(f, "InProgress"),
            Status::Review => write!(f, "Review"),
            Status::Rebasing => write!(f, "Rebasing"),
            Status::Merging => write!(f, "Merging"),
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
            "Rebasing" => Ok(Status::Rebasing),
            "Merging" => Ok(Status::Merging),
            "CreatingPullRequest" => Ok(Status::CreatingPullRequest),
            "PullRequest" | "Processing" => Ok(Status::PullRequest),
            "Done" => Ok(Status::Done),
            "Canceled" => Ok(Status::Canceled),
            _ => Err(format!("Unknown status: {s}")),
        }
    }
}

impl SessionSize {
    pub(crate) const ALL: [SessionSize; 6] = [
        SessionSize::Xs,
        SessionSize::S,
        SessionSize::M,
        SessionSize::L,
        SessionSize::Xl,
        SessionSize::Xxl,
    ];

    pub(crate) fn from_diff(diff: &str) -> Self {
        let changed_line_count = diff
            .lines()
            .filter(|line| {
                (line.starts_with('+') && !line.starts_with("+++"))
                    || (line.starts_with('-') && !line.starts_with("---"))
            })
            .count();

        Self::from_changed_line_count(changed_line_count)
    }

    fn from_changed_line_count(changed_line_count: usize) -> Self {
        match changed_line_count {
            0..=10 => SessionSize::Xs,
            11..=30 => SessionSize::S,
            31..=80 => SessionSize::M,
            81..=200 => SessionSize::L,
            201..=500 => SessionSize::Xl,
            _ => SessionSize::Xxl,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SessionSize::Xs => "XS",
            SessionSize::S => "S",
            SessionSize::M => "M",
            SessionSize::L => "L",
            SessionSize::Xl => "XL",
            SessionSize::Xxl => "XXL",
        }
    }
}

impl std::fmt::Display for SessionSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl std::str::FromStr for SessionSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "XS" | "Xs" | "xs" => Ok(SessionSize::Xs),
            "S" | "s" => Ok(SessionSize::S),
            "M" | "m" => Ok(SessionSize::M),
            "L" | "l" => Ok(SessionSize::L),
            "XL" | "Xl" | "xl" => Ok(SessionSize::Xl),
            "XXL" | "Xxl" | "xxl" => Ok(SessionSize::Xxl),
            _ => Err(format!("Unknown session size: {s}")),
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

pub enum AppMode {
    List,
    ConfirmDeleteSession {
        session_id: String,
        session_title: String,
    },
    Prompt {
        at_mention_state: Option<PromptAtMentionState>,
        history_state: PromptHistoryState,
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
}

/// Captures which page opened the help overlay so it can be restored on close.
pub enum HelpContext {
    List,
    View {
        is_done: bool,
        session_id: String,
        scroll_offset: Option<u16>,
    },
    Diff {
        session_id: String,
        diff: String,
        scroll_offset: u16,
    },
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
                ("o", "Open"),
                ("Enter", "Open session"),
                ("Tab", "Switch tab"),
                ("/", "Command palette"),
                ("?", "Help"),
            ],
            HelpContext::View { is_done: true, .. } => {
                &[("q", "Quit"), ("j", "Scroll down"), ("k", "Scroll up")]
            }
            HelpContext::View { .. } => &[
                ("q", "Back to list"),
                ("Enter", "Reply"),
                ("Ctrl+c", "Stop agent"),
                ("d", "Show diff"),
                ("p", "Create PR"),
                ("m", "Merge"),
                ("r", "Rebase"),
                ("S-Tab", "Toggle permission mode"),
                ("g", "Scroll to top"),
                ("G", "Scroll to bottom"),
                ("Ctrl+d", "Half page down"),
                ("Ctrl+u", "Half page up"),
                ("?", "Help"),
            ],
            HelpContext::Diff { .. } => &[("q / Esc", "Back to session"), ("?", "Help")],
        }
    }

    /// Reconstructs the `AppMode` that was active before help was opened.
    pub fn restore_mode(self) -> AppMode {
        match self {
            HelpContext::List => AppMode::List,
            HelpContext::View {
                session_id,
                scroll_offset,
                ..
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
    Projects,
}

impl PaletteCommand {
    pub const ALL: &[PaletteCommand] = &[PaletteCommand::Projects];

    pub fn label(self) -> &'static str {
        match self {
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

/// Pure data model for a session, used across UI rendering and persistence.
///
/// Live runtime state (output streaming, status transitions, commit counting)
/// is managed separately via [`SessionHandles`] in `SessionState`.
pub struct Session {
    pub agent: String,
    pub base_branch: String,
    pub commit_count: i64,
    pub folder: PathBuf,
    pub id: String,
    pub model: String,
    pub output: String,
    pub permission_mode: PermissionMode,
    pub project_name: String,
    pub prompt: String,
    pub size: SessionSize,
    pub stats: SessionStats,
    pub status: Status,
    pub summary: Option<String>,
    pub title: Option<String>,
}

impl Session {
    /// Returns the display title for this session.
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("No title")
    }
}

/// Runtime communication handles shared between the UI and background tasks.
///
/// These `Arc<Mutex<...>>` wrappers allow concurrent writes from workers
/// while the UI reads snapshots via `SessionState::sync_from_handles()`.
pub struct SessionHandles {
    pub child_pid: Arc<Mutex<Option<u32>>>,
    pub commit_count: Arc<Mutex<i64>>,
    pub output: Arc<Mutex<String>>,
    pub status: Arc<Mutex<Status>>,
}

impl SessionHandles {
    /// Creates handles initialized with the given values.
    pub fn new(output: String, status: Status, commit_count: i64) -> Self {
        Self {
            child_pid: Arc::new(Mutex::new(None)),
            commit_count: Arc::new(Mutex::new(commit_count)),
            output: Arc::new(Mutex::new(output)),
            status: Arc::new(Mutex::new(status)),
        }
    }

    /// Appends text to the output buffer.
    pub fn append_output(&self, message: &str) {
        if let Ok(mut buf) = self.output.lock() {
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
                | (Status::New | Status::InProgress, Status::Rebasing)
                | (
                    Status::Review,
                    Status::InProgress
                        | Status::PullRequest
                        | Status::Rebasing
                        | Status::Merging
                        | Status::Canceled
                        | Status::CreatingPullRequest
                )
                | (Status::InProgress | Status::Rebasing, Status::Review)
                | (
                    Status::CreatingPullRequest,
                    Status::PullRequest | Status::Review
                )
                | (Status::Merging, Status::Done | Status::Review)
                | (Status::PullRequest, Status::Done | Status::Canceled)
        )
    }

    pub fn icon(self) -> Icon {
        match self {
            Status::New | Status::Review => Icon::Pending,
            Status::InProgress
            | Status::Rebasing
            | Status::Merging
            | Status::PullRequest
            | Status::CreatingPullRequest => Icon::current_spinner(),
            Status::Done => Icon::Check,
            Status::Canceled => Icon::Cross,
        }
    }

    pub fn color(self) -> Color {
        match self {
            Status::New => Color::DarkGray,
            Status::InProgress => Color::Yellow,
            Status::Review => Color::LightBlue,
            Status::Rebasing
            | Status::Merging
            | Status::PullRequest
            | Status::CreatingPullRequest => Color::Cyan,
            Status::Done => Color::Green,
            Status::Canceled => Color::Red,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_title() {
        // Arrange
        let session = Session {
            agent: "gemini".to_string(),
            base_branch: String::new(),
            commit_count: 0,
            folder: PathBuf::new(),
            id: "abc123".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: String::new(),
            permission_mode: PermissionMode::default(),
            project_name: String::new(),
            prompt: String::new(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::New,
            summary: None,
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
            commit_count: 0,
            folder: PathBuf::new(),
            id: "abc123".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: String::new(),
            permission_mode: PermissionMode::default(),
            project_name: String::new(),
            prompt: String::new(),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::New,
            summary: None,
            title: None,
        };

        // Act & Assert
        assert_eq!(session.display_title(), "No title");
    }

    #[test]
    fn test_session_handles_append_output() {
        // Arrange
        let handles = SessionHandles::new(String::new(), Status::Done, 0);

        // Act
        handles.append_output("[Test] Hello\n");

        // Assert
        let buf = handles.output.lock().expect("lock failed");
        assert_eq!(*buf, "[Test] Hello\n");
    }

    #[test]
    fn test_status_icon() {
        // Arrange & Act & Assert
        assert_eq!(Status::New.icon(), Icon::Pending);
        assert!(matches!(Status::InProgress.icon(), Icon::Spinner(_)));
        assert_eq!(Status::Review.icon(), Icon::Pending);
        assert!(matches!(Status::Merging.icon(), Icon::Spinner(_)));
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
        assert_eq!(Status::Merging.color(), Color::Cyan);
        assert_eq!(Status::CreatingPullRequest.color(), Color::Cyan);
        assert_eq!(Status::PullRequest.color(), Color::Cyan);
        assert_eq!(Status::Done.color(), Color::Green);
        assert_eq!(Status::Canceled.color(), Color::Red);
    }

    #[test]
    fn test_session_size_from_diff_uses_expected_buckets() {
        // Arrange
        let tiny_diff = "+line\n".repeat(10);
        let small_diff = "+line\n".repeat(11);
        let medium_diff = "+line\n".repeat(31);
        let large_diff = "+line\n".repeat(81);
        let extra_large_diff = "+line\n".repeat(201);
        let double_extra_large_diff = "+line\n".repeat(501);

        // Act & Assert
        assert_eq!(SessionSize::from_diff(&tiny_diff), SessionSize::Xs);
        assert_eq!(SessionSize::from_diff(&small_diff), SessionSize::S);
        assert_eq!(SessionSize::from_diff(&medium_diff), SessionSize::M);
        assert_eq!(SessionSize::from_diff(&large_diff), SessionSize::L);
        assert_eq!(SessionSize::from_diff(&extra_large_diff), SessionSize::Xl);
        assert_eq!(
            SessionSize::from_diff(&double_extra_large_diff),
            SessionSize::Xxl
        );
    }

    #[test]
    fn test_session_size_from_str() {
        // Arrange & Act & Assert
        assert_eq!(
            "XS".parse::<SessionSize>().expect("failed to parse"),
            SessionSize::Xs
        );
        assert_eq!(
            "S".parse::<SessionSize>().expect("failed to parse"),
            SessionSize::S
        );
        assert_eq!(
            "M".parse::<SessionSize>().expect("failed to parse"),
            SessionSize::M
        );
        assert_eq!(
            "L".parse::<SessionSize>().expect("failed to parse"),
            SessionSize::L
        );
        assert_eq!(
            "XL".parse::<SessionSize>().expect("failed to parse"),
            SessionSize::Xl
        );
        assert_eq!(
            "XXL".parse::<SessionSize>().expect("failed to parse"),
            SessionSize::Xxl
        );
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
    fn test_status_from_str_cancelled_is_unknown() {
        // Arrange & Act
        let result = "Cancelled".parse::<Status>();

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_status_transition_rules() {
        // Arrange & Act & Assert
        assert!(Status::New.can_transition_to(Status::InProgress));
        assert!(Status::New.can_transition_to(Status::Rebasing));
        assert!(!Status::New.can_transition_to(Status::Review));
        assert!(!Status::New.can_transition_to(Status::Done));
        assert!(Status::InProgress.can_transition_to(Status::Rebasing));
        assert!(Status::Review.can_transition_to(Status::InProgress));
        assert!(Status::Review.can_transition_to(Status::Merging));
        assert!(Status::Review.can_transition_to(Status::PullRequest));
        assert!(Status::Review.can_transition_to(Status::CreatingPullRequest));
        assert!(Status::Review.can_transition_to(Status::Canceled));
        assert!(Status::CreatingPullRequest.can_transition_to(Status::PullRequest));
        assert!(Status::CreatingPullRequest.can_transition_to(Status::Review));
        assert!(!Status::CreatingPullRequest.can_transition_to(Status::Canceled));
        assert!(Status::Merging.can_transition_to(Status::Done));
        assert!(Status::Merging.can_transition_to(Status::Review));
        assert!(!Status::Merging.can_transition_to(Status::PullRequest));
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
        assert_eq!(PaletteCommand::Projects.label(), "projects");
    }

    #[test]
    fn test_palette_command_all() {
        // Arrange & Act & Assert
        assert_eq!(PaletteCommand::ALL.len(), 1);
        assert_eq!(PaletteCommand::ALL[0], PaletteCommand::Projects);
    }

    #[test]
    fn test_palette_command_filter() {
        // Arrange & Act
        let results = PaletteCommand::filter("proj");

        // Assert
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], PaletteCommand::Projects);
    }

    #[test]
    fn test_palette_command_filter_case_insensitive() {
        // Arrange & Act
        let results = PaletteCommand::filter("PROJ");

        // Assert
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], PaletteCommand::Projects);
    }

    #[test]
    fn test_palette_command_filter_no_match() {
        // Arrange & Act
        let results = PaletteCommand::filter("xyz");

        // Assert
        assert!(results.is_empty());
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
    fn test_prompt_history_state_new() {
        // Arrange
        let entries = vec!["one".to_string(), "two".to_string()];

        // Act
        let state = PromptHistoryState::new(entries.clone());

        // Assert
        assert_eq!(state.entries, entries);
        assert_eq!(state.selected_index, None);
        assert_eq!(state.draft_text, None);
    }

    #[test]
    fn test_prompt_history_state_reset_navigation() {
        // Arrange
        let mut state = PromptHistoryState {
            draft_text: Some("draft".to_string()),
            entries: vec!["one".to_string()],
            selected_index: Some(0),
        };

        // Act
        state.reset_navigation();

        // Assert
        assert_eq!(state.entries, vec!["one".to_string()]);
        assert_eq!(state.selected_index, None);
        assert_eq!(state.draft_text, None);
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
            is_done: false,
            session_id: "s1".to_string(),
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|(key, _)| *key == "d"));
        assert!(bindings.iter().any(|(key, _)| *key == "p"));
        assert!(bindings.iter().any(|(key, _)| *key == "r"));
    }

    #[test]
    fn test_help_context_done_view_keybindings_only_quit_and_scroll() {
        // Arrange
        let context = HelpContext::View {
            is_done: true,
            session_id: "s1".to_string(),
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|(key, _)| *key == "q"));
        assert!(bindings.iter().any(|(key, _)| *key == "j"));
        assert!(bindings.iter().any(|(key, _)| *key == "k"));
        assert!(!bindings.iter().any(|(key, _)| *key == "Enter"));
        assert!(!bindings.iter().any(|(key, _)| *key == "d"));
        assert!(!bindings.iter().any(|(key, _)| *key == "p"));
        assert!(!bindings.iter().any(|(key, _)| *key == "m"));
        assert!(!bindings.iter().any(|(key, _)| *key == "r"));
        assert!(!bindings.iter().any(|(key, _)| *key == "?"));
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
            is_done: false,
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
    fn test_help_context_title_returns_keybindings() {
        // Arrange & Act & Assert
        assert_eq!(HelpContext::List.title(), "Keybindings");
        assert_eq!(
            HelpContext::Diff {
                session_id: "s1".to_string(),
                diff: "diff".to_string(),
                scroll_offset: 0,
            }
            .title(),
            "Keybindings"
        );
    }

    #[test]
    fn test_at_mention_query_at_start() {
        // Arrange
        let state = InputState::with_text("@foo".to_string());

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, Some((0, "foo".to_string())));
    }

    #[test]
    fn test_at_mention_query_after_space() {
        // Arrange
        let state = InputState::with_text("hello @bar".to_string());

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, Some((6, "bar".to_string())));
    }

    #[test]
    fn test_at_mention_query_empty_query() {
        // Arrange
        let state = InputState::with_text("hello @".to_string());

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, Some((6, String::new())));
    }

    #[test]
    fn test_at_mention_query_no_at_sign() {
        // Arrange
        let state = InputState::with_text("hello".to_string());

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_at_mention_query_email_pattern() {
        // Arrange
        let state = InputState::with_text("email@test".to_string());

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_at_mention_query_cursor_before_at() {
        // Arrange
        let mut state = InputState::with_text("hello @foo".to_string());
        state.cursor = 5; // cursor at "hello|"

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_at_mention_query_cursor_at_zero() {
        // Arrange
        let mut state = InputState::with_text("@foo".to_string());
        state.cursor = 0;

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_at_mention_query_with_path() {
        // Arrange
        let state = InputState::with_text("look at @src/main.rs".to_string());

        // Act
        let result = state.at_mention_query();

        // Assert
        assert_eq!(result, Some((8, "src/main.rs".to_string())));
    }

    #[test]
    fn test_replace_range_at_mention() {
        // Arrange
        let mut state = InputState::with_text("hello @que world".to_string());
        state.cursor = 10; // after "que"

        // Act
        state.replace_range(6, 10, "@src/main.rs ");

        // Assert
        assert_eq!(state.text(), "hello @src/main.rs  world");
        assert_eq!(state.cursor, 19); // after "@src/main.rs "
    }

    #[test]
    fn test_replace_range_updates_cursor() {
        // Arrange
        let mut state = InputState::with_text("@ab".to_string());

        // Act
        state.replace_range(0, 3, "@src/lib.rs ");

        // Assert
        assert_eq!(state.text(), "@src/lib.rs ");
        assert_eq!(state.cursor, 12);
    }

    #[test]
    fn test_permission_mode_default() {
        // Arrange & Act
        let mode = PermissionMode::default();

        // Assert
        assert_eq!(mode, PermissionMode::AutoEdit);
    }

    #[test]
    fn test_permission_mode_label() {
        // Arrange & Act & Assert
        assert_eq!(PermissionMode::AutoEdit.label(), "auto_edit");
        assert_eq!(PermissionMode::Autonomous.label(), "autonomous");
        assert_eq!(PermissionMode::Plan.label(), "plan");
    }

    #[test]
    fn test_permission_mode_display_label() {
        // Arrange & Act & Assert
        assert_eq!(PermissionMode::AutoEdit.display_label(), "Auto Edit");
        assert_eq!(PermissionMode::Autonomous.display_label(), "Autonomous");
        assert_eq!(PermissionMode::Plan.display_label(), "Plan");
    }

    #[test]
    fn test_permission_mode_toggle() {
        // Arrange & Act & Assert
        assert_eq!(
            PermissionMode::AutoEdit.toggle(),
            PermissionMode::Autonomous
        );
        assert_eq!(PermissionMode::Autonomous.toggle(), PermissionMode::Plan);
        assert_eq!(PermissionMode::Plan.toggle(), PermissionMode::AutoEdit);
    }

    #[test]
    fn test_permission_mode_display() {
        // Arrange & Act & Assert
        assert_eq!(PermissionMode::AutoEdit.to_string(), "auto_edit");
        assert_eq!(PermissionMode::Autonomous.to_string(), "autonomous");
        assert_eq!(PermissionMode::Plan.to_string(), "plan");
    }

    #[test]
    fn test_permission_mode_from_str() {
        // Arrange & Act & Assert
        assert_eq!(
            "auto_edit".parse::<PermissionMode>().expect("parse"),
            PermissionMode::AutoEdit
        );
        assert_eq!(
            "autonomous".parse::<PermissionMode>().expect("parse"),
            PermissionMode::Autonomous
        );
        assert_eq!(
            "plan".parse::<PermissionMode>().expect("parse"),
            PermissionMode::Plan
        );
        assert!("unknown".parse::<PermissionMode>().is_err());
    }

    #[test]
    fn test_permission_mode_roundtrip() {
        // Arrange & Act & Assert
        for mode in [
            PermissionMode::AutoEdit,
            PermissionMode::Autonomous,
            PermissionMode::Plan,
        ] {
            let label = mode.to_string();
            let parsed: PermissionMode = label.parse().expect("roundtrip parse");
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn test_permission_mode_apply_to_prompt_auto_edit() {
        // Arrange
        let prompt = "Fix the bug";

        // Act
        let result = PermissionMode::AutoEdit.apply_to_prompt(prompt);

        // Assert
        assert_eq!(result.as_ref(), "Fix the bug");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_permission_mode_apply_to_prompt_autonomous() {
        // Arrange
        let prompt = "Fix the bug";

        // Act
        let result = PermissionMode::Autonomous.apply_to_prompt(prompt);

        // Assert
        assert_eq!(result.as_ref(), "Fix the bug");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_permission_mode_apply_to_prompt_plan() {
        // Arrange
        let prompt = "Fix the bug";

        // Act
        let result = PermissionMode::Plan.apply_to_prompt(prompt);

        // Assert
        assert!(result.starts_with("[PLAN MODE]"));
        assert!(result.ends_with("Prompt: Fix the bug"));
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn test_plan_followup_action_label() {
        // Arrange & Act & Assert
        assert_eq!(
            PlanFollowupAction::ImplementPlan.label(),
            "Implement the plan"
        );
        assert_eq!(PlanFollowupAction::TypeFeedback.label(), "Type feedback");
    }

    #[test]
    fn test_plan_followup_action_next() {
        // Arrange & Act & Assert
        assert_eq!(
            PlanFollowupAction::ImplementPlan.next(),
            PlanFollowupAction::TypeFeedback
        );
        assert_eq!(
            PlanFollowupAction::TypeFeedback.next(),
            PlanFollowupAction::ImplementPlan
        );
    }

    #[test]
    fn test_plan_followup_action_previous() {
        // Arrange & Act & Assert
        assert_eq!(
            PlanFollowupAction::ImplementPlan.previous(),
            PlanFollowupAction::TypeFeedback
        );
        assert_eq!(
            PlanFollowupAction::TypeFeedback.previous(),
            PlanFollowupAction::ImplementPlan
        );
    }

    #[test]
    fn test_help_context_view_keybindings_include_ctrl_c() {
        // Arrange
        let context = HelpContext::View {
            is_done: false,
            session_id: "s1".to_string(),
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(
            bindings
                .iter()
                .any(|(key, desc)| *key == "Ctrl+c" && *desc == "Stop agent")
        );
    }

    #[test]
    fn test_help_context_done_view_keybindings_exclude_ctrl_c() {
        // Arrange
        let context = HelpContext::View {
            is_done: true,
            session_id: "s1".to_string(),
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(!bindings.iter().any(|(key, _)| *key == "Ctrl+c"));
    }

    #[test]
    fn test_help_context_view_keybindings_include_shift_tab() {
        // Arrange
        let context = HelpContext::View {
            is_done: false,
            session_id: "s1".to_string(),
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|(key, _)| *key == "S-Tab"));
    }
}
