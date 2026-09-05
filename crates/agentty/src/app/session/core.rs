//! Session lifecycle orchestration for creation, refresh, prompt handling,
//! history management, merge, and cleanup.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ag_git as git;
use tokio::task::{JoinHandle, JoinSet};

use super::SessionError;
use super::workflow::merge::SessionMergeService;
pub(crate) use super::workflow::merge::{SyncMainOutcome, SyncSessionStartError};
pub(crate) use super::workflow::task::{
    RunAgentAssistTaskInput, SessionTaskService, StatusTransition,
};
use super::workflow::worker::SessionWorkerService;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppServices, SessionState, setting};
use crate::domain::agent::{AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::file_entry::FileEntry;
use crate::domain::question::QuestionItem;
use crate::domain::resource::SessionResources;
use crate::domain::session::{
    DailyActivity, FollowUpTaskAction, ReviewRequest, Session, SessionFollowUpTask, SessionId,
    SessionStats, Status,
};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot,
};

/// Low-frequency fallback interval for metadata-based session refresh.
pub(crate) const SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Cache duration for `@`-mention filesystem index snapshots.
pub(crate) const AT_MENTION_INDEX_TTL: Duration = Duration::from_secs(30);

/// Defaults used when creating new sessions from the UI.
#[derive(Clone, Copy)]
pub(crate) struct SessionDefaults {
    /// Default model selected for newly created sessions.
    pub(crate) model: AgentModel,
}

/// Deterministic provider settings captured for one new session.
#[derive(Clone)]
pub(crate) struct SessionCreationSettings {
    /// Agent provider and model assigned to the session.
    pub(crate) agent: AgentSelection,
    /// Provider permission mode assigned to future turns.
    pub(crate) permission_mode: crate::domain::permission::PermissionMode,
    /// Workspace personality selected for future turns.
    pub(crate) personality_id: Option<String>,
    /// Session-scoped reasoning level.
    pub(crate) reasoning_level: ReasoningLevel,
    /// Session-scoped response style.
    pub(crate) response_style: ResponseStyle,
    /// Role assigned to the new session.
    pub(crate) role: crate::domain::session::SessionRole,
    /// Session-scoped response-speed preference.
    pub(crate) speed_mode: SpeedMode,
}

/// Branch-owning purpose assigned when materializing a regular worktree
/// session.
#[derive(Clone, Copy)]
pub(crate) enum SessionCreationKind {
    /// Independent worker session.
    Worker,
    /// Controller that delegates every repository change.
    Orchestrator,
    /// Worker linked durably to one orchestration task.
    OrchestrationChild { task_id: i64 },
    /// Temporary read-only researcher linked to one orchestration task.
    OrchestrationResearch { task_id: i64 },
}

impl SessionCreationKind {
    /// Returns the persisted role for this creation purpose.
    pub(crate) fn role(self) -> crate::domain::session::SessionRole {
        match self {
            Self::Worker => crate::domain::session::SessionRole::Worker,
            Self::OrchestrationChild { .. } => {
                crate::domain::session::SessionRole::OrchestrationWorker
            }
            Self::OrchestrationResearch { .. } => {
                crate::domain::session::SessionRole::OrchestrationResearcher
            }
            Self::Orchestrator => crate::domain::session::SessionRole::Orchestrator,
        }
    }

    /// Returns the task link persisted on an orchestration child.
    pub(crate) fn orchestration_task_id(self) -> Option<i64> {
        match self {
            Self::Worker | Self::Orchestrator => None,
            Self::OrchestrationChild { task_id } | Self::OrchestrationResearch { task_id } => {
                Some(task_id)
            }
        }
    }
}

/// Borrowed session state required to draw one UI frame.
pub(crate) struct SessionRenderParts<'a> {
    /// Exact prompt transcript blocks keyed by session id for active turns.
    pub(crate) active_prompt_outputs: &'a HashMap<SessionId, String>,
    /// Detected session worktree branch names keyed by session id.
    pub(crate) session_branch_names: &'a HashMap<SessionId, String>,
    /// Latest session-branch ahead/behind snapshots keyed by session id.
    pub(crate) session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    /// Cached session list positions keyed by stable session id.
    pub(crate) session_index_by_id: &'a HashMap<SessionId, usize>,
    /// Latest resource totals for tracked agent process trees.
    pub(crate) session_resources: &'a HashMap<SessionId, SessionResources>,
    /// Whether each rendered session currently has a materialized worktree on
    /// disk, keyed by session id.
    pub(crate) session_worktree_availability: &'a HashMap<SessionId, bool>,
    /// Session rows available for rendering.
    pub(crate) sessions: &'a [Session],
    /// Daily session activity series used by dashboard activity summaries.
    pub(crate) stats_activity: &'a [DailyActivity],
    /// Selected session row index.
    pub(crate) selected_index: Option<usize>,
}

