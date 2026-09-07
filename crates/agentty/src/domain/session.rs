use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use ag_agent::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};
pub use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId, SessionRole,
    SessionStatus as Status, activity_day_key_with_offset,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use super::agent::{AgentSelection, ReasoningLevel};
use super::session_message::SessionTranscript;
use crate::domain::question::QuestionItem;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageBody, TransientMessageSlot, TransientMessageStore,
};
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment};

/// Folder name under a project root that stores Agentty session metadata.
pub const SESSION_DATA_DIR: &str = ".agentty";

/// Maximum number of stacked descendants in one root-to-child chain.
pub const MAX_STACK_DEPTH: usize = 5;

/// Full in-progress loader label shown while post-turn commit-message
/// generation and git commit orchestration are running.
pub(crate) const COMMITTING_PROGRESS_LABEL: &str = "Committing...";

/// Lead sentence used when seeding a follow-on prompt from a terminal session.
const TERMINAL_CONTINUATION_PROMPT_INTRO: &str =
    "Continue the work from this previous Agentty session.";

/// Size bucket derived from a session's git diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SessionSize {
    /// At most 10 changed lines.
    #[default]
    Xs,
    /// Between 11 and 30 changed lines.
    S,
    /// Between 31 and 80 changed lines.
    M,
    /// Between 81 and 200 changed lines.
    L,
    /// Between 201 and 500 changed lines.
    Xl,
    /// More than 500 changed lines.
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

/// Result of refreshing diff-derived metadata for one session worktree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionDiffStats {
    /// The worktree diff was loaded successfully.
    Known {
        /// Added line count parsed from the diff.
        added_lines: u64,
        /// Deleted line count parsed from the diff.
        deleted_lines: u64,
        /// Whether the diff contains any content, including binary-only or
        /// metadata-only changes.
        has_diff: bool,
        /// Size bucket derived from text line changes.
        session_size: SessionSize,
    },
    /// The worktree diff could not be loaded.
    Unknown,
}

impl SessionDiffStats {
    /// Derives known diff metadata from one successful Git diff response.
    pub fn from_diff(diff: &str) -> Self {
        let (added_lines, deleted_lines) = SessionStats::line_change_counts(diff);

        Self::Known {
            added_lines,
            deleted_lines,
            has_diff: !diff.trim().is_empty(),
            session_size: SessionSize::from_diff(diff),
        }
    }

