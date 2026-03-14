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
pub use ag_forge::ForgeKind;
/// Normalized remote lifecycle state for one linked review request.
pub use ag_forge::ReviewRequestState;
/// Normalized remote summary for one linked review request.
pub use ag_forge::ReviewRequestSummary;

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

/// Session-view action currently available for manual session-branch
/// publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishBranchAction {
    /// Pushes the session branch to the configured Git remote.
    Push,
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
    /// Upstream reference recorded after the latest successful branch publish,
    /// for example `origin/agentty/session-id`.
    pub published_upstream_ref: Option<String>,
    /// Model clarification questions emitted by the agent.
    pub questions: Vec<QuestionItem>,
    /// Persisted forge review-request link for this session, when available.
    pub review_request: Option<ReviewRequest>,
    /// Derived size bucket computed from diff size.
    pub size: SessionSize,
    /// Token usage statistics associated with this session.
    pub stats: SessionStats,
    /// Current lifecycle status.
    pub status: Status,
    /// Optional persisted summary payload rendered by the UI and reused as
    /// review-assist context.
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

    /// Returns the session-branch action currently available in session view.
    pub fn publish_branch_action(&self) -> Option<PublishBranchAction> {
        (self.status == Status::Review).then_some(PublishBranchAction::Push)
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

    #[test]
    fn test_publish_branch_action_returns_push_for_review_session() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let action = session.publish_branch_action();

        // Assert
        assert_eq!(action, Some(PublishBranchAction::Push));
    }

    #[test]
    fn test_publish_branch_action_returns_none_for_in_progress_session() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            published_upstream_ref: Some("origin/agentty/session-id".to_string()),
            questions: Vec::new(),
            review_request: Some(ReviewRequest {
                last_refreshed_at: 0,
                summary: ReviewRequestSummary {
                    display_id: "#42".to_string(),
                    forge_kind: ForgeKind::GitHub,
                    source_branch: "agentty/session-id".to_string(),
                    state: ReviewRequestState::Open,
                    status_summary: None,
                    target_branch: "main".to_string(),
                    title: "Review request".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                },
            }),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let action = session.publish_branch_action();

        // Assert
        assert_eq!(action, None);
    }

    #[test]
    fn test_publish_branch_action_returns_none_for_done_session() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            published_upstream_ref: Some("origin/agentty/session-id".to_string()),
            questions: Vec::new(),
            review_request: Some(ReviewRequest {
                last_refreshed_at: 0,
                summary: ReviewRequestSummary {
                    display_id: "#42".to_string(),
                    forge_kind: ForgeKind::GitHub,
                    source_branch: "agentty/session-id".to_string(),
                    state: ReviewRequestState::Open,
                    status_summary: None,
                    target_branch: "main".to_string(),
                    title: "Review request".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                },
            }),
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Done,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let action = session.publish_branch_action();

        // Assert
        assert_eq!(action, None);
    }
}