/// Reducer-facing snapshot derived from one persisted turn result.
///
/// The worker computes this projection immediately after writing canonical turn
/// metadata so the reducer can apply the same clarification-question,
/// follow-up-task, and token-usage updates without waiting for a full reload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TurnAppliedState {
    /// Persisted follow-up tasks for the latest completed turn.
    pub(crate) follow_up_tasks: Vec<SessionFollowUpTask>,
    /// Persisted clarification questions for the latest completed turn.
    pub(crate) questions: Vec<QuestionItem>,
    /// Token-usage delta reported for the completed turn.
    pub(crate) token_usage_delta: SessionStats,
}

impl TurnAppliedState {
    /// Merges one newer reducer projection into this batched state.
    ///
    /// Latest-turn fields (`follow_up_tasks` and `questions`) replace
    /// the previous projection, while `token_usage_delta` accumulates so
    /// multiple completed turns queued in one reducer tick do not undercount
    /// session usage.
    pub(crate) fn merge_newer(&mut self, newer_turn_applied_state: Self) {
        self.follow_up_tasks = newer_turn_applied_state.follow_up_tasks;
        self.questions = newer_turn_applied_state.questions;
        self.token_usage_delta.input_tokens = self
            .token_usage_delta
            .input_tokens
            .saturating_add(newer_turn_applied_state.token_usage_delta.input_tokens);
        self.token_usage_delta.output_tokens = self
            .token_usage_delta
            .output_tokens
            .saturating_add(newer_turn_applied_state.token_usage_delta.output_tokens);
    }
}

pub(crate) use crate::infra::clock::Clock;

/// Session domain state and worker orchestration state.
pub struct SessionManager {
    pub(super) active_prompt_outputs: HashMap<SessionId, String>,
    at_mention_indexes: HashMap<PathBuf, AtMentionIndex>,
    pub(super) default_session_model: AgentModel,
    pub(super) git_client: Arc<dyn git::GitClient>,
    pub(super) merge_service: SessionMergeService,
    pub(super) resources: super::resource::ResourceMonitor,
    pub(super) state: SessionState,
    pub(super) stats_activity: Vec<DailyActivity>,
    pub(super) workflow_state: SessionWorkflowState,
    pub(super) worker_service: SessionWorkerService,
}

/// Live bookkeeping shared by session lifecycle workflows.
pub(super) struct SessionWorkflowState {
    pub(super) pending_history_replay: HashSet<SessionId>,
    pub(super) published_branch_sync_operations: HashMap<SessionId, String>,
    pub(super) title_generation_tasks: HashMap<SessionId, TitleGenerationTask>,
}

/// Tracks one draft-title generation task plus the generation used to identify
/// stale completion events.
pub(crate) struct TitleGenerationTask {
    generation: u64,
    join_handle: JoinHandle<()>,
}

/// Cached `@`-mention file index snapshot for one lookup root.
#[derive(Debug)]
struct AtMentionIndex {
    created_at: Instant,
    entries: Vec<FileEntry>,
}

impl SessionManager {
    /// Creates a session manager from persisted snapshot state and defaults.
    ///
    /// Review sessions are marked for one-time transcript replay so the next
    /// reply can rehydrate provider context after app restart.
    pub(crate) fn new(
        defaults: SessionDefaults,
        git_client: Arc<dyn git::GitClient>,
        state: SessionState,
        stats_activity: Vec<DailyActivity>,
    ) -> Self {
        let pending_history_replay = Self::startup_history_replay_set(&state.sessions);

        Self {
            active_prompt_outputs: HashMap::new(),
            resources: super::resource::ResourceMonitor::new(Arc::new(
                crate::infra::resource::RealResourceClient,
            )),
            at_mention_indexes: HashMap::new(),
            default_session_model: defaults.model,
            git_client,
            merge_service: SessionMergeService,
            state,
            stats_activity,
            workflow_state: SessionWorkflowState {
                pending_history_replay,
                published_branch_sync_operations: HashMap::new(),
                title_generation_tasks: HashMap::new(),
            },
            worker_service: SessionWorkerService::new(),
        }
    }

