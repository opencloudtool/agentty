//! Shared session identity, lifecycle, settings, and aggregate models.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use ag_agent::{AgentSelection, ReasoningLevel};
pub use ag_agent::{PermissionMode, ResponseStyle, SpeedMode};
pub use ag_forge::{ForgeKind, ReviewRequestState, ReviewRequestSummary};
use ag_protocol::QuestionItem;
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::SessionMessage;

/// Number of seconds in one activity-calendar day.
const SECONDS_PER_DAY: i64 = 86_400;

/// Shared stable identifier for one session.
///
/// Session identifiers are cloned heavily across maps, events, and worker
/// tasks. Wrapping the identifier in `Arc<str>` keeps those clones cheap while
/// retaining borrowed `&str` lookup support.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(Arc<str>);

impl SessionId {
    /// Returns the session identifier as a string slice.
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl AsRef<str> for SessionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<Path> for SessionId {
    fn as_ref(&self) -> &Path {
        Path::new(self.as_str())
    }
}

impl Borrow<str> for SessionId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Deref for SessionId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self(Arc::<str>::from(value))
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self(Arc::<str>::from(value))
    }
}

impl From<Arc<str>> for SessionId {
    fn from(value: Arc<str>) -> Self {
        Self(value)
    }
}

impl From<SessionId> for String {
    fn from(value: SessionId) -> Self {
        value.as_str().to_string()
    }
}

impl PartialEq<str> for SessionId {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SessionId {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for SessionId {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&String> for SessionId {
    fn eq(&self, other: &&String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Serialize for SessionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .map(Self::from)
            .map_err(de::Error::custom)
    }
}

/// Role one session plays in a multi-session workflow.
///
/// The role is orthogonal to [`SessionStatus`]: an orchestrator moves through
/// the same lifecycle states as any other session, but its worktree exists only
/// for repository reads and never receives commits. Diff, merge, and
/// review-request affordances are therefore gated on the role rather than on a
/// dedicated lifecycle state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionRole {
    /// Ordinary session that owns the changes on its own branch.
    #[default]
    Worker,
    /// Worker whose write capabilities are owned exclusively by an
    /// orchestration coordinator.
    OrchestrationWorker,
    /// Temporary read-only researcher whose worktree changes are discarded
    /// after its report is captured.
    OrchestrationResearcher,
    /// Controller session that plans and supervises child worker sessions.
    Orchestrator,
}

impl SessionRole {
    /// Returns whether this role produces commits on its own session branch.
    ///
    /// Orchestrators read the repository to plan work but delegate every edit
    /// to child sessions, so their branch stays empty for the session's whole
    /// lifetime.
    pub fn owns_branch_changes(self) -> bool {
        matches!(self, SessionRole::Worker | SessionRole::OrchestrationWorker)
    }

    /// Returns whether Agentty should observe worktree changes after a turn.
    ///
    /// Research children do not own their changes, but the coordinator records
    /// whether a child attempted edits before discarding its worktree.
    pub fn tracks_worktree_changes(self) -> bool {
        !matches!(self, SessionRole::Orchestrator)
    }

    /// Returns whether an end user may submit turns or branch mutations
    /// directly to this session.
    pub fn accepts_user_turns(self) -> bool {
        !self.is_managed()
    }

    /// Returns whether this worker is owned by an orchestration coordinator.
    pub fn is_managed(self) -> bool {
        matches!(
            self,
            SessionRole::OrchestrationWorker | SessionRole::OrchestrationResearcher
        )
    }
}

impl fmt::Display for SessionRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            SessionRole::Worker => "Worker",
            SessionRole::OrchestrationWorker => "OrchestrationWorker",
            SessionRole::OrchestrationResearcher => "OrchestrationResearcher",
            SessionRole::Orchestrator => "Orchestrator",
        };

        formatter.write_str(value)
    }
}

impl FromStr for SessionRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Worker" => Ok(SessionRole::Worker),
            "OrchestrationWorker" => Ok(SessionRole::OrchestrationWorker),
            "OrchestrationResearcher" => Ok(SessionRole::OrchestrationResearcher),
            "Orchestrator" => Ok(SessionRole::Orchestrator),
            _ => Err(format!("Unknown role: {value}")),
        }
    }
}