    /// Returns the UI availability state represented by this refresh result.
    pub fn diff_state(self) -> SessionDiffState {
        match self {
            Self::Known { has_diff: true, .. } => SessionDiffState::Present,
            Self::Known {
                has_diff: false, ..
            } => SessionDiffState::Empty,
            Self::Unknown => SessionDiffState::Unknown,
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
    pub launched_session_id: Option<SessionId>,
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
    /// Agent provider and model selected for this session.
    pub agent: AgentSelection,
    /// Base branch used to create the session worktree.
    pub base_branch: String,
    /// Session creation timestamp (Unix seconds).
    pub created_at: i64,
    /// Controller session that owns this orchestration child, when present.
    pub controller_session_id: Option<SessionId>,
    /// Ordered image attachments staged for the draft-session prompt stored in
    /// `prompt` while the session remains `Draft`.
    pub draft_attachments: Vec<TurnPromptAttachment>,
    /// Planned or active worktree folder path for this session.
    pub folder: PathBuf,
    /// Persisted read-only follow-up tasks emitted after the latest turn.
    pub follow_up_tasks: Vec<SessionFollowUpTask>,
    /// Stable session identifier.
    pub id: SessionId,
    /// Unix timestamp when the current active-work interval started, if the
    /// session is presently accumulating `InProgress` time.
    pub in_progress_started_at: Option<i64>,
    /// Cumulative active-work time already completed by this session, in whole
    /// seconds.
    pub in_progress_total_seconds: i64,
    /// Whether the session was created through the explicit draft workflow
    /// from the sessions list.
    pub is_draft: bool,
    /// Derived orchestration progress rendered in place of a lifecycle label.
    pub orchestration_progress: Option<String>,
    /// Parent session this stacked session is based on while its parent branch
    /// remains active.
    pub parent_session_id: Option<SessionId>,
    /// Provider permission mode selected through the prompt shortcut.
    pub permission_mode: crate::domain::permission::PermissionMode,
    /// Workspace personality selected for future turns, when present.
    pub personality_id: Option<String>,
    /// Human-readable project name associated with the session.
    pub project_name: String,
    /// Initial user prompt used to create the session.
    pub prompt: String,
    /// Chat messages queued while the active turn is running, mirrored from
    /// [`SessionHandles::queued_messages`] for render in submission order
    /// alongside queued workflow actions.
    pub queued_messages: Vec<QueuedMessage>,
    /// Session-scoped reasoning override selected through prompt slash
    /// commands.
    pub reasoning_level_override: Option<ReasoningLevel>,
    /// Presentation style requested for future model responses.
    pub response_style: ResponseStyle,
    /// Upstream reference recorded after the latest successful branch publish,
    /// for example `origin/wt/session-id`.
    pub published_upstream_ref: Option<String>,
    /// Model clarification questions emitted by the agent.
    pub questions: Vec<QuestionItem>,
    /// Persisted forge review-request link for this session, when available.
    pub review_request: Option<ReviewRequest>,
    /// Role this session plays in multi-session orchestration.
    pub role: SessionRole,
    /// Derived size bucket computed from diff size.
    pub size: SessionSize,
    /// Response-speed preference selected through `/speed`.
    pub speed_mode: SpeedMode,
    /// Token usage statistics associated with this session.
    pub stats: SessionStats,
    /// Current lifecycle status.
    pub status: Status,
    /// Optional explicit session title.
    pub title: Option<String>,
    /// Typed transcript snapshot used by the UI when available.
    pub transcript: Option<SessionTranscript>,
    /// Last update timestamp (Unix seconds).
    pub updated_at: i64,
    /// Explicit non-durable output slots and their render lifecycle.
    pub(crate) transient_messages: TransientMessageStore,
}

impl Session {
    /// Returns the latest persisted user-prompt position for transient-message
    /// lifecycle binding.
    pub(crate) fn latest_user_prompt_position(&self) -> Option<i64> {
        self.transcript
            .as_ref()?
            .messages()
            .iter()
            .rev()
            .find_map(|message| message.kind.is_prompt().then_some(message.position))
    }

    /// Resolves turn-scoped messages when a snapshot leaves an active turn.
    pub(crate) fn reconcile_status_transition(&mut self, previous_status: Status) {
        if previous_status == Status::InProgress && self.status != Status::InProgress {
            self.transient_messages
                .retract(TransientMessageSlot::ReviewCommentResolution);
        }

        self.reconcile_transient_messages();
    }

    /// Applies turn-bound lifecycle cleanup after reducer-owned snapshot sync.
    pub(crate) fn reconcile_transient_messages(&mut self) {
        if matches!(self.status, Status::InProgress | Status::Queued)
            && let Some(active_turn_position) = self.latest_user_prompt_position()
        {
            self.transient_messages
                .clear_for_new_turn(active_turn_position);
        }
    }

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
        self.is_draft_session() && self.status == Status::Draft && !self.prompt.is_empty()
    }

    /// Returns whether this session can parent a stacked draft.
    ///
    /// Unstarted standalone drafts are excluded because their worktree branch
    /// is deferred until start. Unstarted stacked drafts can stage descendants
    /// against their deterministic future branch, though each descendant must
    /// still wait for its immediate parent to reach review before starting.
    /// Terminal sessions no longer provide an active branch to stack on. The
    /// stack-wide depth policy is evaluated separately because it requires the
    /// loaded session graph.
    pub fn allows_stacked_child_creation(&self) -> bool {
        if self.is_draft_session()
            && self.status == Status::Draft
            && self.parent_session_id.is_none()
        {
            return false;
        }

        !matches!(
            self.status,
            Status::Merged | Status::Done | Status::Canceled
        )
    }

    /// Returns whether this session can be moved beneath another session.
    ///
    /// Appending changes the branch base and immediately starts a sync, so the
    /// source must be an independent, review-ready user-owned branch without
    /// a forge review request whose target would become stale.
    pub fn allows_stack_append(&self) -> bool {
        self.accepts_user_turns()
            && self.owns_branch_changes()
            && self.parent_session_id.is_none()
            && self.review_request.is_none()
            && self.status.allows_review_actions()
    }

    /// Returns whether this session can be forked into a new independent
    /// session branch.
    ///
    /// Forks start from the current session branch and snapshot durable
    /// transcript history, so the source must be a root session with a
    /// materialized branch in a review-ready state. Drafts are excluded
    /// because their worktree may not exist yet, stacked children are excluded
    /// because they remain coupled to parent stack workflow, and non-review
    /// statuses are excluded because active branch work or terminal cleanup
    /// could race with the snapshot.
    pub fn allows_fork_action(&self) -> bool {
        self.accepts_user_turns()
            && self.role.owns_branch_changes()
            && self.parent_session_id.is_none()
            && !self.is_draft_session()
            && self.status.allows_review_actions()
    }

    /// Returns whether the session can submit an agent reply for actionable
    /// forge review comments.
    pub fn allows_review_comment_reply(&self) -> bool {
        self.accepts_user_turns()
            && self.role.owns_branch_changes()
            && (self.status.allows_review_actions() || self.status == Status::Question)
    }

    /// Returns whether the session lifecycle and ownership role permit opening
    /// its materialized worktree.
    ///
    /// Managed orchestration workers remain unavailable for direct user turns,
    /// but a settled worker in `Review` may be opened for external inspection.
    pub fn allows_worktree_open_action(&self) -> bool {
        self.status.allows_session_actions()
            && (self.accepts_user_turns()
                || (self.role == SessionRole::OrchestrationWorker && self.status == Status::Review))
    }

    /// Returns whether this session exposes branch diff, merge, and publish
    /// affordances.
    pub fn owns_branch_changes(&self) -> bool {
        self.role.owns_branch_changes()
    }

    /// Returns whether direct user turns and branch mutations are allowed.
    pub fn accepts_user_turns(&self) -> bool {
        self.role.accepts_user_turns()
    }

    /// Returns whether this session is owned by an orchestration campaign.
    pub fn is_managed(&self) -> bool {
        self.role.is_managed()
    }

    /// Returns whether this session is stacked beneath another session branch.
    pub fn is_stacked_child(&self) -> bool {
        self.parent_session_id.is_some()
    }

    /// Returns whether the staged draft bundle can start its first live turn.
    pub fn can_start_staged_session(&self) -> bool {
        self.is_draft_session() && self.status == Status::Draft && self.has_staged_drafts()
    }

    /// Returns whether the session can be canceled by the user.
    ///
    /// Running sessions can be canceled from the list after their active turn
    /// is signaled to stop. Review-oriented sessions remain cancelable, and
    /// unstarted draft sessions can also be canceled before they materialize a
    /// worktree. Draft orchestrators remain cancelable after their controller
    /// worktree is materialized but before their first goal is submitted.
    pub fn allows_cancel_action(&self) -> bool {
        self.accepts_user_turns()
            && (self.status == Status::InProgress
                || self.status.allows_review_actions()
                || (self.status == Status::Draft
                    && (self.is_draft_session()
                        || self.role == SessionRole::Orchestrator
                        || self
                            .transient_messages
                            .get(TransientMessageSlot::WorkspacePreparation)
                            .is_some())))
    }

    /// Returns whether this terminal session can launch a seeded follow-on
    /// session from view mode.
    pub fn allows_terminal_continuation(&self) -> bool {
        self.role == SessionRole::Worker && self.status.allows_terminal_continuation()
    }

    /// Returns one seeded first-prompt body for a follow-on session launched
    /// from a terminal session view.
    pub fn continuation_prompt_seed(&self) -> Option<String> {
        if !self.allows_terminal_continuation() {
            return None;
        }

        let (context_label, context_text) = self.continuation_context()?;

        Some(format!(
            "{TERMINAL_CONTINUATION_PROMPT_INTRO}\n\nPrevious session: {}\nProject: {}\nStatus: \
             {}\n\n{context_label}:\n{context_text}\n",
            self.display_title(),
            self.project_name,
            self.status,
        ))
    }

    /// Returns whether session chat should render the cumulative active-work
    /// timer for this session.
    pub fn has_in_progress_timer(&self) -> bool {
        self.in_progress_total_seconds > 0 || self.in_progress_started_at.is_some()
    }

    /// Returns the session-persisted reasoning level used for the next turn.
    pub fn effective_reasoning_level(&self) -> ReasoningLevel {
        self.reasoning_level_override.unwrap_or_default()
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

    /// Returns whether this session has a linked forge review request.
    pub fn has_review_request(&self) -> bool {
        self.review_request.is_some()
    }

    /// Returns whether this session can trigger a forge review request sync.
    ///
    /// Sync is available when the session has a published branch or a linked
    /// review request and the status allows review actions.
    pub fn can_sync_review_request(&self) -> bool {
        let has_forge_context = self.published_upstream_ref.is_some() || self.has_review_request();

        has_forge_context && matches!(self.status, Status::Review | Status::AgentReview)
    }

    /// Returns the review-request publish action currently available in session
    /// view, including queueing behind active turn or rebase work.
    pub fn publish_pull_request_action(&self) -> Option<PublishBranchAction> {
        let is_publish_active = self
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_some_and(|message| message.body.is_pending_indicator());

        (self.accepts_user_turns()
            && self.owns_branch_changes()
            && (self.status.allows_review_actions()
                || matches!(self.status, Status::InProgress | Status::Rebasing))
            && !is_publish_active)
            .then_some(PublishBranchAction::PublishPullRequest)
    }

    /// Returns the follow-up task at `position`, when present.
    pub fn follow_up_task(&self, position: usize) -> Option<&SessionFollowUpTask> {
        self.follow_up_tasks
            .iter()
            .find(|task| task.position == position)
    }

    /// Returns the best persisted context section for a continuation prompt.
    fn continuation_context(&self) -> Option<(&'static str, String)> {
        self.non_empty_transcript()
            .map(|transcript| ("Previous session transcript", transcript))
            .or_else(|| {
                self.non_empty_prompt()
                    .map(|prompt| ("Previous session prompt", prompt.to_string()))
            })
    }

    /// Returns the formatted transcript text when it is non-empty.
    fn non_empty_transcript(&self) -> Option<String> {
        self.transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .and_then(|transcript| {
                Self::trimmed_non_empty_text(&transcript).map(ToString::to_string)
            })
    }

    /// Returns the trimmed persisted initial prompt when it is non-empty.
    fn non_empty_prompt(&self) -> Option<&str> {
        Self::trimmed_non_empty_text(&self.prompt)
    }

    /// Returns `value` trimmed to a non-empty slice when any content remains.
    fn trimmed_non_empty_text(value: &str) -> Option<&str> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }
}

/// Returns whether `parent_session_id` can parent another stacked draft
/// without exceeding [`MAX_STACK_DEPTH`].
pub(crate) fn can_create_stacked_child(sessions: &[Session], parent_session_id: &str) -> bool {
    let Some(parent_session) = find_session(sessions, parent_session_id) else {
        return false;
    };

    parent_session.allows_stacked_child_creation()
        && session_stack_depth(sessions, parent_session_id)
            .is_some_and(|depth| depth < MAX_STACK_DEPTH)
}

/// Returns whether one review-ready root session can be moved beneath
/// `parent_session_id` and synchronized as a stacked child.
pub(crate) fn can_append_session_to_stack(
    sessions: &[Session],
    session_id: &str,
    parent_session_id: &str,
) -> bool {
    if session_id == parent_session_id {
        return false;
    }
    let Some(session) = find_session(sessions, session_id) else {
        return false;
    };
    let Some(parent_session) = find_session(sessions, parent_session_id) else {
        return false;
    };

    session.allows_stack_append()
        && !sessions.iter().any(|candidate| {
            candidate
                .parent_session_id
                .as_deref()
                .is_some_and(|parent_id| parent_id == session_id)
        })
        && parent_session.accepts_user_turns()
        && parent_session.owns_branch_changes()
        && parent_session.status.allows_review_actions()
        && can_create_stacked_child(sessions, parent_session_id)
        && can_rebase_session_branch_in_stack(sessions, parent_session_id)
}

/// Returns whether the staged draft identified by `session_id` can start
/// under the currently loaded stack.
///
/// Root drafts only need their own staged prompt state. Stacked drafts also
/// require a review-ready immediate parent and no other branch work already
/// running or queued in the same stack.
pub(crate) fn can_start_staged_session_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };
    let session = stack.requested_session();
    if !session.can_start_staged_session() {
        return false;
    }

    if session.parent_session_id.is_none() {
        return true;
    }
    if !stack.parent_allows_stacked_child_start() {
        return false;
    }

    !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether the session identified by `session_id` can start slash