    /// Replaces any in-flight staged title-generation task for one session.
    ///
    /// The superseded task is aborted before the new task handle is stored so
    /// rapid draft staging does not fan out redundant provider requests.
    pub(crate) fn replace_title_generation_task(
        &mut self,
        session_id: &str,
        generation: u64,
        title_generation_task: JoinHandle<()>,
    ) {
        if let Some(existing_task) = self
            .workflow_state
            .title_generation_tasks
            .remove(session_id)
            && !existing_task.join_handle.is_finished()
        {
            existing_task.join_handle.abort();
        }

        self.workflow_state.title_generation_tasks.insert(
            SessionId::from(session_id),
            TitleGenerationTask {
                generation,
                join_handle: title_generation_task,
            },
        );
    }

    /// Aborts and forgets any tracked staged title-generation task for one
    /// session.
    pub(crate) fn abort_title_generation_task(&mut self, session_id: &str) {
        if let Some(existing_task) = self
            .workflow_state
            .title_generation_tasks
            .remove(session_id)
            && !existing_task.join_handle.is_finished()
        {
            existing_task.join_handle.abort();
        }
    }

    /// Returns the next tracked generation number for one session's draft
    /// title-generation task.
    pub(crate) fn next_title_generation_task_generation(&self, session_id: &str) -> u64 {
        self.workflow_state
            .title_generation_tasks
            .get(session_id)
            .map_or(1, |tracked_task| tracked_task.generation.saturating_add(1))
    }

    /// Clears one tracked draft-title generation task when the completion event
    /// matches the currently tracked generation.
    pub(crate) fn clear_title_generation_task_if_matches(
        &mut self,
        session_id: &str,
        generation: u64,
    ) {
        let should_clear = self
            .workflow_state
            .title_generation_tasks
            .get(session_id)
            .is_some_and(|tracked_task| tracked_task.generation == generation);

        if should_clear {
            self.workflow_state
                .title_generation_tasks
                .remove(session_id);
        }
    }

    /// Returns the internal merge orchestration service.
    pub(crate) fn merge_service(&self) -> &SessionMergeService {
        &self.merge_service
    }

    /// Returns the configured session git client used by orchestration flows.
    pub(crate) fn git_client(&self) -> Arc<dyn git::GitClient> {
        Arc::clone(&self.git_client)
    }

    /// Returns mutable access to worker orchestration state.
    pub(crate) fn worker_service_mut(&mut self) -> &mut SessionWorkerService {
        &mut self.worker_service
    }

    /// Returns the default smart model used for session-scoped agent
    /// workflows.
    pub(crate) fn default_session_model(&self) -> AgentModel {
        self.default_session_model
    }

    /// Replaces the default smart model used for newly created sessions.
    pub(crate) fn set_default_session_model(&mut self, session_model: AgentModel) {
        self.default_session_model = session_model;
    }

    /// Loads the default smart model persisted for new sessions.
    pub(crate) async fn load_default_session_model(
        services: &AppServices,
        project_id: Option<i64>,
        fallback_model: AgentModel,
    ) -> AgentModel {
        setting::load_default_smart_model_setting(services, project_id, fallback_model).await
    }