/// High-level lifecycle state for one session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStatus {
    /// Session has been created but has not started its first agent turn yet.
    Draft,
    /// An agent turn or its post-processing workflow is running.
    InProgress,
    /// The session is ready for user review and follow-up work.
    Review,
    /// The session is generating focused-review output.
    AgentReview,
    /// The session is waiting for model clarification responses.
    Question,
    /// The session is waiting in the merge queue.
    Queued,
    /// The session branch is being rebased.
    Rebasing,
    /// The session branch is being merged.
    Merging,
    /// A remote merge awaits local target-branch synchronization.
    Merged,
    /// The session completed successfully.
    Done,
    /// The session was canceled before completion.
    Canceled,
}

impl SessionStatus {
    /// Ordered list of all session statuses.
    pub const ALL: [SessionStatus; 11] = [
        SessionStatus::Draft,
        SessionStatus::InProgress,
        SessionStatus::Review,
        SessionStatus::AgentReview,
        SessionStatus::Question,
        SessionStatus::Queued,
        SessionStatus::Rebasing,
        SessionStatus::Merging,
        SessionStatus::Merged,
        SessionStatus::Done,
        SessionStatus::Canceled,
    ];

    /// Returns whether this status permits opening the chat composer.
    pub fn allows_chat_composer(self) -> bool {
        self.allows_session_actions()
            || matches!(self, SessionStatus::InProgress | SessionStatus::Rebasing)
    }

    /// Returns whether this status permits idle session actions.
    pub fn allows_session_actions(self) -> bool {
        self.allows_review_actions()
            || matches!(self, SessionStatus::Draft | SessionStatus::Question)
    }

    /// Returns whether this status permits opening the session diff.
    pub fn allows_diff_view(self) -> bool {
        self.allows_review_actions() || self.is_read_only()
    }

    /// Returns whether this status permits starting or queueing session sync.
    pub fn allows_rebase_action(self) -> bool {
        self.allows_review_actions() || self == SessionStatus::InProgress
    }

    /// Returns whether this status keeps review actions enabled.
    pub fn allows_review_actions(self) -> bool {
        matches!(self, SessionStatus::Review | SessionStatus::AgentReview)
    }

    /// Returns whether this status can seed a follow-on continuation session.
    pub fn allows_terminal_continuation(self) -> bool {
        matches!(self, SessionStatus::Done | SessionStatus::Canceled)
    }

    /// Returns whether this lifecycle state permits only local inspection.
    pub fn is_read_only(self) -> bool {
        matches!(self, SessionStatus::Merged)
    }

    /// Returns whether this status is stable enough to start a stacked child.
    pub fn allows_stacked_child_start(self) -> bool {
        self.allows_review_actions()
    }

    /// Returns whether this status represents branch-mutating stack work.
    pub fn is_stack_branch_mutating(self) -> bool {
        matches!(
            self,
            SessionStatus::InProgress
                | SessionStatus::Question
                | SessionStatus::Queued
                | SessionStatus::Rebasing
                | SessionStatus::Merging
                | SessionStatus::Merged
        )
    }

    /// Returns whether a transition to `next` is valid.
    pub fn can_transition_to(self, next: SessionStatus) -> bool {
        if self == next {
            return true;
        }
        if self == SessionStatus::Merged {
            return next == SessionStatus::Done;
        }

        matches!(
            (self, next),
            (
                SessionStatus::Draft,
                SessionStatus::InProgress | SessionStatus::Canceled
            ) | (SessionStatus::InProgress, SessionStatus::Draft)
                | (
                    SessionStatus::Draft | SessionStatus::InProgress,
                    SessionStatus::Rebasing
                )
                | (
                    SessionStatus::InProgress
                        | SessionStatus::Queued
                        | SessionStatus::Rebasing
                        | SessionStatus::Merging,
                    SessionStatus::Canceled
                )
                | (SessionStatus::Review, SessionStatus::AgentReview)
                | (SessionStatus::AgentReview, SessionStatus::Review)
                | (
                    SessionStatus::Review | SessionStatus::AgentReview,
                    SessionStatus::Merged | SessionStatus::Done
                )
                | (
                    SessionStatus::Review | SessionStatus::AgentReview | SessionStatus::Question,
                    SessionStatus::InProgress
                        | SessionStatus::Queued
                        | SessionStatus::Rebasing
                        | SessionStatus::Merging
                        | SessionStatus::Canceled
                )
                | (
                    SessionStatus::Queued,
                    SessionStatus::Merging | SessionStatus::Review | SessionStatus::AgentReview
                )
                | (
                    SessionStatus::InProgress | SessionStatus::Rebasing,
                    SessionStatus::Review | SessionStatus::AgentReview | SessionStatus::Question
                )
                | (
                    SessionStatus::Merging,
                    SessionStatus::Done | SessionStatus::Review | SessionStatus::AgentReview
                )
        )
    }
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            SessionStatus::Draft => "Draft",
            SessionStatus::InProgress => "InProgress",
            SessionStatus::Review => "Review",
            SessionStatus::AgentReview => "AgentReview",
            SessionStatus::Question => "Question",
            SessionStatus::Queued => "Queued",
            SessionStatus::Rebasing => "Rebasing",
            SessionStatus::Merging => "Merging",
            SessionStatus::Merged => "Merged",
            SessionStatus::Done => "Done",
            SessionStatus::Canceled => "Canceled",
        };

        formatter.write_str(value)
    }
}

