use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use ratatui::style::Color;
use tokio_util::sync::CancellationToken;

use super::agent::{AgentModel, ReasoningLevel};
use crate::infra::agent::protocol::QuestionItem;
use crate::infra::channel::TurnPromptAttachment;

/// Folder name under a project root that stores Agentty session metadata.
pub const SESSION_DATA_DIR: &str = ".agentty";

/// High-level lifecycle state for one session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    New,
    InProgress,
    Review,
    /// Session is generating focused-review output while keeping the
    /// review-oriented shortcuts available and temporarily hiding rebase.
    AgentReview,
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
    /// Ordered list of all session statuses used for UI sizing and iteration.
    pub const ALL: [Status; 10] = [
        Status::New,
        Status::InProgress,
        Status::Review,
        Status::AgentReview,
        Status::Question,
        Status::Queued,
        Status::Rebasing,
        Status::Merging,
        Status::Done,
        Status::Canceled,
    ];

    /// Returns the UI color associated with this status.
    pub fn color(&self) -> Color {
        match self {
            Status::New => Color::DarkGray,
            Status::InProgress => Color::Yellow,
            Status::Review | Status::AgentReview => Color::LightBlue,
            Status::Question => Color::LightMagenta,
            Status::Queued => Color::LightCyan,
            Status::Rebasing | Status::Merging => Color::Cyan,
            Status::Done => Color::Green,
            Status::Canceled => Color::Red,
        }
    }

    /// Returns whether this status keeps the review shortcut set enabled.
    pub fn allows_review_actions(self) -> bool {
        matches!(self, Status::Review | Status::AgentReview)
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
                | (Status::Review, Status::AgentReview)
                | (Status::AgentReview, Status::Review)
                | (
                    Status::Review | Status::AgentReview | Status::Question,
                    Status::InProgress
                        | Status::Queued
                        | Status::Rebasing
                        | Status::Merging
                        | Status::Canceled
                )
                | (Status::Review | Status::AgentReview, Status::Done)
                | (
                    Status::Queued,
                    Status::Merging | Status::Review | Status::AgentReview
                )
                | (
                    Status::InProgress | Status::Rebasing,
                    Status::Review | Status::AgentReview | Status::Question
                )
                | (
                    Status::Merging,
                    Status::Done | Status::Review | Status::AgentReview
                )
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::New => write!(f, "New"),
            Status::InProgress => write!(f, "InProgress"),
            Status::Review => write!(f, "Review"),
            Status::AgentReview => write!(f, "AgentReview"),
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
            "AgentReview" => Ok(Status::AgentReview),
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
        let (added_lines, deleted_lines) = SessionStats::line_change_counts(diff);
        let changed_line_count =
            usize::try_from(added_lines.saturating_add(deleted_lines)).unwrap_or(usize::MAX);

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
    /// Pushes the session branch and creates or refreshes the forge review
    /// request for it.
    PublishPullRequest,
}

/// Launch action currently available for one persisted follow-up task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FollowUpTaskAction {
    /// Starts a new sibling session from the selected task text.
    Launch,
    /// Opens the already launched sibling session linked to the task.
    Open,
}

/// Auto-push state for one already-published session branch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PublishedBranchSyncStatus {
    /// No background sync push is currently active and the last push did not
    /// fail.
    #[default]
    Idle,
    /// A completed turn is currently pushing the published branch upstream.
    InProgress,
    /// The latest automatic push attempt updated the published branch.
    Succeeded,
    /// The latest automatic push attempt failed and left the branch stale.
    Failed,
}

/// Per-session usage and diff statistics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionStats {
    /// Added diff lines currently attributed to the session worktree.
    pub added_lines: u64,
    /// Deleted diff lines currently attributed to the session worktree.
    pub deleted_lines: u64,
    /// Input/prompt tokens consumed by this session.
    pub input_tokens: u64,
    /// Output/response tokens produced by this session.
    pub output_tokens: u64,
}

