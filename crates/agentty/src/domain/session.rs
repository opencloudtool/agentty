use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

use super::agent::AgentModel;
use crate::infra::agent::protocol::QuestionItem;

/// Folder name under a project root that stores Agentty session metadata.
pub const SESSION_DATA_DIR: &str = ".agentty";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// High-level lifecycle state for one session.
pub enum Status {
    New,
    InProgress,
    Review,
    /// Session is waiting for model clarification responses.
    Question,
    /// Session is waiting in the merge queue for its turn to merge.
    Queued,
    Rebasing,
    Merging,
    Done,
    Canceled,
}

impl Status {
    /// Returns the UI color associated with this status.
    pub fn color(&self) -> Color {
        match self {
            Status::New => Color::DarkGray,
            Status::InProgress => Color::Yellow,
            Status::Review => Color::LightBlue,
            Status::Question => Color::LightMagenta,
            Status::Queued => Color::LightCyan,
            Status::Rebasing | Status::Merging => Color::Cyan,
            Status::Done => Color::Green,
            Status::Canceled => Color::Red,
        }
    }

    /// Returns whether a transition to `next` is valid.
    pub fn can_transition_to(self, next: Status) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (Status::New, Status::InProgress)
                | (Status::New | Status::InProgress, Status::Rebasing)
                | (
                    Status::Review | Status::Question,
                    Status::InProgress
                        | Status::Queued
                        | Status::Rebasing
                        | Status::Merging
                        | Status::Canceled
                )
                | (Status::Queued, Status::Merging | Status::Review)
                | (
                    Status::InProgress | Status::Rebasing,
                    Status::Review | Status::Question
                )
                | (Status::Merging, Status::Done | Status::Review)
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::New => write!(f, "New"),
            Status::InProgress => write!(f, "InProgress"),
            Status::Review => write!(f, "Review"),
            Status::Question => write!(f, "Question"),
            Status::Queued => write!(f, "Queued"),
            Status::Rebasing => write!(f, "Rebasing"),
            Status::Merging => write!(f, "Merging"),
            Status::Done => write!(f, "Done"),
            Status::Canceled => write!(f, "Canceled"),
        }
    }
}

impl FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "New" => Ok(Status::New),
            "InProgress" | "Committing" => Ok(Status::InProgress),
            "Review" => Ok(Status::Review),
            "Question" => Ok(Status::Question),
            "Queued" => Ok(Status::Queued),
            "Rebasing" => Ok(Status::Rebasing),
            "Merging" => Ok(Status::Merging),
            "Done" => Ok(Status::Done),
            "Canceled" => Ok(Status::Canceled),
            _ => Err(format!("Unknown status: {s}")),
        }
    }
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

impl SessionSize {
    /// Ordered list of all session size buckets from smallest to largest.
    pub const ALL: [SessionSize; 6] = [
        SessionSize::Xs,
        SessionSize::S,
        SessionSize::M,
        SessionSize::L,
        SessionSize::Xl,
        SessionSize::Xxl,
    ];

    /// Classifies one git diff into a session size bucket.
    pub fn from_diff(diff: &str) -> Self {
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

    /// Returns a short UI label for this size bucket.
    pub fn label(self) -> &'static str {
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

impl fmt::Display for SessionSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl FromStr for SessionSize {
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

/// Supported forge families for persisted session review-request links.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeKind {
    /// GitHub pull requests created through `gh`.
    GitHub,
    /// GitLab merge requests created through `glab`.
    GitLab,
}

impl ForgeKind {
    /// Returns the user-facing forge name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
        }
    }

    /// Returns the CLI executable name used for this forge.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::GitHub => "gh",
            Self::GitLab => "glab",
        }
    }

    /// Returns the login command users should run to authorize forge access.
    pub fn auth_login_command(self) -> &'static str {
        match self {
            Self::GitHub => "gh auth login",
            Self::GitLab => "glab auth login",
        }
    }

    /// Returns the persisted string representation for this forge kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
        }
    }
}

impl fmt::Display for ForgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ForgeKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "GitHub" => Ok(Self::GitHub),
            "GitLab" => Ok(Self::GitLab),
            _ => Err(format!("Unknown review-request forge: {value}")),
        }
    }
}

/// Normalized remote lifecycle state for one linked review request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewRequestState {
    /// The linked review request is still open.
    Open,
    /// The linked review request was merged upstream.
    Merged,
    /// The linked review request was closed without merge.
    Closed,
}

impl ReviewRequestState {
    /// Returns the persisted string representation for this remote state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Merged => "Merged",
            Self::Closed => "Closed",
        }
    }
}

impl fmt::Display for ReviewRequestState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReviewRequestState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Open" => Ok(Self::Open),
            "Merged" => Ok(Self::Merged),
            "Closed" => Ok(Self::Closed),
            _ => Err(format!("Unknown review-request state: {value}")),
        }
    }
}