/// command branch mutation while preserving one active branch worker per
/// stack.
///
/// This blocks parent branch edits once a child branch has materialized, and
/// blocks any stack member from starting branch work while a different member
/// is already running, queued, rebasing, merging, or waiting on a question.
pub(crate) fn can_mutate_session_branch_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    if stack.has_branch_mutating_member_except(session_id) {
        return false;
    }

    if stack.has_materialized_descendant() {
        return false;
    }

    true
}

/// Returns whether a session can enter the merge queue while preserving stack
/// consistency.
///
/// A linked forge review request disables local merge queueing so the remote
/// review remains the only merge path. Otherwise, merging a parent with idle
/// materialized children is allowed because the successful parent merge
/// retargets and syncs the children afterward. Active stack members still
/// block the request so the stack does not run competing branch work.
pub(crate) fn can_merge_session_branch_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    stack.requested_session.review_request.is_none()
        && !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether a session can start session sync while preserving stack
/// consistency.
///
/// Like merge, syncing a parent with idle materialized children is allowed
/// because the successful parent sync fans out child syncs afterward. Active
/// stack members still block the request so the stack does not run competing
/// branch work.
pub(crate) fn can_rebase_session_branch_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether a session can accept a chat reply under stack
/// constraints.
///
/// Replies are allowed when the stack has no other member actively running or
/// reserving branch work. Unlike merge or sync gates, an idle review-ready
/// materialized child does not block parent replies; the child can be synced
/// again after the parent produces its next review state.
pub(crate) fn can_reply_to_session_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether a caller-owned branch reservation belongs to this stack.
/// Missing or invalid stacks fail closed, matching the branch-work gates.
pub(crate) fn has_reserved_branch_work_in_stack(
    sessions: &[Session],
    session_id: &str,
    mut is_reserved: impl FnMut(&str) -> bool,
) -> bool {
    SessionStack::for_session(sessions, session_id)
        .is_none_or(|stack| stack.members.iter().any(|member| is_reserved(&member.id)))
}

/// Snapshot of one loaded stack tree for branch-work policy checks.
struct SessionStack<'a> {
    members: Vec<&'a Session>,
    requested_session: &'a Session,
}

impl<'a> SessionStack<'a> {
    /// Builds the stack containing `session_id` from the loaded session list.
    fn for_session(sessions: &'a [Session], session_id: &str) -> Option<Self> {
        let requested_session = find_session(sessions, session_id)?;
        let root_session = stack_root_session(sessions, requested_session)?;
        let members = sessions
            .iter()
            .filter(|session| session_stack_root_id(sessions, session) == Some(&root_session.id))
            .collect();

        Some(Self {
            members,
            requested_session,
        })
    }

    /// Returns the session whose action is being evaluated.
    fn requested_session(&self) -> &'a Session {
        self.requested_session
    }

    /// Returns whether another stack member is currently reserving or
    /// performing branch-mutating work.
    fn has_branch_mutating_member_except(&self, ignored_session_id: &str) -> bool {
        self.members
            .iter()
            .filter(|session| session.id.as_str() != ignored_session_id)
            .any(|session| session.status.is_stack_branch_mutating())
    }

    /// Returns whether the requested session has a non-terminal descendant
    /// branch that has started at least one live turn.
    fn has_materialized_descendant(&self) -> bool {
        self.members.iter().any(|session| {
            session.id != self.requested_session.id
                && session_is_descendant_of(
                    &self.members,
                    session,
                    self.requested_session.id.as_str(),
                )
                && !matches!(
                    session.status,
                    Status::Draft | Status::Merged | Status::Done | Status::Canceled
                )
        })
    }

    /// Returns whether the immediate parent is in a state that lets the
    /// requested stacked draft materialize.
    ///
    /// The caller handles root drafts before invoking this stacked-only gate.
    fn parent_allows_stacked_child_start(&self) -> bool {
        self.requested_session
            .parent_session_id
            .as_ref()
            .is_some_and(|parent_session_id| {
                self.members.iter().any(|session| {
                    session.id == *parent_session_id && session.status.allows_stacked_child_start()
                })
            })
    }
}

/// Returns the zero-based stack depth of a session, rejecting missing or
/// cyclic parent chains. Root sessions have depth zero.
fn session_stack_depth(sessions: &[Session], session_id: &str) -> Option<usize> {
    let mut current_session = find_session(sessions, session_id)?;
    let mut visited_session_ids = Vec::new();
    let mut depth = 0;

    while let Some(parent_session_id) = current_session.parent_session_id.as_ref() {
        if visited_session_ids.contains(&current_session.id) {
            return None;
        }
        visited_session_ids.push(current_session.id.clone());
        current_session = find_session(sessions, parent_session_id.as_str())?;
        depth += 1;
    }

    (!visited_session_ids.contains(&current_session.id)).then_some(depth)
}

/// Returns the root session for a valid loaded parent chain.
fn stack_root_session<'a>(sessions: &'a [Session], session: &'a Session) -> Option<&'a Session> {
    let depth = session_stack_depth(sessions, session.id.as_str())?;
    let mut root_session = session;

    for _ in 0..depth {
        root_session = find_session(sessions, root_session.parent_session_id.as_deref()?)?;
    }

    Some(root_session)
}

/// Returns the root id for a session with a valid loaded parent chain.
fn session_stack_root_id<'a>(
    sessions: &'a [Session],
    session: &'a Session,
) -> Option<&'a SessionId> {
    stack_root_session(sessions, session).map(|root_session| &root_session.id)
}

/// Returns whether `session` descends from `ancestor_session_id` within the
/// already-connected stack member set.
fn session_is_descendant_of(
    members: &[&Session],
    session: &Session,
    ancestor_session_id: &str,
) -> bool {
    let mut current_session = session;

    while let Some(parent_session_id) = current_session.parent_session_id.as_ref() {
        if parent_session_id.as_str() == ancestor_session_id {
            return true;
        }
        let Some(parent_session) = members
            .iter()
            .find(|candidate| candidate.id == *parent_session_id)
        else {
            return false;
        };
        current_session = parent_session;
    }

    false
}

/// Finds one loaded session by id.
fn find_session<'a>(sessions: &'a [Session], session_id: &str) -> Option<&'a Session> {
    sessions
        .iter()
        .find(|session| session.id.as_str() == session_id)
}

/// One chat prompt waiting behind active session work.
///
/// `order` comes from the same session-local sequence as queued workflow
/// actions, allowing the worker and renderer to preserve one FIFO order
/// across both kinds of work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedMessage {
    order: u64,
    prompt: TurnPrompt,
    transcript_text: String,
}

impl QueuedMessage {
    /// Creates one queued chat prompt at its reserved submission order.
    pub(crate) fn new(order: u64, prompt: TurnPrompt) -> Self {
        let transcript_text = prompt.transcript_text();

        Self {
            order,
            prompt,
            transcript_text,
        }
    }

    /// Consumes the queue entry and returns its structured prompt.
    pub(crate) fn into_prompt(self) -> TurnPrompt {
        self.prompt
    }

    /// Returns the session-local submission order shared with queued actions.
    pub(crate) fn order(&self) -> u64 {
        self.order
    }

    /// Returns the structured prompt without consuming the queue entry.
    pub(crate) fn prompt(&self) -> &TurnPrompt {
        &self.prompt
    }

    /// Returns the transcript rendering of the queued prompt.
    pub(crate) fn transcript_text(&self) -> &str {
        &self.transcript_text
    }
}

/// Shared runtime handles for one active session worker.
pub struct SessionHandles {
    /// Serializes branch-publish ownership with queued branch operations.
    ///
    /// The guard is held across async persistence and push work, so this is
    /// intentionally an async mutex rather than [`std::sync::Mutex`].
    pub branch_operation_lock: Arc<AsyncMutex<()>>,
    /// Per-turn cancellation token shared between the UI and the worker.
    ///
    /// The worker swaps in a fresh [`CancellationToken`] at the start of
    /// each turn. The UI calls `cancel()` on the current token to
    /// interrupt the running turn. Because each turn gets its own token,
    /// stale cancellations from previous turns cannot affect new work.
    pub cancel_token: Arc<Mutex<CancellationToken>>,
    /// Child process identifier for the running agent command, when present.
    pub child_pid: Arc<Mutex<Option<u32>>>,
    /// In-memory queue of prompts staged while the current turn is running.
    ///
    /// Pushed by the chat composer when the user submits while the session is
    /// `InProgress`; popped by the session worker between turns. The queue is
    /// session-local and discarded on app restart.
    pub queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    /// Monotonic submission order shared by queued chat and workflow actions.
    pub queued_work_sequence: Arc<AtomicU64>,
    /// Shared mutable status synchronized with persistence/UI.
    pub status: Arc<Mutex<Status>>,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub transcript: Arc<Mutex<SessionTranscript>>,
    /// Queued workflow rows that must survive active-project snapshot reloads.
    queued_actions: Arc<Mutex<TransientMessageStore>>,
    /// Whether [`Self::transcript`] contains the complete persisted history.
    ///
    /// Lazy session-list handles start unhydrated so background workflow
    /// notices cannot make a partial transcript look authoritative.
    transcript_is_hydrated: AtomicBool,
}