impl SessionStats {
    /// Counts added and deleted lines in one git patch while ignoring file
    /// header markers such as `+++` and `---`.
    pub fn line_change_counts(diff: &str) -> (u64, u64) {
        diff.lines()
            .fold((0_u64, 0_u64), |(added_lines, deleted_lines), line| {
                if line.starts_with('+') && !line.starts_with("+++") {
                    return (added_lines.saturating_add(1), deleted_lines);
                }

                if line.starts_with('-') && !line.starts_with("---") {
                    return (added_lines, deleted_lines.saturating_add(1));
                }

                (added_lines, deleted_lines)
            })
    }
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

/// Persisted read-only follow-up task rendered alongside one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFollowUpTask {
    /// Stable database identifier for the persisted follow-up task row.
    pub id: i64,
    /// Previously launched sibling session linked to this task, when one has
    /// already been created.
    pub launched_session_id: Option<String>,
    /// Stable display-order position persisted for this follow-up task.
    pub position: usize,
    /// User-visible task text emitted by the agent.
    pub text: String,
}

impl SessionFollowUpTask {
    /// Returns the action the session view should expose for this task.
    pub fn action(&self) -> FollowUpTaskAction {
        if self.launched_session_id.is_some() {
            return FollowUpTaskAction::Open;
        }

        FollowUpTaskAction::Launch
    }
}

/// In-memory snapshot of one persisted session row used by the UI and app
/// orchestration layers.
pub struct Session {
    /// Base branch used to create the session worktree.
    pub base_branch: String,
    /// Session creation timestamp (Unix seconds).
    pub created_at: i64,
    /// Ordered image attachments staged for the draft-session prompt stored in
    /// `prompt` while the session remains `New`.
    pub draft_attachments: Vec<TurnPromptAttachment>,
    /// Planned or active worktree folder path for this session.
    pub folder: PathBuf,
    /// Persisted read-only follow-up tasks emitted after the latest turn.
    pub follow_up_tasks: Vec<SessionFollowUpTask>,
    /// Stable session identifier.
    pub id: String,
    /// Unix timestamp when the current active-work interval started, if the
    /// session is presently accumulating `InProgress` time.
    pub in_progress_started_at: Option<i64>,
    /// Cumulative active-work time already completed by this session, in whole
    /// seconds.
    pub in_progress_total_seconds: i64,
    /// Whether the session was created through the explicit draft workflow
    /// from the sessions list.
    pub is_draft: bool,
    /// Agent model selected for this session.
    pub model: AgentModel,
    /// Captured output transcript.
    pub output: String,
    /// Human-readable project name associated with the session.
    pub project_name: String,
    /// Initial user prompt used to create the session.
    pub prompt: String,
    /// Session-scoped reasoning override selected through prompt slash
    /// commands.
    pub reasoning_level_override: Option<ReasoningLevel>,
    /// Upstream reference recorded after the latest successful branch publish,
    /// for example `origin/agentty/session-id`.
    pub published_upstream_ref: Option<String>,
    /// Background auto-push state for the already-published upstream branch.
    pub published_branch_sync_status: PublishedBranchSyncStatus,
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
    /// Optional persisted session summary text sourced from the raw agent
    /// `summary` payload, applied immediately from reducer events during
    /// review/question states and, once the session reaches `Done`,
    /// formatted with `# Summary` and `# Commit` sections using the canonical
    /// session commit message. This text is also reused as review-assist
    /// context.
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

    /// Returns whether the session should use staged-draft behavior before
    /// its first live turn starts.
    pub fn is_draft_session(&self) -> bool {
        self.is_draft
    }

    /// Returns whether the session currently has one or more staged draft
    /// prompts waiting for an explicit start action.
    pub fn has_staged_drafts(&self) -> bool {
        self.is_draft_session() && self.status == Status::New && !self.prompt.is_empty()
    }