/// Normalized remote summary for one linked review request.
///
/// Local session lifecycle transitions such as `Rebasing`, `Done`, and
/// `Canceled` retain this metadata so the session can continue to reference the
/// same remote review request. Remote terminal outcomes are stored in
/// `state` instead of clearing the link; only an explicit unlink action or
/// session deletion should remove this metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRequestSummary {
    /// Provider display id such as GitHub `#123` or GitLab `!42`.
    pub display_id: String,
    /// Forge family that owns the linked review request.
    pub forge_kind: ForgeKind,
    /// Source branch published for review.
    pub source_branch: String,
    /// Latest normalized remote lifecycle state.
    pub state: ReviewRequestState,
    /// Provider-specific condensed status text for UI display.
    pub status_summary: Option<String>,
    /// Target branch receiving the review request.
    pub target_branch: String,
    /// Remote review-request title.
    pub title: String,
    /// Browser-openable review-request URL.
    pub web_url: String,
}

/// Persisted forge linkage for one session.
///
/// This wraps the normalized remote summary with the last successful refresh
/// timestamp recorded by Agentty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRequest {
    /// Unix timestamp of the most recent successful refresh.
    pub last_refreshed_at: i64,
    /// Normalized remote summary captured at `last_refreshed_at`.
    pub summary: ReviewRequestSummary,
}

/// Per-session token statistics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionStats {
    /// Input/prompt tokens consumed by this session.
    pub input_tokens: u64,
    /// Output/response tokens produced by this session.
    pub output_tokens: u64,
}

/// Aggregated activity count for one day key.
///
/// `day_key` is the number of days since Unix epoch (`1970-01-01`).
/// App/session loading stores local day keys derived from immutable
/// session-creation activity history so heatmap remains visible after session
/// deletion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DailyActivity {
    /// Day key measured as whole days since Unix epoch.
    pub day_key: i64,
    /// Number of sessions created on the corresponding day.
    pub session_count: u32,
}

/// In-memory snapshot of one persisted session row used by the UI and app
/// orchestration layers.
pub struct Session {
    /// Base branch used to create the session worktree.
    pub base_branch: String,
    /// Session creation timestamp (Unix seconds).
    pub created_at: i64,
    /// Worktree folder path for this session.
    pub folder: PathBuf,
    /// Stable session identifier.
    pub id: String,
    /// Agent model selected for this session.
    pub model: AgentModel,
    /// Captured output transcript.
    pub output: String,
    /// Human-readable project name associated with the session.
    pub project_name: String,
    /// Initial user prompt used to create the session.
    pub prompt: String,
    /// Persisted forge review-request link for this session, when available.
    pub review_request: Option<ReviewRequest>,
    /// Model clarification questions emitted by the agent.
    pub questions: Vec<QuestionItem>,
    /// Derived size bucket computed from diff size.
    pub size: SessionSize,
    /// Token usage statistics associated with this session.
    pub stats: SessionStats,
    /// Current lifecycle status.
    pub status: Status,
    /// Optional summary generated for list display.
    pub summary: Option<String>,
    /// Optional explicit session title.
    pub title: Option<String>,
    /// Last update timestamp (Unix seconds).
    pub updated_at: i64,
}

impl Session {
    /// Returns the display title for this session.
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("No title")
    }
}

/// Shared runtime handles for one active session worker.
pub struct SessionHandles {
    /// Child process identifier for the running agent command, when present.
    pub child_pid: Arc<Mutex<Option<u32>>>,
    /// Shared output buffer mirrored to persistence/UI.
    pub output: Arc<Mutex<String>>,
    /// Shared mutable status synchronized with persistence/UI.
    pub status: Arc<Mutex<Status>>,
}

impl SessionHandles {
    /// Creates handles initialized with the given values.
    pub fn new(output: String, status: Status) -> Self {
        Self {
            child_pid: Arc::new(Mutex::new(None)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_from_str_queued() {
        // Arrange
        let raw_status = "Queued";

        // Act
        let status = raw_status
            .parse::<Status>()
            .expect("failed to parse status");

        // Assert
        assert_eq!(status, Status::Queued);
    }

    #[test]
    fn test_status_display_queued() {
        // Arrange
        let status = Status::Queued;

        // Act
        let displayed_status = status.to_string();

        // Assert
        assert_eq!(displayed_status, "Queued");
    }

    #[test]
    fn test_status_transition_review_to_queued() {
        // Arrange
        let current_status = Status::Review;

        // Act
        let can_transition = current_status.can_transition_to(Status::Queued);

        // Assert
        assert!(can_transition);
    }

    #[test]
    fn test_status_transition_queued_to_merging() {
        // Arrange
        let current_status = Status::Queued;

        // Act
        let can_transition = current_status.can_transition_to(Status::Merging);

        // Assert
        assert!(can_transition);
    }

    #[test]
    fn test_status_transition_queued_to_in_progress_is_rejected() {
        // Arrange
        let current_status = Status::Queued;

        // Act
        let can_transition = current_status.can_transition_to(Status::InProgress);

        // Assert
        assert!(!can_transition);
    }

    #[test]
    fn test_forge_kind_from_str_gitlab() {
        // Arrange
        let raw_forge_kind = "GitLab";

        // Act
        let forge_kind = raw_forge_kind
            .parse::<ForgeKind>()
            .expect("failed to parse review-request forge");

        // Assert
        assert_eq!(forge_kind, ForgeKind::GitLab);
    }

    #[test]
    fn test_review_request_state_display_merged() {
        // Arrange
        let review_request_state = ReviewRequestState::Merged;

        // Act
        let displayed_state = review_request_state.to_string();

        // Assert
        assert_eq!(displayed_state, "Merged");
    }
}