impl SessionHandles {
    /// Creates handles with a loaded, empty transcript.
    pub fn new(status: Status) -> Self {
        Self {
            branch_operation_lock: Arc::new(AsyncMutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            child_pid: Arc::new(Mutex::new(None)),
            queued_actions: Arc::new(Mutex::new(TransientMessageStore::default())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            queued_work_sequence: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(status)),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
            transcript_is_hydrated: AtomicBool::new(true),
        }
    }

    /// Creates handles whose persisted transcript has not been loaded yet.
    pub(crate) fn new_unloaded(status: Status) -> Self {
        Self {
            branch_operation_lock: Arc::new(AsyncMutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            child_pid: Arc::new(Mutex::new(None)),
            queued_actions: Arc::new(Mutex::new(TransientMessageStore::default())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            queued_work_sequence: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(status)),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
            transcript_is_hydrated: AtomicBool::new(false),
        }
    }

    /// Creates handles initialized with a typed transcript snapshot.
    pub fn new_with_transcript(status: Status, transcript: SessionTranscript) -> Self {
        Self {
            branch_operation_lock: Arc::new(AsyncMutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            child_pid: Arc::new(Mutex::new(None)),
            queued_actions: Arc::new(Mutex::new(TransientMessageStore::default())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            queued_work_sequence: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(status)),
            transcript: Arc::new(Mutex::new(transcript)),
            transcript_is_hydrated: AtomicBool::new(true),
        }
    }

    /// Returns the live transcript, hydrating an unloaded handle from the
    /// persisted snapshot even when background notices made it non-empty.
    pub(crate) fn transcript_snapshot_with_loaded(
        &self,
        loaded_transcript: Option<&SessionTranscript>,
    ) -> Option<SessionTranscript> {
        let Ok(mut transcript) = self.transcript.lock() else {
            return None;
        };
        if !self.transcript_is_hydrated.load(Ordering::Acquire)
            && let Some(loaded_transcript) = loaded_transcript
        {
            *transcript = Self::merge_unloaded_transcript(loaded_transcript, &transcript);
            self.transcript_is_hydrated.store(true, Ordering::Release);
        }
        if transcript.is_empty() {
            return None;
        }

        Some(transcript.clone())
    }

    /// Reserves the next shared submission order for queued session work.
    pub(crate) fn next_queued_work_order(&self) -> u64 {
        self.queued_work_sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns queued chat messages in submission order so callers can mirror
    /// queue contents into render snapshots.
    pub fn queued_message_snapshot(&self) -> Vec<QueuedMessage> {
        // Sync critical section (read-only clone, no `.await`);
        // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex
        // Selection".
        self.queued_messages
            .lock()
            .map(|guard| guard.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    /// Stores one queued workflow row beside the worker-owned queue state.
    pub(crate) fn upsert_queued_action(&self, message: TransientMessage) {
        debug_assert!(matches!(&message.body, TransientMessageBody::Queued(_)));
        if let Ok(mut queued_actions) = self.queued_actions.lock() {
            queued_actions.upsert(message);
        }
    }

    /// Removes one queued workflow row after its command starts or resolves.
    pub(crate) fn resolve_queued_action(&self, slot: TransientMessageSlot) {
        if let Ok(mut queued_actions) = self.queued_actions.lock() {
            queued_actions.retract(slot);
        }
    }

    /// Removes all queued workflow rows during terminal cancellation.
    pub(crate) fn clear_queued_actions(&self) {
        if let Ok(mut queued_actions) = self.queued_actions.lock() {
            *queued_actions = TransientMessageStore::default();
        }
    }

    /// Returns queued workflow rows in their stable display order.
    pub(crate) fn queued_action_snapshot(&self) -> Vec<TransientMessage> {
        self.queued_actions
            .lock()
            .map(|queued_actions| queued_actions.messages().to_vec())
            .unwrap_or_default()
    }

    /// Merges messages appended while persistence was in flight into a
    /// database snapshot, deduplicating exact matches and retaining conflicts.
    fn merge_unloaded_transcript(
        loaded_transcript: &SessionTranscript,
        live_transcript: &SessionTranscript,
    ) -> SessionTranscript {
        let mut messages = loaded_transcript.messages().to_vec();
        for live_message in live_transcript.messages() {
            if let Some(loaded_message) = messages
                .iter()
                .find(|message| message.position == live_message.position)
            {
                if loaded_message == live_message {
                    continue;
                }

                let next_position = messages
                    .last()
                    .map_or(0, |message| message.position.saturating_add(1));
                let mut appended_message = live_message.clone();
                appended_message.position = next_position;
                messages.push(appended_message);
            } else {
                messages.push(live_message.clone());
            }
        }

        SessionTranscript::new(messages)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::transient_message::{
        QueuedAction, TransientMessageAnchor, TransientMessageLifecycle,
    };
    use crate::test_support::SessionFixtureBuilder;

    #[test]
    fn test_activity_day_key_with_offset_applies_offsets_at_day_boundaries() {
        // Arrange
        let end_of_utc_day = 86_399_i64;
        let start_of_utc_day = 86_400_i64;

        // Act
        let positive_offset_day_key = activity_day_key_with_offset(end_of_utc_day, 3_600);
        let negative_offset_day_key = activity_day_key_with_offset(start_of_utc_day, -3_600);

        // Assert
        assert_eq!(positive_offset_day_key, 1);
        assert_eq!(negative_offset_day_key, 0);
    }

    #[test]
    fn test_transcript_snapshot_with_loaded_returns_none_for_poisoned_transcript_lock() {
        // Arrange
        let handles = SessionHandles::new_unloaded(Status::Review);
        let transcript = Arc::clone(&handles.transcript);
        let poison_transcript = |transcript: Arc<Mutex<SessionTranscript>>, should_poison: bool| {
            let _transcript = transcript
                .lock()
                .expect("transcript lock should initially be available");

            assert!(!should_poison, "poison transcript lock");
        };
        poison_transcript(Arc::clone(&transcript), false);
        let poison_result = std::thread::spawn(move || poison_transcript(transcript, true)).join();
        assert!(poison_result.is_err());

        // Act
        let snapshot = handles.transcript_snapshot_with_loaded(None);

        // Assert
        assert_eq!(snapshot, None);
    }

    #[test]
    fn queued_action_snapshot_tracks_updates_and_clear() {
        // Arrange
        let handles = SessionHandles::new(Status::InProgress);
        let branch_publish = TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Queued(QueuedAction::new(
                1,
                "publish after turn".to_string(),
            )),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::BranchPublish,
            turn_position: Some(0),
        };
        let sync = TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Queued(QueuedAction::new(2, "sync after turn".to_string())),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::SyncQueue,
            turn_position: Some(0),
        };

        // Act
        handles.upsert_queued_action(branch_publish.clone());
        handles.upsert_queued_action(sync.clone());
        let queued_actions = handles.queued_action_snapshot();
        handles.resolve_queued_action(TransientMessageSlot::BranchPublish);
        let after_resolve = handles.queued_action_snapshot();
        handles.clear_queued_actions();

        // Assert
        assert_eq!(queued_actions, vec![branch_publish, sync.clone()]);
        assert_eq!(after_resolve, vec![sync]);
        assert_eq!(handles.queued_action_snapshot(), []);
    }

    #[test]
    fn test_allows_stacked_child_creation_returns_true_for_materialized_active_session() {
        // Arrange
        let session = SessionFixtureBuilder::new()
            .draft(false)
            .status(Status::Review)
            .build();

        // Act
        let allows_stacked_child = session.allows_stacked_child_creation();

        // Assert
        assert!(allows_stacked_child);
    }

    #[test]
    fn test_stacked_child_creation_accepts_stacked_drafts_and_rejects_root_drafts_or_terminal() {
        // Arrange
        let draft_session = SessionFixtureBuilder::new()
            .draft(true)
            .status(Status::Draft)
            .build();
        let stacked_draft_session = SessionFixtureBuilder::new()
            .draft(true)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .status(Status::Draft)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .draft(true)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .status(Status::Review)
            .build();
        let done_session = SessionFixtureBuilder::new().status(Status::Done).build();
        let merged_session = SessionFixtureBuilder::new().status(Status::Merged).build();
        let canceled_session = SessionFixtureBuilder::new()
            .status(Status::Canceled)
            .build();

        // Act
        let allows_draft_child = draft_session.allows_stacked_child_creation();
        let allows_stacked_draft_child = stacked_draft_session.allows_stacked_child_creation();
        let allows_nested_child = child_session.allows_stacked_child_creation();
        let allows_merged_child = merged_session.allows_stacked_child_creation();
        let allows_done_child = done_session.allows_stacked_child_creation();
        let allows_canceled_child = canceled_session.allows_stacked_child_creation();

        // Assert
        assert!(!allows_draft_child);
        assert!(allows_stacked_draft_child);
        assert!(allows_nested_child);
        assert!(!allows_merged_child);
        assert!(!allows_done_child);
        assert!(!allows_canceled_child);
    }

    #[test]
    fn test_allows_stack_append_accepts_review_states_and_rejects_non_review_or_child() {
        // Arrange
        let review_session = SessionFixtureBuilder::new().status(Status::Review).build();
        let agent_review_session = SessionFixtureBuilder::new()
            .status(Status::AgentReview)
            .build();
        let running_session = SessionFixtureBuilder::new()
            .status(Status::InProgress)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .parent_session_id(Some(SessionId::from("parent-session")))
            .status(Status::Review)
            .build();

        // Act
        let allows_review = review_session.allows_stack_append();
        let allows_agent_review = agent_review_session.allows_stack_append();
        let allows_running = running_session.allows_stack_append();
        let allows_child = child_session.allows_stack_append();

        // Assert
        assert!(allows_review);
        assert!(allows_agent_review);
        assert!(!allows_running);
        assert!(!allows_child);
    }

    #[test]
    fn test_can_append_session_to_stack_accepts_idle_review_ready_root_and_parent() {
        // Arrange
        let source_session = SessionFixtureBuilder::new()
            .id("source-session")
            .status(Status::AgentReview)
            .build();
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .status(Status::Review)
            .build();
        let sessions = vec![source_session, parent_session];

        // Act
        let can_append = can_append_session_to_stack(&sessions, "source-session", "parent-session");

        // Assert
        assert!(can_append);
    }

    #[test]
    fn test_can_append_session_to_stack_rejects_invalid_ids_and_source_with_child() {
        // Arrange
        let source_session = SessionFixtureBuilder::new()
            .id("source-session")
            .status(Status::Review)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .parent_session_id(Some(SessionId::from("source-session")))
            .status(Status::Review)
            .build();
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .status(Status::Review)
            .build();
        let sessions = vec![source_session, child_session, parent_session];

        // Act
        let same_session =
            can_append_session_to_stack(&sessions, "source-session", "source-session");
        let missing_source =
            can_append_session_to_stack(&sessions, "missing-session", "parent-session");
        let missing_parent =
            can_append_session_to_stack(&sessions, "source-session", "missing-session");
        let source_with_child =
            can_append_session_to_stack(&sessions, "source-session", "parent-session");

        // Assert
        assert!(!same_session);
        assert!(!missing_source);
        assert!(!missing_parent);
        assert!(!source_with_child);
    }

    #[test]
    fn test_can_append_session_to_stack_rejects_active_parent_stack() {
        // Arrange
        let source_session = SessionFixtureBuilder::new()
            .id("source-session")
            .status(Status::Review)
            .build();
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .status(Status::Review)
            .build();
        let running_child = SessionFixtureBuilder::new()
            .id("running-child")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .status(Status::InProgress)
            .build();
        let sessions = vec![source_session, parent_session, running_child];

        // Act
        let can_append = can_append_session_to_stack(&sessions, "source-session", "parent-session");

        // Assert
        assert!(!can_append);
    }

    #[test]
    fn test_can_create_stacked_child_allows_depth_five_and_rejects_depth_six() {
        // Arrange
        let root_session = SessionFixtureBuilder::new()
            .id("root")
            .draft(false)
            .status(Status::Review)
            .build();
        let level_1 = SessionFixtureBuilder::new()
            .id("level-1")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("root")))
            .build();
        let level_2 = SessionFixtureBuilder::new()
            .id("level-2")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("level-1")))
            .build();
        let level_3 = SessionFixtureBuilder::new()
            .id("level-3")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("level-2")))
            .build();
        let level_4 = SessionFixtureBuilder::new()
            .id("level-4")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("level-3")))
            .build();
        let level_5 = SessionFixtureBuilder::new()
            .id("level-5")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("level-4")))
            .build();
        let sessions = vec![root_session, level_1, level_2, level_3, level_4, level_5];

        // Act
        let can_create_level_5 = can_create_stacked_child(&sessions, "level-4");
        let can_create_level_6 = can_create_stacked_child(&sessions, "level-5");

        // Assert
        assert!(can_create_level_5);
        assert!(!can_create_level_6);
    }

    #[test]
    fn test_can_create_stacked_child_rejects_missing_sessions_and_invalid_parent_chains() {
        // Arrange
        let missing_parent = SessionFixtureBuilder::new()
            .id("missing-parent-child")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("missing")))
            .build();
        let first_cycle_member = SessionFixtureBuilder::new()
            .id("cycle-a")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("cycle-b")))
            .build();
        let second_cycle_member = SessionFixtureBuilder::new()
            .id("cycle-b")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("cycle-a")))
            .build();
        let sessions = vec![missing_parent, first_cycle_member, second_cycle_member];

        // Act
        let missing_session_allowed = can_create_stacked_child(&sessions, "missing-session");
        let missing_parent_allowed = can_create_stacked_child(&sessions, "missing-parent-child");
        let cycle_allowed = can_create_stacked_child(&sessions, "cycle-a");

        // Assert
        assert!(!missing_session_allowed);
        assert!(!missing_parent_allowed);
        assert!(!cycle_allowed);
    }

    #[test]
    fn test_session_is_descendant_of_walks_nested_chain_and_rejects_missing_parent() {
        // Arrange
        let root_session = SessionFixtureBuilder::new().id("root-session").build();
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .parent_session_id(Some(SessionId::from("root-session")))
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let orphan_session = SessionFixtureBuilder::new()
            .id("orphan-session")
            .parent_session_id(Some(SessionId::from("missing-session")))
            .build();
        let members = vec![
            &root_session,
            &parent_session,
            &child_session,
            &orphan_session,
        ];

        // Act
        let child_is_descendant =
            session_is_descendant_of(&members, &child_session, "root-session");
        let orphan_is_descendant =
            session_is_descendant_of(&members, &orphan_session, "root-session");

        // Assert
        assert!(child_is_descendant);
        assert!(!orphan_is_descendant);
    }

    #[test]
    fn test_allows_fork_action_accepts_review_ready_materialized_sessions() {
        // Arrange
        let review_session = SessionFixtureBuilder::new()
            .draft(false)
            .status(Status::Review)
            .build();
        let agent_review_session = SessionFixtureBuilder::new()
            .draft(false)
            .status(Status::AgentReview)
            .build();

        // Act
        let allows_review_fork = review_session.allows_fork_action();
        let allows_agent_review_fork = agent_review_session.allows_fork_action();

        // Assert
        assert!(allows_review_fork);
        assert!(allows_agent_review_fork);
    }

    #[test]
    fn test_allows_fork_action_rejects_drafts_children_active_and_terminal_sessions() {
        // Arrange
        let draft_review_session = SessionFixtureBuilder::new()
            .draft(true)
            .status(Status::Review)
            .build();
        let child_review_session = SessionFixtureBuilder::new()
            .draft(false)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .status(Status::Review)
            .build();
        let in_progress_session = SessionFixtureBuilder::new()
            .draft(false)
            .status(Status::InProgress)
            .build();
        let done_session = SessionFixtureBuilder::new()
            .draft(false)
            .status(Status::Done)
            .build();
        let orchestrator_session = SessionFixtureBuilder::new()
            .draft(false)
            .role(SessionRole::Orchestrator)
            .status(Status::Review)
            .build();

        // Act
        let allows_draft_fork = draft_review_session.allows_fork_action();
        let allows_child_fork = child_review_session.allows_fork_action();
        let allows_active_fork = in_progress_session.allows_fork_action();
        let allows_done_fork = done_session.allows_fork_action();
        let allows_orchestrator_fork = orchestrator_session.allows_fork_action();

        // Assert
        assert!(!allows_draft_fork);
        assert!(!allows_child_fork);
        assert!(!allows_active_fork);
        assert!(!allows_done_fork);
        assert!(!allows_orchestrator_fork);
        assert!(!orchestrator_session.owns_branch_changes());
    }

    #[test]
    fn test_allows_review_comment_reply_accepts_review_or_question_sessions() {
        // Arrange
        let statuses = [Status::Review, Status::AgentReview, Status::Question];

        // Act
        let reply_permissions = statuses.map(|status| {
            SessionFixtureBuilder::new()
                .status(status)
                .build()
                .allows_review_comment_reply()
        });

        // Assert
        assert_eq!(reply_permissions, [true, true, true]);
    }

    #[test]
    fn test_allows_review_comment_reply_rejects_non_reply_or_managed_session() {
        // Arrange
        let session = SessionFixtureBuilder::new()
            .status(Status::InProgress)
            .build();
        let managed_session = SessionFixtureBuilder::new()
            .role(SessionRole::OrchestrationWorker)
            .status(Status::Review)
            .build();

        // Act
        let allows_reply = session.allows_review_comment_reply();
        let managed_allows_reply = managed_session.allows_review_comment_reply();

        // Assert
        assert!(!allows_reply);
        assert!(!managed_allows_reply);
    }

    #[test]
    fn test_allows_worktree_open_action_accepts_managed_worker_only_in_review() {
        // Arrange
        let statuses = [Status::InProgress, Status::Review, Status::AgentReview];

        // Act
        let open_permissions = statuses.map(|status| {
            SessionFixtureBuilder::new()
                .role(SessionRole::OrchestrationWorker)
                .status(status)
                .build()
                .allows_worktree_open_action()
        });
        let research_permission = SessionFixtureBuilder::new()
            .role(SessionRole::OrchestrationResearcher)
            .status(Status::Review)
            .build()
            .allows_worktree_open_action();

        // Assert
        assert_eq!(open_permissions, [false, true, false]);
        assert!(!research_permission);
    }

    #[test]
    fn test_can_start_staged_session_checks_only_draft_readiness() {
        // Arrange
        let root_draft_session = SessionFixtureBuilder::new()
            .draft(true)
            .status(Status::Draft)
            .prompt("Ready to start")
            .build();
        let stacked_draft_session = SessionFixtureBuilder::new()
            .draft(true)
            .status(Status::Draft)
            .prompt("Waiting on parent")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();

        // Act
        let can_start_root_draft = root_draft_session.can_start_staged_session();
        let can_start_stacked_draft = stacked_draft_session.can_start_staged_session();

        // Assert
        assert!(can_start_root_draft);
        assert!(can_start_stacked_draft);
    }

    #[test]
    fn test_can_start_staged_session_in_stack_requires_parent_review() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::InProgress)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Draft)
            .prompt("Ready child draft")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, child_session];

        // Act
        let can_start_child = can_start_staged_session_in_stack(&sessions, "child-session");

        // Assert
        assert!(!can_start_child);
    }

    #[test]
    fn test_branch_reservations_follow_stack_membership() {
        // Arrange
        let sessions = vec![
            SessionFixtureBuilder::new().id("root").build(),
            SessionFixtureBuilder::new()
                .id("child")
                .parent_session_id(Some(SessionId::from("root")))
                .build(),
            SessionFixtureBuilder::new()
                .id("grandchild")
                .parent_session_id(Some(SessionId::from("child")))
                .build(),
            SessionFixtureBuilder::new().id("unrelated").build(),
        ];

        // Act
        let root_is_reserved =
            has_reserved_branch_work_in_stack(&sessions, "root", |id| id == "grandchild");
        let child_is_reserved =
            has_reserved_branch_work_in_stack(&sessions, "child", |id| id == "root");
        let unrelated_is_reserved =
            has_reserved_branch_work_in_stack(&sessions, "unrelated", |id| id == "child");
        let missing_is_reserved =
            has_reserved_branch_work_in_stack(&sessions, "missing", |_| false);

        // Assert
        assert!(root_is_reserved);
        assert!(child_is_reserved);
        assert!(!unrelated_is_reserved);
        assert!(missing_is_reserved);
    }

    #[test]
    fn test_can_start_staged_session_in_stack_blocks_active_stack_member() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let running_child_session = SessionFixtureBuilder::new()
            .id("running-child")
            .draft(true)
            .status(Status::InProgress)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let staged_child_session = SessionFixtureBuilder::new()
            .id("staged-child")
            .draft(true)
            .status(Status::Draft)
            .prompt("Ready child draft")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, running_child_session, staged_child_session];

        // Act
        let can_start_child = can_start_staged_session_in_stack(&sessions, "staged-child");

        // Assert
        assert!(!can_start_child);
    }

    #[test]
    fn test_can_start_staged_session_in_stack_allows_review_ready_parent() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Draft)
            .prompt("Ready child draft")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, child_session];

        // Act
        let can_start_child = can_start_staged_session_in_stack(&sessions, "child-session");

        // Assert
        assert!(can_start_child);
    }

    #[test]
    fn test_can_start_nested_staged_session_uses_immediate_parent_review_state() {
        // Arrange
        let root_session = SessionFixtureBuilder::new()
            .id("root-session")
            .draft(false)
            .status(Status::Draft)
            .build();
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("root-session")))
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Draft)
            .prompt("Ready nested draft")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![root_session, parent_session, child_session];

        // Act
        let can_start_child = can_start_staged_session_in_stack(&sessions, "child-session");

        // Assert
        assert!(can_start_child);
    }

    #[test]
    fn test_can_mutate_session_branch_in_stack_blocks_parent_with_materialized_child() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, child_session];

        // Act
        let can_mutate_parent = can_mutate_session_branch_in_stack(&sessions, "parent-session");

        // Assert
        assert!(!can_mutate_parent);
    }

    #[test]
    fn test_can_mutate_session_branch_in_stack_blocks_parent_with_nested_descendant() {
        // Arrange
        let root_session = SessionFixtureBuilder::new()
            .id("root-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("root-session")))
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![root_session, parent_session, child_session];

        // Act
        let can_mutate_parent = can_mutate_session_branch_in_stack(&sessions, "parent-session");

        // Assert
        assert!(!can_mutate_parent);
    }

    #[test]
    fn test_can_merge_session_branch_in_stack_allows_parent_with_materialized_child() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, child_session];

        // Act
        let can_merge_parent = can_merge_session_branch_in_stack(&sessions, "parent-session");

        // Assert
        assert!(can_merge_parent);
    }

    #[test]
    fn test_can_merge_session_branch_in_stack_blocks_concurrent_stack_member() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let running_child_session = SessionFixtureBuilder::new()
            .id("running-child")
            .draft(true)
            .status(Status::InProgress)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let review_child_session = SessionFixtureBuilder::new()
            .id("review-child")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, running_child_session, review_child_session];

        // Act
        let can_merge_review_child = can_merge_session_branch_in_stack(&sessions, "review-child");

        // Assert
        assert!(!can_merge_review_child);
    }

    #[test]
    fn test_can_merge_session_branch_in_stack_blocks_linked_review_request() {
        // Arrange
        let review_request = ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Review request".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        };
        let session = SessionFixtureBuilder::new()
            .id("linked-session")
            .review_request(Some(review_request))
            .status(Status::Review)
            .build();
        let sessions = vec![session];

        // Act
        let can_merge_session = can_merge_session_branch_in_stack(&sessions, "linked-session");

        // Assert
        assert!(!can_merge_session);
    }

    #[test]
    fn test_can_mutate_session_branch_in_stack_blocks_concurrent_stack_member() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let running_child_session = SessionFixtureBuilder::new()
            .id("running-child")
            .draft(true)
            .status(Status::InProgress)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let review_child_session = SessionFixtureBuilder::new()
            .id("review-child")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, running_child_session, review_child_session];

        // Act
        let can_mutate_review_child = can_mutate_session_branch_in_stack(&sessions, "review-child");

        // Assert
        assert!(!can_mutate_review_child);
    }

    #[test]
    fn test_can_rebase_session_branch_in_stack_allows_parent_with_review_child() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, child_session];

        // Act
        let can_rebase_parent = can_rebase_session_branch_in_stack(&sessions, "parent-session");

        // Assert
        assert!(can_rebase_parent);
    }

    #[test]
    fn test_can_rebase_session_branch_in_stack_blocks_concurrent_stack_member() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let running_child_session = SessionFixtureBuilder::new()
            .id("running-child")
            .draft(true)
            .status(Status::InProgress)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let review_child_session = SessionFixtureBuilder::new()
            .id("review-child")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, running_child_session, review_child_session];

        // Act
        let can_rebase_review_child = can_rebase_session_branch_in_stack(&sessions, "review-child");

        // Assert
        assert!(!can_rebase_review_child);
    }

    #[test]
    fn test_can_reply_to_session_in_stack_allows_parent_with_review_child() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, child_session];

        // Act
        let can_reply_to_parent = can_reply_to_session_in_stack(&sessions, "parent-session");

        // Assert
        assert!(can_reply_to_parent);
    }

    #[test]
    fn test_can_reply_to_session_in_stack_blocks_active_stack_member() {
        // Arrange
        let parent_session = SessionFixtureBuilder::new()
            .id("parent-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let running_child_session = SessionFixtureBuilder::new()
            .id("running-child")
            .draft(true)
            .status(Status::InProgress)
            .parent_session_id(Some(SessionId::from("parent-session")))
            .build();
        let sessions = vec![parent_session, running_child_session];

        // Act
        let can_reply_to_parent = can_reply_to_session_in_stack(&sessions, "parent-session");

        // Assert
        assert!(!can_reply_to_parent);
    }

    #[test]
    fn test_can_reply_to_session_in_stack_blocks_active_nested_descendant() {
        // Arrange
        let root_session = SessionFixtureBuilder::new()
            .id("root-session")
            .draft(false)
            .status(Status::Review)
            .build();
        let child_session = SessionFixtureBuilder::new()
            .id("child-session")
            .draft(true)
            .status(Status::Review)
            .parent_session_id(Some(SessionId::from("root-session")))
            .build();
        let running_grandchild_session = SessionFixtureBuilder::new()
            .id("grandchild-session")
            .draft(true)
            .status(Status::InProgress)
            .parent_session_id(Some(SessionId::from("child-session")))
            .build();
        let sessions = vec![root_session, child_session, running_grandchild_session];

        // Act
        let can_reply_to_root = can_reply_to_session_in_stack(&sessions, "root-session");

        // Assert
        assert!(!can_reply_to_root);
    }

    /// Builds a minimal session fixture for reasoning-level tests.
    fn test_session(reasoning_level_override: Option<ReasoningLevel>) -> Session {
        SessionFixtureBuilder::new()
            .reasoning_level_override(reasoning_level_override)
            .build()
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
    fn test_status_from_str_draft() {
        // Arrange
        let raw_status = "Draft";

        // Act
        let status = raw_status
            .parse::<Status>()
            .expect("failed to parse status");

        // Assert
        assert_eq!(status, Status::Draft);
    }

    #[test]
    fn test_status_display_draft() {
        // Arrange
        let status = Status::Draft;

        // Act
        let displayed_status = status.to_string();

        // Assert
        assert_eq!(displayed_status, "Draft");
    }

    #[test]
    fn test_session_id_hash_map_borrowed_lookup() {
        // Arrange
        let session_id = SessionId::from("session-id");
        let sessions = HashMap::from([(session_id, "ready")]);

        // Act
        let status = sessions.get("session-id");

        // Assert
        assert_eq!(status, Some(&"ready"));
    }

    #[test]
    fn test_session_id_serde_serializes_as_plain_string() {
        // Arrange
        let session_id = SessionId::from("session-id");

        // Act
        let serialized_session_id =
            serde_json::to_string(&session_id).expect("session id should serialize");
        let deserialized_session_id: SessionId =
            serde_json::from_str(&serialized_session_id).expect("session id should deserialize");

        // Assert
        assert_eq!(serialized_session_id, "\"session-id\"");
        assert_eq!(deserialized_session_id, session_id);
    }

    #[test]
    fn test_status_all_lists_every_supported_status_in_display_order() {
        // Arrange
        let expected_statuses = [
            Status::Draft,
            Status::InProgress,
            Status::Review,
            Status::AgentReview,
            Status::Question,
            Status::Queued,
            Status::Rebasing,
            Status::Merging,
            Status::Merged,
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
    fn merged_status_is_read_only_and_only_transitions_to_done() {
        // Arrange
        let merged_status = Status::Merged;
        let review_status = Status::Review;

        // Act
        let merged_is_read_only = merged_status.is_read_only();
        let review_is_read_only = review_status.is_read_only();
        let review_can_merge = review_status.can_transition_to(merged_status);
        let merged_can_finish = merged_status.can_transition_to(Status::Done);
        let merged_can_repeat = merged_status.can_transition_to(Status::Merged);
        let merged_can_reopen = merged_status.can_transition_to(Status::Review);

        // Assert
        assert!(merged_is_read_only);
        assert!(!review_is_read_only);
        assert!(review_can_merge);
        assert!(merged_can_finish);
        assert!(merged_can_repeat);
        assert!(!merged_can_reopen);
    }

    #[test]
    fn test_latest_user_prompt_position_tracks_generated_turn() {
        // Arrange
        let mut session = SessionFixtureBuilder::new().status(Status::Review).build();
        session.transcript = Some(SessionTranscript::new(vec![
            crate::domain::session_message::SessionMessage::conversation(
                7,
                crate::domain::session_message::SessionMessageKind::AgentPrompt,
                "resolve comments",
            ),
        ]));

        // Act
        let latest_prompt_position = session.latest_user_prompt_position();

        // Assert
        assert_eq!(latest_prompt_position, Some(7));
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
    fn test_status_allows_session_actions_for_idle_interactive_states() {
        // Arrange
        let expected_statuses = [
            Status::Draft,
            Status::Review,
            Status::AgentReview,
            Status::Question,
        ];

        // Act
        let allowed_statuses: Vec<Status> = Status::ALL
            .into_iter()
            .filter(|status| status.allows_session_actions())
            .collect();

        // Assert
        assert_eq!(allowed_statuses, expected_statuses);
    }

    #[test]
    fn test_status_allows_chat_composer_during_idle_and_queueable_states() {
        // Arrange
        let expected_statuses = [
            Status::Draft,
            Status::InProgress,
            Status::Review,
            Status::AgentReview,
            Status::Question,
            Status::Rebasing,
        ];

        // Act
        let allowed_statuses: Vec<Status> = Status::ALL
            .into_iter()
            .filter(|status| status.allows_chat_composer())
            .collect();

        // Assert
        assert_eq!(allowed_statuses, expected_statuses);
    }

    #[test]
    fn test_status_allows_diff_view_for_review_ready_and_read_only_states() {
        // Arrange
        let expected_statuses = [Status::Review, Status::AgentReview, Status::Merged];

        // Act
        let allowed_statuses: Vec<Status> = Status::ALL
            .into_iter()
            .filter(|status| status.allows_diff_view())
            .collect();

        // Assert
        assert_eq!(allowed_statuses, expected_statuses);
    }

    #[test]
    fn test_status_allows_rebase_action_for_review_ready_and_in_progress_states() {
        // Arrange
        let expected_statuses = [Status::InProgress, Status::Review, Status::AgentReview];

        // Act
        let allowed_statuses: Vec<Status> = Status::ALL
            .into_iter()
            .filter(|status| status.allows_rebase_action())
            .collect();

        // Assert
        assert_eq!(allowed_statuses, expected_statuses);
    }

    #[test]
    fn test_status_allows_terminal_continuation_for_terminal_session_outcomes() {
        // Arrange
        let done_status = Status::Done;
        let canceled_status = Status::Canceled;
        let review_status = Status::Review;

        // Act
        let done_allows_continuation = done_status.allows_terminal_continuation();
        let canceled_allows_continuation = canceled_status.allows_terminal_continuation();
        let review_allows_continuation = review_status.allows_terminal_continuation();

        // Assert
        assert!(done_allows_continuation);
        assert!(canceled_allows_continuation);
        assert!(!review_allows_continuation);
    }

    #[test]
    fn test_status_transition_draft_to_canceled() {
        // Arrange
        let current_status = Status::Draft;

        // Act
        let can_transition = current_status.can_transition_to(Status::Canceled);

        // Assert
        assert!(can_transition);
    }

    #[test]
    fn test_status_transition_in_progress_to_canceled() {
        // Arrange
        let current_status = Status::InProgress;

        // Act
        let can_transition = current_status.can_transition_to(Status::Canceled);

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
    fn session_diff_stats_distinguish_empty_and_binary_diffs() {
        // Arrange
        let empty_diff = "";
        let binary_diff = "diff --git a/image.png b/image.png\nBinary files differ\n";

        // Act
        let empty_stats = SessionDiffStats::from_diff(empty_diff);
        let binary_stats = SessionDiffStats::from_diff(binary_diff);

        // Assert
        assert_eq!(empty_stats.diff_state(), SessionDiffState::Empty);
        assert_eq!(binary_stats.diff_state(), SessionDiffState::Present);
    }

    #[test]
    /// Ensures invalid rows without a stored value use the stable application
    /// fallback rather than the current project setting.
    fn test_effective_reasoning_level_uses_stable_fallback_when_value_is_missing() {
        // Arrange
        let session = test_session(None);

        // Act
        let effective_reasoning_level = session.effective_reasoning_level();
        // Assert
        assert_eq!(effective_reasoning_level, ReasoningLevel::High);
    }

    #[test]
    /// Ensures sessions with an override use that override instead of the
    /// provided default.
    fn test_effective_reasoning_level_prefers_session_override() {
        // Arrange
        let session = test_session(Some(ReasoningLevel::High));

        // Act
        let effective_reasoning_level = session.effective_reasoning_level();
        // Assert
        assert_eq!(effective_reasoning_level, ReasoningLevel::High);
    }

    #[test]
    /// Ensures clearing a session value uses the stable application fallback.
    fn test_effective_reasoning_level_uses_stable_fallback_after_value_is_cleared() {
        // Arrange
        let mut session = test_session(Some(ReasoningLevel::XHigh));
        session.reasoning_level_override = None;

        // Act
        let effective_reasoning_level = session.effective_reasoning_level();
        // Assert
        assert_eq!(effective_reasoning_level, ReasoningLevel::High);
    }

    #[test]
    fn test_session_continuation_prompt_seed_uses_transcript_for_terminal_session() {
        // Arrange
        let session = SessionFixtureBuilder::new()
            .status(Status::Done)
            .project_name("project-alpha")
            .transcript("assistant transcript")
            .title(Some("Terminal session".to_string()))
            .build();

        // Act
        let continuation_prompt_seed = session
            .continuation_prompt_seed()
            .expect("expected continuation prompt seed");

        // Assert
        assert!(continuation_prompt_seed.contains(TERMINAL_CONTINUATION_PROMPT_INTRO));
        assert!(continuation_prompt_seed.contains("Previous session: Terminal session"));
        assert!(continuation_prompt_seed.contains("Project: project-alpha"));
        assert!(continuation_prompt_seed.contains("Status: Done"));
        assert!(
            continuation_prompt_seed.contains("Previous session transcript:\nassistant transcript")
        );
    }

    #[test]
    fn test_session_continuation_prompt_seed_uses_transcript_for_canceled_session() {
        // Arrange
        let session = SessionFixtureBuilder::new()
            .status(Status::Canceled)
            .project_name("project-beta")
            .transcript("assistant transcript")
            .title(Some("Canceled session".to_string()))
            .build();

        // Act
        let continuation_prompt_seed = session
            .continuation_prompt_seed()
            .expect("expected canceled continuation prompt seed");

        // Assert
        assert!(continuation_prompt_seed.contains(TERMINAL_CONTINUATION_PROMPT_INTRO));
        assert!(continuation_prompt_seed.contains("Previous session: Canceled session"));
        assert!(continuation_prompt_seed.contains("Project: project-beta"));
        assert!(continuation_prompt_seed.contains("Status: Canceled"));
        assert!(
            continuation_prompt_seed.contains("Previous session transcript:\nassistant transcript")
        );
    }

    #[test]
    fn test_session_continuation_prompt_seed_rejects_non_terminal_session() {
        // Arrange
        let session = SessionFixtureBuilder::new().status(Status::Review).build();

        // Act
        let continuation_prompt_seed = session.continuation_prompt_seed();

        // Assert
        assert_eq!(continuation_prompt_seed, None);
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
    fn test_publish_pull_request_action_respects_review_session_capabilities() {
        // Arrange
        let worker = SessionFixtureBuilder::new().status(Status::Review).build();
        let orchestrator = SessionFixtureBuilder::new()
            .role(SessionRole::Orchestrator)
            .status(Status::Review)
            .build();
        let managed_worker = SessionFixtureBuilder::new()
            .role(SessionRole::OrchestrationWorker)
            .status(Status::Review)
            .build();

        // Act
        let actions = [
            worker.publish_pull_request_action(),
            orchestrator.publish_pull_request_action(),
            managed_worker.publish_pull_request_action(),
        ];

        // Assert
        assert_eq!(
            actions,
            [Some(PublishBranchAction::PublishPullRequest), None, None]
        );
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
            id: "session-id".into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            parent_session_id: None,
            permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
            personality_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::AgentReview,
            title: None,
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
        };

        // Act
        let action = session.publish_pull_request_action();

        // Assert
        assert_eq!(action, Some(PublishBranchAction::PublishPullRequest));
    }

    #[test]
    fn test_publish_pull_request_action_returns_none_while_publish_is_active() {
        for body in [
            TransientMessageBody::Queued(QueuedAction::new(
                0,
                "publish after this turn".to_string(),
            )),
            TransientMessageBody::Loading("Publishing review request...".to_string()),
        ] {
            // Arrange
            let mut session = crate::test_support::session_fixture("session-id", Status::Review);
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body,
                lifecycle: TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::BranchPublish,
                turn_position: None,
            });

            // Act
            let action = session.publish_pull_request_action();

            // Assert
            assert_eq!(action, None);
        }
    }

    #[test]
    fn test_publish_pull_request_action_queues_for_active_session() {
        // Arrange
        let sessions = [Status::InProgress, Status::Rebasing]
            .map(|status| SessionFixtureBuilder::new().status(status).build());

        // Act
        let actions = sessions.map(|session| session.publish_pull_request_action());

        // Assert
        assert_eq!(
            actions,
            [
                Some(PublishBranchAction::PublishPullRequest),
                Some(PublishBranchAction::PublishPullRequest),
            ]
        );
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
            id: "session-id".into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 180,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            parent_session_id: None,
            permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
            personality_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::Done,
            title: None,
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
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
            follow_up_tasks: Vec::new(),
            id: "session-id".into(),
            in_progress_started_at: Some(120),
            in_progress_total_seconds: 0,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            parent_session_id: None,
            permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
            personality_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::InProgress,
            title: None,
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
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
            follow_up_tasks: Vec::new(),
            id: "session-id".into(),
            in_progress_started_at: Some(200),
            in_progress_total_seconds: 90,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            parent_session_id: None,
            permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
            personality_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::InProgress,
            title: None,
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
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
                source_branch: "wt/session-id".to_string(),
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
                source_branch: "wt/session-id".to_string(),
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
                source_branch: "wt/session-id".to_string(),
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
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());

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
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#10".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
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
    fn test_has_review_request_reports_link_presence() {
        // Arrange
        let session_without_review_request = test_session(None);
        let mut session_with_review_request = test_session(None);
        session_with_review_request.review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#1".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "feat".to_string(),
                web_url: String::new(),
            },
        });

        // Act
        let has_no_link = session_without_review_request.has_review_request();
        let has_link = session_with_review_request.has_review_request();

        // Assert
        assert!(!has_no_link);
        assert!(has_link);
    }

    #[test]
    fn test_can_sync_review_request_true_for_review_with_published_ref() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Review;
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());

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
                source_branch: "wt/session-id".to_string(),
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
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());

        // Act / Assert
        assert!(!session.can_sync_review_request());
    }

    #[test]
    fn test_can_sync_review_request_false_for_in_progress() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::InProgress;
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());

        // Act / Assert
        assert!(!session.can_sync_review_request());
    }

    #[test]
    fn test_can_sync_review_request_false_for_done() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Done;
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());

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
    fn test_session_allows_cancel_action_for_unstarted_draft_session() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Draft;
        session.is_draft = true;

        // Act
        let allows_cancel_action = session.allows_cancel_action();

        // Assert
        assert!(allows_cancel_action);
    }

    #[test]
    fn test_session_allows_cancel_action_for_draft_orchestrator() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Draft;
        session.role = SessionRole::Orchestrator;

        // Act
        let allows_cancel_action = session.allows_cancel_action();

        // Assert
        assert!(allows_cancel_action);
    }

    #[test]
    fn test_session_allows_cancel_action_for_running_session() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::InProgress;

        // Act
        let allows_cancel_action = session.allows_cancel_action();

        // Assert
        assert!(allows_cancel_action);
    }

    #[test]
    fn test_session_allows_cancel_action_rejects_regular_draft_session() {
        // Arrange
        let mut session = test_session(None);
        session.status = Status::Draft;

        // Act
        let allows_cancel_action = session.allows_cancel_action();

        // Assert
        assert!(!allows_cancel_action);
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