    /// Returns whether session chat should render the cumulative active-work
    /// timer for this session.
    pub fn has_in_progress_timer(&self) -> bool {
        self.in_progress_total_seconds > 0 || self.in_progress_started_at.is_some()
    }

    /// Returns the reasoning level that will be used for the next turn.
    pub fn effective_reasoning_level(
        &self,
        default_reasoning_level: ReasoningLevel,
    ) -> ReasoningLevel {
        self.reasoning_level_override
            .unwrap_or(default_reasoning_level)
    }

    /// Returns whether this session currently inherits the project default
    /// reasoning level.
    pub fn uses_default_reasoning_level(&self) -> bool {
        self.reasoning_level_override.is_none()
    }

    /// Returns cumulative active-work time including any open `InProgress`
    /// interval measured at `wall_clock_unix_seconds`.
    pub fn in_progress_duration_seconds(&self, wall_clock_unix_seconds: i64) -> i64 {
        let open_interval_seconds = self.in_progress_started_at.map_or(0, |started_at| {
            wall_clock_unix_seconds.saturating_sub(started_at).max(0)
        });

        self.in_progress_total_seconds
            .saturating_add(open_interval_seconds)
    }

    /// Returns a short forge indicator suffix for the session list status
    /// column.
    ///
    /// The indicator reflects the most specific known forge state:
    /// - `↑` when the branch was pushed but no review request is linked.
    /// - `⊙ #N` when a linked review request is open.
    /// - `✓ #N` when a linked review request was merged.
    /// - `✗ #N` when a linked review request was closed without merge.
    /// - Empty when neither published nor linked.
    pub fn forge_indicator(&self) -> String {
        if let Some(review_request) = &self.review_request {
            let display_id = &review_request.summary.display_id;

            return match review_request.summary.state {
                ReviewRequestState::Open => format!("⊙ {display_id}"),
                ReviewRequestState::Merged => format!("✓ {display_id}"),
                ReviewRequestState::Closed => format!("✗ {display_id}"),
            };
        }

        if self.published_upstream_ref.is_some() {
            return "↑".to_string();
        }

        String::new()
    }

    /// Returns whether this session can trigger a forge review request sync.
    ///
    /// Sync is available when the session has a published branch or a linked
    /// review request and the status allows review actions.
    pub fn can_sync_review_request(&self) -> bool {
        let has_forge_context =
            self.published_upstream_ref.is_some() || self.review_request.is_some();

        has_forge_context && matches!(self.status, Status::Review | Status::AgentReview)
    }

    /// Returns one UI message describing the current published-branch sync
    /// state when the session tracks an upstream branch.
    pub fn published_branch_sync_message(&self) -> Option<&'static str> {
        self.published_upstream_ref.as_ref()?;

        match self.published_branch_sync_status {
            PublishedBranchSyncStatus::Idle => None,
            PublishedBranchSyncStatus::InProgress => {
                Some("Auto-pushing published branch after completed turn...")
            }
            PublishedBranchSyncStatus::Succeeded => {
                Some("Auto-pushed published branch after completed turn.")
            }
            PublishedBranchSyncStatus::Failed => Some("Published branch sync failed."),
        }
    }

    /// Returns the review-request publish action currently available in session
    /// view.
    pub fn publish_pull_request_action(&self) -> Option<PublishBranchAction> {
        self.status
            .allows_review_actions()
            .then_some(PublishBranchAction::PublishPullRequest)
    }

    /// Returns the follow-up task at `position`, when present.
    pub fn follow_up_task(&self, position: usize) -> Option<&SessionFollowUpTask> {
        self.follow_up_tasks
            .iter()
            .find(|task| task.position == position)
    }
}