impl FromStr for SessionStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Draft" => Ok(SessionStatus::Draft),
            "InProgress" | "Committing" => Ok(SessionStatus::InProgress),
            "Review" => Ok(SessionStatus::Review),
            "AgentReview" => Ok(SessionStatus::AgentReview),
            "Question" => Ok(SessionStatus::Question),
            "Queued" => Ok(SessionStatus::Queued),
            "Rebasing" => Ok(SessionStatus::Rebasing),
            "Merging" => Ok(SessionStatus::Merging),
            "Merged" => Ok(SessionStatus::Merged),
            "Done" => Ok(SessionStatus::Done),
            "Canceled" => Ok(SessionStatus::Canceled),
            _ => Err(format!("Unknown status: {value}")),
        }
    }
}

/// Complete session-scoped execution settings returned by the API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSettings {
    /// Agent provider and model selected for this session.
    pub agent: AgentSelection,
    /// Base branch used to create the session.
    pub base_branch: String,
    /// Whether the session uses deferred draft materialization.
    pub is_draft: bool,
    /// Immediate parent session for a stacked session.
    pub parent_session_id: Option<SessionId>,
    /// Provider permission mode used for future turns.
    pub permission_mode: PermissionMode,
    /// Workspace personality selected for future turns, when present.
    pub personality_id: Option<String>,
    /// Owning project identifier.
    pub project_id: i64,
    /// Session-scoped reasoning level.
    pub reasoning_level: ReasoningLevel,
    /// Session-scoped response style.
    pub response_style: ResponseStyle,
    /// Role this session plays in a multi-session workflow.
    pub role: SessionRole,
    /// Session-scoped response-speed preference.
    pub speed_mode: SpeedMode,
}

/// Persisted forge linkage for one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRequest {
    /// Unix timestamp of the most recent successful refresh.
    pub last_refreshed_at: i64,
    /// Normalized remote summary captured at `last_refreshed_at`.
    pub summary: ReviewRequestSummary,
}

/// Complete persisted session aggregate returned by the programmatic API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    /// Session creation timestamp in Unix seconds.
    pub created_at: i64,
    /// Staged draft prompt that has not entered the durable transcript.
    pub draft_prompt: Option<String>,
    /// Stable session identifier.
    pub id: SessionId,
    /// Ordered durable transcript messages.
    pub messages: Vec<SessionMessage>,
    /// Published upstream branch reference, when present.
    pub published_upstream_ref: Option<String>,
    /// Structured clarification questions waiting for answers.
    pub questions: Vec<QuestionItem>,
    /// Chat messages waiting behind the active turn or queued branch action.
    pub queued_messages: Vec<String>,
    /// Persisted review-request linkage, when present.
    pub review_request: Option<ReviewRequest>,
    /// Complete session-scoped execution settings.
    pub settings: SessionSettings,
    /// Current lifecycle status.
    pub status: SessionStatus,
    /// Optional user-visible session title.
    pub title: Option<String>,
    /// Last update timestamp in Unix seconds.
    pub updated_at: i64,
}