    /// Returns session snapshots, render caches, and semantic selection
    /// required for one frame.
    ///
    /// The render parts borrow disjoint manager fields directly so
    /// [`crate::ui::render_app`] can avoid cloning session maps or active
    /// prompt output blocks while runtime retains the concrete table viewport.
    pub(crate) fn render_parts(&self) -> SessionRenderParts<'_> {
        SessionRenderParts {
            active_prompt_outputs: &self.active_prompt_outputs,
            session_resources: &self.resources.values,
            session_branch_names: &self.state.session_branch_names,
            session_git_statuses: &self.state.session_git_statuses,
            session_index_by_id: &self.state.session_index_by_id,
            session_worktree_availability: &self.state.session_worktree_availability,
            sessions: &self.state.sessions,
            stats_activity: &self.stats_activity,
            selected_index: self.state.table_state.selected(),
        }
    }

    /// Refreshes live process accounting through the background sampler.
    pub(crate) async fn refresh_resources(&mut self) -> bool {
        let roots = self
            .state
            .handles()
            .iter()
            .filter_map(|(id, handles)| {
                handles
                    .child_pid
                    .lock()
                    .ok()
                    .and_then(|pid| *pid)
                    .map(|pid| (id.clone(), pid))
            })
            .collect();

        self.resources
            .refresh(roots, self.state.clock.now_instant())
            .await
    }

    /// Returns all loaded session snapshots in current list order.
    pub(crate) fn sessions(&self) -> &[Session] {
        &self.state.sessions
    }

    /// Returns mutable access to all loaded session snapshots for focused
    /// reducers and tests that need to stage state directly.
    pub(crate) fn sessions_mut(&mut self) -> &mut [Session] {
        &mut self.state.sessions
    }

    /// Appends one loaded session snapshot and updates stable id lookups.
    #[cfg(test)]
    pub(crate) fn push_session(&mut self, session: Session) {
        self.state.push_session(session);
    }

    /// Removes one loaded session snapshot by list index.
    pub(crate) fn remove_session_at(&mut self, session_index: usize) -> Option<Session> {
        self.state.remove_session_at(session_index)
    }

    /// Returns the selected session-list index.
    pub(crate) fn selected_session_index(&self) -> Option<usize> {
        self.state.table_state.selected()
    }

    /// Replaces the selected session-list index.
    pub(crate) fn select_session_index(&mut self, session_index: Option<usize>) {
        self.state.table_state.select(session_index);
    }

    /// Returns one mutable session snapshot by current list index.
    pub(crate) fn session_at_mut(&mut self, session_index: usize) -> Option<&mut Session> {
        self.state.sessions.get_mut(session_index)
    }

    /// Returns one immutable session snapshot by stable identifier.
    pub(crate) fn session_for_id(&self, session_id: &str) -> Option<&Session> {
        self.state.session_for_id(session_id)
    }

    /// Synchronizes all loaded session snapshots from live runtime handles.
    #[cfg(test)]
    pub(crate) fn sync_from_handles(&mut self) {
        self.state.sync_from_handles();
    }

    /// Synchronizes one loaded session snapshot from its live runtime handle.
    pub(crate) fn sync_session_from_handle(&mut self, session_id: &str) {
        self.state.sync_session_from_handle(session_id);
    }

    /// Applies recomputed diff metadata to one loaded session.
    pub(crate) fn apply_session_diff_stats_updated(
        &mut self,
        session_id: &str,
        diff_stats: crate::domain::session::SessionDiffStats,
    ) {
        self.state
            .apply_session_diff_stats_updated(session_id, diff_stats);
    }

    /// Returns runtime handles keyed by stable session id.
    pub(crate) fn session_handles(
        &self,
    ) -> &HashMap<SessionId, crate::domain::session::SessionHandles> {
        self.state.handles()
    }

    /// Returns mutable runtime handles keyed by stable session id.
    #[cfg(test)]
    pub(crate) fn session_handles_mut(
        &mut self,
    ) -> &mut HashMap<SessionId, crate::domain::session::SessionHandles> {
        self.state.handles_mut()
    }

    /// Returns the active prompt transcript block cached for sessions that are
    /// currently running a turn.
    pub(crate) fn active_prompt_outputs(&self) -> &HashMap<SessionId, String> {
        &self.active_prompt_outputs
    }

    /// Returns shared immutable access to session render and refresh state.
    pub(crate) fn state(&self) -> &SessionState {
        &self.state
    }

    /// Returns shared mutable access to session render and refresh state for
    /// reducers that still operate on [`SessionState`] directly.
    pub(crate) fn state_mut(&mut self) -> &mut SessionState {
        &mut self.state
    }

    /// Applies reducer updates after session agent/model changes are
    /// persisted.
    ///
    /// This updates only the target session snapshot. Global default-model
    /// selection is managed by settings persistence and loaded when creating
    /// new sessions.
    pub(crate) fn apply_session_model_updated(
        &mut self,
        session_id: &str,
        session_agent: crate::domain::agent::AgentSelection,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.agent = session_agent;
        }
    }

    /// Applies one persisted reasoning-level update to the matching
    /// in-memory session snapshot.
    pub(crate) fn apply_session_reasoning_level_updated(
        &mut self,
        session_id: &str,
        reasoning_level: ReasoningLevel,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.reasoning_level_override = Some(reasoning_level);
        }
    }

    /// Applies one persisted response-style update to the matching in-memory
    /// session snapshot.
    pub(crate) fn apply_session_response_style_updated(
        &mut self,
        session_id: &str,
        response_style: ResponseStyle,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.response_style = response_style;
        }
    }

    /// Applies one persisted provider permission update to the matching
    /// in-memory session snapshot.
    pub(crate) fn apply_session_permission_mode_updated(
        &mut self,
        session_id: &str,
        permission_mode: crate::domain::permission::PermissionMode,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.permission_mode = permission_mode;
        }
    }

    /// Applies one persisted response-speed update to the matching in-memory
    /// session snapshot.
    pub(crate) fn apply_session_speed_mode_updated(
        &mut self,
        session_id: &str,
        speed_mode: SpeedMode,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.speed_mode = speed_mode;
        }
    }

    /// Applies one persisted personality update to the matching in-memory
    /// session snapshot.
    pub(crate) fn apply_session_personality_updated(
        &mut self,
        session_id: &str,
        personality_id: Option<String>,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.personality_id = personality_id;
        }
    }

    /// Applies one persisted published-upstream reference to the matching
    /// in-memory session snapshot.
    pub(crate) fn apply_published_upstream_ref(
        &mut self,
        session_id: &str,
        published_upstream_ref: String,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.published_upstream_ref = Some(published_upstream_ref);
        }
    }

    /// Applies one persisted review-request snapshot to the matching in-memory
    /// session row.
    pub(crate) fn apply_review_request(&mut self, session_id: &str, review_request: ReviewRequest) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.review_request = Some(review_request);
        }
    }

    /// Shows one manual branch-publish action as an inline session-chat task.
    pub(crate) fn start_branch_publish(&mut self, session_id: &str, loading_label: String) {
        self.state
            .resolve_queued_action(session_id, TransientMessageSlot::BranchPublish);
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Loading(loading_label),
                lifecycle: TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::BranchPublish,
                turn_position: session.latest_user_prompt_position(),
            });
        }
    }

    /// Shows one review-request publish action waiting behind the active turn.
    pub(crate) fn queue_branch_publish(
        &mut self,
        session_id: &str,
        order: u64,
        queued_label: String,
    ) {
        let turn_position = self
            .state
            .session_for_id(session_id)
            .and_then(Session::latest_user_prompt_position);
        self.state.upsert_queued_action(
            session_id,
            TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Queued(QueuedAction::new(order, queued_label)),
                lifecycle: TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::BranchPublish,
                turn_position,
            },
        );
    }

    /// Removes a queued review-request row that resolved without starting.
    pub(crate) fn resolve_queued_branch_publish(&mut self, session_id: &str) {
        self.state
            .resolve_queued_action(session_id, TransientMessageSlot::BranchPublish);
    }

    /// Shows one session sync waiting behind the active turn.
    pub(crate) fn queue_session_sync(&mut self, session_id: &str, order: u64) {
        let turn_position = self
            .state
            .session_for_id(session_id)
            .and_then(Session::latest_user_prompt_position);
        self.state.upsert_queued_action(
            session_id,
            TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Queued(QueuedAction::new(
                    order,
                    "sync — rebase onto the base branch after this turn".to_string(),
                )),
                lifecycle: TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::SyncQueue,
                turn_position,
            },
        );
    }

    /// Removes a queued-sync row after its worker command resolves or starts.
    pub(crate) fn resolve_queued_session_sync(&mut self, session_id: &str) {
        self.state
            .resolve_queued_action(session_id, TransientMessageSlot::SyncQueue);
    }

    /// Replaces manual branch-publish progress with its inline final result.
    ///
    /// When the owning project is not loaded, appends the result to the live
    /// transcript instead and returns it for durable persistence.
    pub(crate) fn finish_branch_publish(
        &mut self,
        session_id: &str,
        body: TransientMessageBody,
    ) -> Option<String> {
        self.state
            .resolve_queued_action(session_id, TransientMessageSlot::BranchPublish);
        let persistent_notice = body.text().to_string();
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::AfterCompletedTurn,
                body,
                lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                slot: TransientMessageSlot::BranchPublish,
                turn_position: session.latest_user_prompt_position(),
            });

            return None;
        }

        self.append_workflow_notice_to_handle(session_id, &persistent_notice)
            .then_some(persistent_notice)
    }

    /// Promotes completed review-request creation into durable transcript
    /// history and removes its transient loading row.
    pub(crate) fn finish_review_request_publish(
        &mut self,
        session_id: &str,
        persistent_notice: &str,
    ) -> bool {
        self.state
            .resolve_queued_action(session_id, TransientMessageSlot::BranchPublish);
        let appended_to_handle =
            self.append_workflow_notice_to_handle(session_id, persistent_notice);
        if appended_to_handle {
            self.state.sync_session_from_handle(session_id);
        }
        let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return appended_to_handle;
        };
        if !appended_to_handle {
            session
                .transcript
                .get_or_insert_default()
                .append_message(SessionMessageKind::WorkflowNotice, persistent_notice);
        }
        Self::place_completed_review_before_new_workflow_notice(session);
        session
            .transient_messages
            .retract(TransientMessageSlot::BranchPublish);

        true
    }

    /// Moves an already-completed focused review ahead of a newer durable
    /// workflow notice while leaving an in-flight review at the output tail.
    fn place_completed_review_before_new_workflow_notice(session: &mut Session) {
        let Some(mut review_message) = session
            .transient_messages
            .get(TransientMessageSlot::Review)
            .filter(|message| {
                matches!(
                    &message.body,
                    TransientMessageBody::Markdown(_) | TransientMessageBody::Plain(_)
                )
            })
            .cloned()
        else {
            return;
        };
        review_message.anchor = TransientMessageAnchor::AfterCompletedTurn;
        session.transient_messages.upsert(review_message);
    }

    /// Marks one session branch as currently auto-syncing to its published
    /// upstream reference.
    pub(crate) fn start_published_branch_sync(
        &mut self,
        session_id: &str,
        sync_operation_id: String,
    ) {
        self.workflow_state
            .published_branch_sync_operations
            .insert(SessionId::from(session_id), sync_operation_id);

        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Loading(
                    "Auto-pushing published branch after completed turn...".to_string(),
                ),
                lifecycle: TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::PublishedBranchSync,
                turn_position: session.latest_user_prompt_position(),
            });
        }
    }

    /// Applies one terminal auto-push state when it matches the latest tracked
    /// sync operation for the session.
    pub(crate) fn finish_published_branch_sync(
        &mut self,
        session_id: &str,
        sync_operation_id: &str,
        persistent_notice: Option<&str>,
    ) -> bool {
        let Some(current_operation_id) = self
            .workflow_state
            .published_branch_sync_operations
            .get(session_id)
        else {
            return false;
        };
        if current_operation_id != sync_operation_id {
            return false;
        }

        self.workflow_state
            .published_branch_sync_operations
            .remove(session_id);

        let appended_to_handle = persistent_notice.is_some_and(|persistent_notice| {
            self.append_workflow_notice_to_handle(session_id, persistent_notice)
        });
        if appended_to_handle {
            self.state.sync_session_from_handle(session_id);
        }

        let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return appended_to_handle;
        };
        if !appended_to_handle && let Some(persistent_notice) = persistent_notice {
            session
                .transcript
                .get_or_insert_default()
                .append_message(SessionMessageKind::WorkflowNotice, persistent_notice);
        }
        session
            .transient_messages
            .retract(TransientMessageSlot::PublishedBranchSync);

        true
    }

    /// Appends transient sync-error notices to stacked children whose
    /// automatic rebase could not be enqueued.
    pub(crate) fn append_stacked_rebase_failure_notices(
        &mut self,
        failures: Vec<(SessionId, SessionError)>,
        failure_context: &str,
    ) {
        for (child_session_id, error) in failures {
            let notice =
                TranscriptNotice::RebaseError.format_line(format!("{failure_context}: {error}"));
            self.append_workflow_notice(child_session_id.as_str(), notice);
        }
    }

    /// Appends one transient workflow notice shown for one session.
    pub(crate) fn append_workflow_notice(&mut self, session_id: &str, notice: String) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            let notice = session
                .transient_messages
                .get(TransientMessageSlot::WorkflowNotice)
                .map(|message| format!("{}\n\n{notice}", message.body.text()))
                .unwrap_or(notice);
            let anchor = if matches!(session.status, Status::InProgress | Status::Queued) {
                TransientMessageAnchor::AfterActiveTurn
            } else {
                TransientMessageAnchor::AfterCompletedTurn
            };
            session.transient_messages.upsert(TransientMessage {
                anchor,
                body: TransientMessageBody::Markdown(notice),
                lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                slot: TransientMessageSlot::WorkflowNotice,
                turn_position: session.latest_user_prompt_position(),
            });
        }
    }

    /// Replaces or clears the board snapshot for an orchestrator.
    pub(crate) fn update_orchestration_progress(
        &mut self,
        session_id: &str,
        progress: Option<String>,
    ) {
        let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        session.orchestration_progress = progress;
        session
            .transient_messages
            .retract(TransientMessageSlot::Orchestration);
    }

    /// Applies one completed-turn projection to the matching in-memory
    /// session snapshot.
    pub(crate) fn apply_turn_applied_state(
        &mut self,
        session_id: &str,
        turn_applied_state: &TurnAppliedState,
    ) {
        let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        session
            .follow_up_tasks
            .clone_from(&turn_applied_state.follow_up_tasks);
        session.questions.clone_from(&turn_applied_state.questions);
        session
            .transient_messages
            .retract(TransientMessageSlot::ReviewCommentResolution);
        session.stats.input_tokens = session
            .stats
            .input_tokens
            .saturating_add(turn_applied_state.token_usage_delta.input_tokens);
        session.stats.output_tokens = session
            .stats
            .output_tokens
            .saturating_add(turn_applied_state.token_usage_delta.output_tokens);
        self.active_prompt_outputs.remove(session_id);
    }

    /// Caches one exact prompt transcript block for an active session turn so
    /// rendering can anchor synthetic metadata to the correct boundary without
    /// reparsing generic transcript markers.
    pub(crate) fn set_active_prompt_output(&mut self, session_id: &str, prompt_output: String) {
        self.active_prompt_outputs
            .insert(SessionId::from(session_id), prompt_output);
    }

    /// Returns cached `@`-mention entries for one lookup root when the cache
    /// entry is still within its TTL window.
    pub(crate) fn at_mention_index_for_root(
        &mut self,
        lookup_root: &Path,
    ) -> Option<Vec<FileEntry>> {
        let cached_index = self.at_mention_indexes.get(lookup_root)?;

        if self
            .state
            .clock
            .now_instant()
            .saturating_duration_since(cached_index.created_at)
            > AT_MENTION_INDEX_TTL
        {
            self.at_mention_indexes.remove(lookup_root);

            return None;
        }

        Some(cached_index.entries.clone())
    }

    /// Replaces the cached `@`-mention index for one lookup root.
    pub(crate) fn set_at_mention_index_for_root(
        &mut self,
        lookup_root: PathBuf,
        entries: Vec<FileEntry>,
    ) {
        self.at_mention_indexes.insert(
            lookup_root,
            AtMentionIndex {
                created_at: self.state.clock.now_instant(),
                entries,
            },
        );
    }

    /// Drops the cached `@`-mention index for one lookup root.
    pub(crate) fn remove_at_mention_index_for_root(&mut self, lookup_root: &Path) {
        self.at_mention_indexes.remove(lookup_root);
    }

    /// Drops cached prompt transcript blocks for sessions that are no longer
    /// actively running a turn and prunes expired `@`-mention indexes.
    pub(crate) fn retain_active_prompt_outputs(&mut self) {
        self.active_prompt_outputs.retain(|session_id, _| {
            self.state
                .sessions
                .iter()
                .find(|session| session.id == *session_id)
                .is_some_and(|session| {
                    matches!(
                        session.status,
                        crate::domain::session::Status::InProgress
                            | crate::domain::session::Status::Queued
                            | crate::domain::session::Status::Rebasing
                            | crate::domain::session::Status::Merging
                    )
                })
        });
        self.prune_expired_at_mention_indexes();
    }

    /// Replaces cached session git-status snapshots from the latest
    /// background poll.
    pub(crate) fn replace_session_git_statuses(
        &mut self,
        session_git_statuses: HashMap<SessionId, SessionGitStatus>,
    ) {
        self.state
            .replace_session_git_statuses(session_git_statuses);
    }

    /// Removes expired `@`-mention indexes so unused lookup roots do not
    /// accumulate indefinitely in memory.
    fn prune_expired_at_mention_indexes(&mut self) {
        let now = self.state.clock.now_instant();

        self.at_mention_indexes.retain(|_, cached_index| {
            now.saturating_duration_since(cached_index.created_at) <= AT_MENTION_INDEX_TTL
        });
    }

    /// Replaces cached worktree-availability snapshots from the latest
    /// session reload.
    pub(crate) fn replace_session_worktree_availability(
        &mut self,
        session_worktree_availability: HashMap<SessionId, bool>,
    ) {
        self.state
            .replace_session_worktree_availability(session_worktree_availability);
    }

    /// Returns cached worktree availability keyed by session id.
    pub(crate) fn session_worktree_availability(&self) -> &HashMap<SessionId, bool> {
        &self.state.session_worktree_availability
    }

    /// Replaces cached detected session branch names from the latest reload.
    pub(crate) fn replace_session_branch_names(
        &mut self,
        session_branch_names: HashMap<SessionId, String>,
    ) {
        self.state
            .replace_session_branch_names(session_branch_names);
    }

    /// Returns the cached or derived branch name for one session.
    pub(crate) fn session_branch_name(&self, session_id: &str) -> Option<&str> {
        self.state
            .session_branch_names
            .get(session_id)
            .map(String::as_str)
    }

    /// Updates cached worktree availability for one session after its
    /// lifecycle materializes or removes the worktree.
    pub(crate) fn set_session_worktree_available(&mut self, session_id: &str, is_available: bool) {
        self.state
            .set_session_worktree_available(session_id, is_available);
    }

    /// Drops cached worktree availability for one removed session.
    pub(crate) fn remove_session_worktree_availability(&mut self, session_id: &str) {
        self.state.remove_session_worktree_availability(session_id);
    }

    /// Refreshes cached branch names for all currently loaded sessions by
    /// detecting each worktree `HEAD` and falling back to the derived default
    /// session branch when detection is unavailable.
    ///
    /// Branch detection runs concurrently so startup and session refresh do
    /// not pay one subprocess round trip per session in series.
    pub(crate) async fn refresh_session_branch_names(&mut self) {
        let session_inputs = self
            .state
            .sessions
            .iter()
            .map(|session| {
                let session_id = session.id.clone();
                let default_branch_name = session_branch(&session_id);

                (session_id, session.folder.clone(), default_branch_name)
            })
            .collect::<Vec<_>>();
        let mut session_branch_names = session_inputs
            .iter()
            .map(|(session_id, _, default_branch_name)| {
                (session_id.clone(), default_branch_name.clone())
            })
            .collect::<HashMap<_, _>>();
        let mut branch_detection_tasks = JoinSet::new();

        for (session_id, session_folder, default_branch_name) in session_inputs {
            let git_client = Arc::clone(&self.git_client);
            branch_detection_tasks.spawn(async move {
                let branch_name = git_client
                    .detect_git_info(session_folder)
                    .await
                    .unwrap_or(default_branch_name);

                (session_id, branch_name)
            });
        }

        while let Some(branch_detection_result) = branch_detection_tasks.join_next().await {
            let Ok((session_id, branch_name)) = branch_detection_result else {
                continue;
            };

            session_branch_names.insert(session_id, branch_name);
        }

        self.replace_session_branch_names(session_branch_names);
    }

    /// Returns the selected follow-up task position for one session.
    pub(crate) fn selected_follow_up_task_position(&self, session_id: &str) -> Option<usize> {
        self.state.selected_follow_up_task_position(session_id)
    }

    /// Returns the action currently available for the selected follow-up task
    /// in one session.
    pub(crate) fn selected_follow_up_task_action(
        &self,
        session_id: &str,
    ) -> Option<FollowUpTaskAction> {
        let position = self.selected_follow_up_task_position(session_id)?;
        let session = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)?;

        let task = session.follow_up_task(position)?;
        if session.status.is_read_only() && task.launched_session_id.is_none() {
            return None;
        }

        Some(task.action())
    }

    /// Returns whether one session has more than one follow-up task.
    pub(crate) fn has_multiple_follow_up_tasks(&self, session_id: &str) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| session.follow_up_tasks.len() > 1)
    }

    /// Advances the selected follow-up task to the next item for one session.
    pub(crate) fn select_next_follow_up_task(&mut self, session_id: &str) {
        self.state.select_next_follow_up_task(session_id);
    }

    /// Moves the selected follow-up task to the previous item for one
    /// session.
    pub(crate) fn select_previous_follow_up_task(&mut self, session_id: &str) {
        self.state.select_previous_follow_up_task(session_id);
    }

    /// Sets the launched sibling-session link for the matching cached
    /// follow-up task.
    pub(crate) fn set_follow_up_task_launched_session_id(
        &mut self,
        session_id: &str,
        position: usize,
        launched_session_id: Option<SessionId>,
    ) {
        self.state.set_follow_up_task_launched_session_id(
            session_id,
            position,
            launched_session_id,
        );
    }

    /// Appends one workflow notice to a live transcript handle when the
    /// session currently owns one.
    fn append_workflow_notice_to_handle(&self, session_id: &str, persistent_notice: &str) -> bool {
        let Some(handles) = self.state.handle(session_id) else {
            return false;
        };
        let Ok(mut transcript) = handles.transcript.lock() else {
            return false;
        };

        transcript.append_message(SessionMessageKind::WorkflowNotice, persistent_notice);

        true
    }
}

/// Prefix used for default session worktree branches.
const SESSION_BRANCH_PREFIX: &str = "wt/";

/// Returns the folder path for a session under the given base directory.
pub(crate) fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    let len = session_id.len().min(8);
    base.join(&session_id[..len])
}

/// Returns the default worktree branch name for a session.
pub(crate) fn session_branch(session_id: &str) -> String {
    let len = session_id.len().min(8);
    format!("{SESSION_BRANCH_PREFIX}{}", &session_id[..len])
}

/// Extracts the remote branch portion from one upstream reference.
///
/// For example, `origin/wt/abc12345` returns `wt/abc12345`. When the
/// reference contains no `/` separator, the full input is returned as-is.
pub(crate) fn remote_branch_name_from_upstream_ref(upstream_ref: &str) -> String {
    upstream_ref.split_once('/').map_or_else(
        || upstream_ref.to_string(),
        |(_, branch_name)| branch_name.to_string(),
    )
}

/// Converts one wall-clock timestamp into Unix seconds.
pub(crate) fn unix_timestamp_from_system_time(system_time: SystemTime) -> i64 {
    system_time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

#[cfg(test)]
#[path = "core_test.rs"]
mod tests;