/// Shared runtime handles for one active session worker.
pub struct SessionHandles {
    /// Per-turn cancellation token shared between the UI and the worker.
    ///
    /// The worker swaps in a fresh [`CancellationToken`] at the start of
    /// each turn. The UI calls `cancel()` on the current token to
    /// interrupt the running turn. Because each turn gets its own token,
    /// stale cancellations from previous turns cannot affect new work.
    pub cancel_token: Arc<Mutex<CancellationToken>>,
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
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
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

    /// Builds a minimal session fixture for reasoning-level tests.
    fn test_session(reasoning_level_override: Option<ReasoningLevel>) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: "session-id".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            reasoning_level_override,
            published_upstream_ref: None,
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        }
    }

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
    fn test_status_all_lists_every_supported_status_in_display_order() {
        // Arrange
        let expected_statuses = [
            Status::New,
            Status::InProgress,
            Status::Review,
            Status::AgentReview,
            Status::Question,
            Status::Queued,
            Status::Rebasing,
            Status::Merging,
            Status::Done,
            Status::Canceled,
        ];

        // Act
        let all_statuses = Status::ALL;

        // Assert
        assert_eq!(all_statuses, expected_statuses);
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
    fn test_status_transition_review_to_agent_review() {
        // Arrange
        let current_status = Status::Review;

        // Act
        let can_transition = current_status.can_transition_to(Status::AgentReview);

        // Assert
        assert!(can_transition);
    }

    #[test]
    fn test_status_allows_review_actions_for_agent_review() {
        // Arrange
        let status = Status::AgentReview;

        // Act
        let allows_review_actions = status.allows_review_actions();

        // Assert
        assert!(allows_review_actions);
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
    fn test_session_stats_line_change_counts_ignore_diff_headers() {
        // Arrange
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ \
                    b/src/lib.rs\n@@ -1,2 +1,3 @@\n-old line\n+new line\n+another line\n";

        // Act
        let (added_lines, deleted_lines) = SessionStats::line_change_counts(diff);

        // Assert
        assert_eq!(added_lines, 2);
        assert_eq!(deleted_lines, 1);
    }

    #[test]
    fn test_session_size_from_diff_counts_added_and_deleted_lines() {
        // Arrange
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1,2 @@\n-old line\n+new line\n+another line\n";

        // Act
        let session_size = SessionSize::from_diff(diff);

        // Assert
        assert_eq!(session_size, SessionSize::Xs);
    }

    #[test]
    /// Ensures sessions without an override inherit the provided default
    /// reasoning level.
    fn test_effective_reasoning_level_uses_default_when_override_is_missing() {
        // Arrange
        let session = test_session(None);

        // Act
        let effective_reasoning_level = session.effective_reasoning_level(ReasoningLevel::Medium);
        let uses_default_reasoning_level = session.uses_default_reasoning_level();

        // Assert
        assert_eq!(effective_reasoning_level, ReasoningLevel::Medium);
        assert!(uses_default_reasoning_level);
    }

    #[test]
    /// Ensures sessions with an override use that override instead of the
    /// provided default.
    fn test_effective_reasoning_level_prefers_session_override() {
        // Arrange
        let session = test_session(Some(ReasoningLevel::High));

        // Act
        let effective_reasoning_level = session.effective_reasoning_level(ReasoningLevel::Low);
        let uses_default_reasoning_level = session.uses_default_reasoning_level();

        // Assert
        assert_eq!(effective_reasoning_level, ReasoningLevel::High);
        assert!(!uses_default_reasoning_level);
    }

    #[test]
    /// Ensures clearing a session override restores inheritance from the
    /// provided default reasoning level.
    fn test_effective_reasoning_level_returns_to_default_after_override_is_cleared() {
        // Arrange
        let mut session = test_session(Some(ReasoningLevel::XHigh));
        session.reasoning_level_override = None;

        // Act
        let effective_reasoning_level = session.effective_reasoning_level(ReasoningLevel::Low);
        let uses_default_reasoning_level = session.uses_default_reasoning_level();

        // Assert
        assert_eq!(effective_reasoning_level, ReasoningLevel::Low);
        assert!(uses_default_reasoning_level);
    }

    #[test]
    fn test_forge_kind_from_str_github() {
        // Arrange
        let raw_forge_kind = "GitHub";

        // Act
        let forge_kind = raw_forge_kind
            .parse::<ForgeKind>()
            .expect("failed to parse review-request forge");

        // Assert
        assert_eq!(forge_kind, ForgeKind::GitHub);
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
    fn test_publish_pull_request_action_returns_publish_for_review_session() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: "session-id".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
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
        let action = session.publish_pull_request_action();

        // Assert
        assert_eq!(action, Some(PublishBranchAction::PublishPullRequest));
    }

    #[test]
    fn test_publish_pull_request_action_returns_publish_for_agent_review_session() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: "session-id".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::AgentReview,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let action = session.publish_pull_request_action();

        // Assert
        assert_eq!(action, Some(PublishBranchAction::PublishPullRequest));
    }

    #[test]
    fn test_publish_pull_request_action_returns_none_for_in_progress_session() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: "session-id".to_string(),
            in_progress_started_at: Some(60),
            in_progress_total_seconds: 120,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            reasoning_level_override: None,
            published_upstream_ref: Some("origin/agentty/session-id".to_string()),
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let action = session.publish_pull_request_action();

        // Assert
        assert_eq!(action, None);
    }

    #[test]
    fn test_publish_pull_request_action_returns_none_for_done_session() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: "session-id".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 180,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            reasoning_level_override: None,
            published_upstream_ref: Some("origin/agentty/session-id".to_string()),
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Done,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let action = session.publish_pull_request_action();

        // Assert
        assert_eq!(action, None);
    }

    #[test]
    fn test_has_in_progress_timer_returns_true_for_open_interval() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            in_progress_started_at: Some(120),
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let shows_timer = session.has_in_progress_timer();

        // Assert
        assert!(shows_timer);
    }

    #[test]
    fn test_in_progress_duration_seconds_accumulates_closed_and_open_intervals() {
        // Arrange
        let session = Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            in_progress_started_at: Some(200),
            in_progress_total_seconds: 90,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::InProgress,
            summary: None,
            title: None,
            updated_at: 0,
        };

        // Act
        let duration_seconds = session.in_progress_duration_seconds(260);

        // Assert
        assert_eq!(duration_seconds, 150);
    }

    // -- forge_indicator tests -----------------------------------------------

    #[test]
    fn test_forge_indicator_returns_open_symbol_with_display_id() {
        // Arrange
        let mut session = test_session(None);
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "agentty/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "feat".to_string(),
                web_url: String::new(),
            },
        });

        // Act
        let indicator = session.forge_indicator();

        // Assert
        assert_eq!(indicator, "⊙ #42");
    }

    #[test]
    fn test_forge_indicator_returns_merged_symbol_with_display_id() {
        // Arrange
        let mut session = test_session(None);
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#99".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "agentty/session-id".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "feat".to_string(),
                web_url: String::new(),
            },
        });

        // Act
        let indicator = session.forge_indicator();

        // Assert
        assert_eq!(indicator, "✓ #99");
    }

    #[test]
    fn test_forge_indicator_returns_closed_symbol_with_display_id() {
        // Arrange
        let mut session = test_session(None);
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#7".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "agentty/session-id".to_string(),
                state: ReviewRequestState::Closed,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "feat".to_string(),
                web_url: String::new(),
            },
        });

        // Act
        let indicator = session.forge_indicator();

        // Assert
        assert_eq!(indicator, "✗ #7");
    }

    #[test]
    fn test_forge_indicator_returns_arrow_for_published_branch_without_review_request() {
        // Arrange
        let mut session = test_session(None);
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());

        // Act
        let indicator = session.forge_indicator();

        // Assert
        assert_eq!(indicator, "↑");
    }

    #[test]
    fn test_forge_indicator_returns_empty_when_no_forge_context() {
        // Arrange
        let session = test_session(None);

        // Act
        let indicator = session.forge_indicator();

        // Assert
        assert_eq!(indicator, "");
    }

    #[test]
    fn test_forge_indicator_prefers_review_request_over_published_ref() {
        // Arrange
        let mut session = test_session(None);
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#10".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "agentty/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "feat".to_string(),
                web_url: String::new(),
            },
        });

        // Act
        let indicator = session.forge_indicator();

        // Assert
        assert_eq!(indicator, "⊙ #10");
    }

    // -- can_sync_review_request tests ---------------------------------------

    #[test]
    fn test_can_sync_review_request_true_for_review_with_published_ref() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Review;
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());

        // Act / Assert
        assert!(session.can_sync_review_request());
    }

    #[test]
    fn test_can_sync_review_request_true_for_agent_review_with_review_request() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::AgentReview;
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#1".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "agentty/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "feat".to_string(),
                web_url: String::new(),
            },
        });

        // Act / Assert
        assert!(session.can_sync_review_request());
    }

    #[test]
    fn test_can_sync_review_request_false_for_question_with_published_ref() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Question;
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());

        // Act / Assert
        assert!(!session.can_sync_review_request());
    }

    #[test]
    fn test_can_sync_review_request_false_for_in_progress() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::InProgress;
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());

        // Act / Assert
        assert!(!session.can_sync_review_request());
    }

    #[test]
    fn test_can_sync_review_request_false_for_done() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Done;
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());

        // Act / Assert
        assert!(!session.can_sync_review_request());
    }

    #[test]
    fn test_can_sync_review_request_false_without_forge_context() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Review;

        // Act / Assert
        assert!(!session.can_sync_review_request());
    }

    #[test]
    fn test_published_branch_sync_message_returns_in_progress_copy() {
        // Arrange
        let mut session = test_session(None);
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());
        session.published_branch_sync_status = PublishedBranchSyncStatus::InProgress;

        // Act
        let sync_message = session.published_branch_sync_message();

        // Assert
        assert_eq!(
            sync_message,
            Some("Auto-pushing published branch after completed turn...")
        );
    }

    #[test]
    fn test_published_branch_sync_message_returns_succeeded_copy() {
        // Arrange
        let mut session = test_session(None);
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());
        session.published_branch_sync_status = PublishedBranchSyncStatus::Succeeded;

        // Act
        let sync_message = session.published_branch_sync_message();

        // Assert
        assert_eq!(
            sync_message,
            Some("Auto-pushed published branch after completed turn.")
        );
    }

    #[test]
    fn test_published_branch_sync_message_returns_failed_copy() {
        // Arrange
        let mut session = test_session(None);
        session.published_upstream_ref = Some("origin/agentty/session-id".to_string());
        session.published_branch_sync_status = PublishedBranchSyncStatus::Failed;

        // Act
        let sync_message = session.published_branch_sync_message();

        // Assert
        assert_eq!(sync_message, Some("Published branch sync failed."));
    }

    // -- status transition: Review/AgentReview/Question → Done ---------------

    #[test]
    fn test_status_transition_review_to_done() {
        // Arrange
        let current_status = Status::Review;

        // Act
        let can_transition = current_status.can_transition_to(Status::Done);

        // Assert
        assert!(can_transition);
    }

    #[test]
    fn test_status_transition_agent_review_to_done() {
        // Arrange
        let current_status = Status::AgentReview;

        // Act
        let can_transition = current_status.can_transition_to(Status::Done);

        // Assert
        assert!(can_transition);
    }

    #[test]
    fn test_status_transition_question_to_done_rejected() {
        // Arrange
        let current_status = Status::Question;

        // Act
        let can_transition = current_status.can_transition_to(Status::Done);

        // Assert
        assert!(!can_transition);
    }
}