/// Converts Unix timestamp seconds to a day key after applying a UTC offset.
pub fn activity_day_key_with_offset(timestamp_seconds: i64, utc_offset_seconds: i64) -> i64 {
    timestamp_seconds
        .saturating_add(utc_offset_seconds)
        .div_euclid(SECONDS_PER_DAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_round_trips_json() {
        // Arrange
        let session_id = SessionId::from("session-1");

        // Act
        let serialized = serde_json::to_string(&session_id).expect("id should serialize");
        let deserialized =
            serde_json::from_str::<SessionId>(&serialized).expect("id should deserialize");

        // Assert
        assert_eq!(deserialized, session_id);
        assert_eq!(deserialized.as_str(), "session-1");
        assert_eq!(AsRef::<Path>::as_ref(&deserialized), Path::new("session-1"));
    }

    #[test]
    fn session_id_supports_owned_conversions_and_comparisons() {
        // Arrange
        let source = Arc::<str>::from("session-1");
        let expected = "session-1".to_string();

        // Act
        let session_id = SessionId::from(source);
        let owned = String::from(session_id.clone());

        // Assert
        assert_eq!(session_id, expected);
        assert_eq!(session_id, &expected);
        assert_eq!(owned, expected);
    }

    #[test]
    fn session_status_round_trips_persisted_values() {
        // Arrange
        let statuses = SessionStatus::ALL;

        // Act
        let round_tripped = statuses.map(|status| {
            status
                .to_string()
                .parse::<SessionStatus>()
                .expect("status should parse")
        });

        // Assert
        assert_eq!(round_tripped, statuses);
        assert_eq!(
            "Committing"
                .parse::<SessionStatus>()
                .expect("legacy status should parse"),
            SessionStatus::InProgress
        );
        assert!("Unknown".parse::<SessionStatus>().is_err());
    }

    #[test]
    fn session_role_round_trips_persisted_values() {
        // Arrange
        let roles = [
            SessionRole::Worker,
            SessionRole::OrchestrationWorker,
            SessionRole::OrchestrationResearcher,
            SessionRole::Orchestrator,
        ];

        // Act
        let round_tripped = roles.map(|role| {
            role.to_string()
                .parse::<SessionRole>()
                .expect("role should parse")
        });

        // Assert
        assert_eq!(round_tripped, roles);
        assert_eq!(SessionRole::default(), SessionRole::Worker);
        assert!("Unknown".parse::<SessionRole>().is_err());
    }

    #[test]
    fn branch_ownership_and_tracking_follow_session_role() {
        // Arrange / Act / Assert
        assert!(SessionRole::Worker.owns_branch_changes());
        assert!(SessionRole::OrchestrationWorker.owns_branch_changes());
        assert!(!SessionRole::OrchestrationResearcher.owns_branch_changes());
        assert!(!SessionRole::Orchestrator.owns_branch_changes());
        assert!(SessionRole::Worker.tracks_worktree_changes());
        assert!(SessionRole::OrchestrationResearcher.tracks_worktree_changes());
        assert!(!SessionRole::Orchestrator.tracks_worktree_changes());
        assert!(SessionRole::Worker.accepts_user_turns());
        assert!(!SessionRole::OrchestrationWorker.accepts_user_turns());
        assert!(!SessionRole::OrchestrationResearcher.accepts_user_turns());
        assert!(SessionRole::OrchestrationWorker.is_managed());
        assert!(SessionRole::OrchestrationResearcher.is_managed());
    }

    #[test]
    fn merged_status_only_transitions_to_done() {
        // Arrange
        let status = SessionStatus::Merged;

        // Act / Assert
        assert!(status.can_transition_to(SessionStatus::Merged));
        assert!(status.can_transition_to(SessionStatus::Done));
        assert!(!status.can_transition_to(SessionStatus::Review));
    }

    #[test]
    fn failed_start_can_return_to_draft_without_reviving_terminal_sessions() {
        // Arrange
        let statuses = [
            SessionStatus::InProgress,
            SessionStatus::Done,
            SessionStatus::Canceled,
        ];

        // Act
        let transitions = statuses.map(|status| status.can_transition_to(SessionStatus::Draft));

        // Assert
        assert_eq!(transitions, [true, false, false]);
    }

    #[test]
    fn active_branch_work_statuses_can_transition_to_canceled() {
        // Arrange
        let statuses = [
            SessionStatus::Question,
            SessionStatus::Queued,
            SessionStatus::Rebasing,
            SessionStatus::Merging,
        ];

        // Act
        let cancellation_transitions =
            statuses.map(|status| status.can_transition_to(SessionStatus::Canceled));

        // Assert
        assert_eq!(cancellation_transitions, [true; 4]);
    }

    #[test]
    fn terminal_continuation_is_available_for_done_and_canceled_sessions() {
        // Arrange
        let statuses = SessionStatus::ALL;

        // Act
        let continuation_statuses = statuses
            .into_iter()
            .filter(|status| status.allows_terminal_continuation())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            continuation_statuses,
            vec![SessionStatus::Done, SessionStatus::Canceled]
        );
    }

    #[test]
    fn activity_day_key_applies_offsets() {
        // Arrange / Act / Assert
        assert_eq!(activity_day_key_with_offset(86_399, 1), 1);
        assert_eq!(activity_day_key_with_offset(0, -1), -1);
    }
}
