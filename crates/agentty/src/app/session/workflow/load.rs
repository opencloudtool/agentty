//! Session loading and derived snapshot attributes from persisted rows.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use ag_git::GitClient;
use ag_orchestration as orchestration;
use tracing::warn;

use super::{draft, session_folder};
use crate::app::session::SessionError;
use crate::app::{AppServices, SessionManager};
use crate::domain::agent::{
    AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
    parse_persisted_session_agent_model,
};
use crate::domain::permission::PermissionMode;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    DailyActivity, QueuedMessage, ReviewRequest, ReviewRequestSummary, Session, SessionDiffState,
    SessionDiffStats, SessionFollowUpTask, SessionHandles, SessionId, SessionRole, SessionSize,
    SessionStats, Status, activity_day_key_with_offset,
};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot, TransientMessageStore,
};
use crate::infra::clock::Clock;
use crate::infra::db::{
    AppRepositories, DbError, SessionDetailRow, SessionListRow, SessionMessageRow,
    SessionPreparationRow, SessionPreparationState,
};
use crate::infra::fs::FsClient;

/// Inputs required to load one project's session and activity snapshots.
pub(crate) struct SessionLoadInput<'a> {
    /// Project identifier used to scope persisted session rows.
    pub(crate) active_project_id: i64,
    /// Session whose transcript-scale details should be loaded.
    pub(crate) active_session_id: Option<&'a str>,
    /// Root directory containing Agentty-managed session worktrees.
    pub(crate) base: &'a Path,
    /// Clock used to resolve the local offset for each activity event.
    pub(crate) clock: &'a dyn Clock,
    /// Repository bundle used to load persisted session state.
    pub(crate) db: &'a AppRepositories,
    /// Filesystem boundary used to check session worktree availability.
    pub(crate) fs_client: &'a dyn FsClient,
    /// Active project directory used to derive display metadata.
    pub(crate) working_dir: &'a Path,
}

/// Mutable context threaded through the per-row session-load helper.
///
/// Keeps the per-row helper signature short while still letting it append
/// loaded sessions, mutate handles, and update worktree availability.
struct LoadSessionContext<'a> {
    active_session_id: Option<&'a str>,
    base: &'a Path,
    db: &'a AppRepositories,
    fs_client: &'a dyn FsClient,
    handles: &'a mut HashMap<SessionId, SessionHandles>,
    orchestration_metadata: &'a HashMap<String, orchestration::OrchestrationSessionMetadata>,
    preparations: &'a HashMap<String, SessionPreparationRow>,
    project_name: &'a str,
    session_worktree_availability: &'a mut HashMap<SessionId, bool>,
    sessions: &'a mut Vec<Session>,
}

/// Precomputed fields needed to assemble one loaded session snapshot.
struct LoadedSessionInput {
    controller_session_id: Option<SessionId>,
    draft_attachments: Vec<crate::domain::turn_prompt::TurnPromptAttachment>,
    follow_up_tasks: Vec<SessionFollowUpTask>,
    folder: std::path::PathBuf,
    parent_session_id: Option<SessionId>,
    orchestration_progress: Option<String>,
    permission_mode: PermissionMode,
    project_name: String,
    reasoning_level_override: Option<ReasoningLevel>,
    response_style: ResponseStyle,
    review_request: Option<ReviewRequest>,
    role: SessionRole,
    row: SessionListRow,
    session_agent: AgentSelection,
    session_id: SessionId,
    session_prompt: String,
    session_queued_actions: Vec<TransientMessage>,
    session_queued_messages: Vec<QueuedMessage>,
    session_questions: Vec<QuestionItem>,
    session_status: Status,
    session_transcript: Option<SessionTranscript>,
    size: SessionSize,
    speed_mode: SpeedMode,
}

/// Migrates every non-terminal session across all saved projects away from
/// retired persisted model ids.
///
/// Query and persistence failures are best-effort so startup remains usable
/// with a degraded database. Individual UI and API loads repeat the same
/// migration for the row they read.
pub(crate) async fn migrate_active_sessions_off_retired_models(db: &AppRepositories) {
    let Ok(rows) = db.sessions().load_active_session_agent_models().await else {
        return;
    };

    for row in rows {
        let session_status = row.status.parse::<Status>().unwrap_or(Status::Done);
        migrate_session_off_retired_model(db, &row.id, &row.agent, &row.model, session_status)
            .await;
    }
}

/// Resolves one persisted provider/model pair and persists the replacement
/// when its model is retired and the session is still active.
///
/// Terminal rows (`Merged`, `Done`, `Canceled`) keep the retired model id in
/// the database as a historical record. Persistence failures are ignored so
/// session reads still return the in-memory replacement.
pub(crate) async fn migrate_session_off_retired_model(
    db: &AppRepositories,
    session_id: &str,
    persisted_agent: &str,
    persisted_model: &str,
    session_status: Status,
) -> AgentSelection {
    let session_agent = parse_persisted_session_agent_model(Some(persisted_agent), persisted_model);
    if matches!(
        session_status,
        Status::Merged | Status::Done | Status::Canceled
    ) || AgentModel::retired_replacement(persisted_model).is_none()
    {
        return session_agent;
    }

    let session_agent_kind = session_agent.kind().to_string();
    db.sessions()
        .update_active_session_agent_model(
            session_id,
            &session_agent_kind,
            session_agent.model().as_str(),
        )
        .await
        .ok();

    session_agent
}

impl SessionManager {
    /// Registers only the newly persisted row; creation never reloads every
    /// session.
    pub(crate) async fn register_created_session(
        &mut self,
        services: &AppServices,
        session_id: &str,
        working_dir: &Path,
    ) -> Result<(), SessionError> {
        if self.session_for_id(session_id).is_some() {
            return Ok(());
        }
        let row = services
            .db()
            .sessions()
            .load_session(session_id)
            .await?
            .ok_or(SessionError::NotFound)?;
        let permission_mode = row
            .permission_mode
            .parse()
            .map_err(|_| SessionError::Workflow("Invalid session permission mode".to_string()))?;
        let metadata = orchestration::session_metadata_for_project(
            services.db(),
            row.project_id.unwrap_or_default(),
        )
        .await;
        let preparation = services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?;
        let preparations = preparation
            .into_iter()
            .map(|row| (row.session_id.clone(), row))
            .collect();
        let mut sessions = Vec::new();
        let mut availability = HashMap::new();
        let fs_client = services.fs_client();
        Self::push_loaded_session_row(
            &mut LoadSessionContext {
                active_session_id: Some(session_id),
                base: services.base_path(),
                db: services.db(),
                fs_client: fs_client.as_ref(),
                handles: self.state.handles_mut(),
                orchestration_metadata: &metadata,
                preparations: &preparations,
                project_name: working_dir
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or_default(),
                session_worktree_availability: &mut availability,
                sessions: &mut sessions,
            },
            row.into(),
            permission_mode,
        )
        .await;
        for session in sessions {
            self.state.push_session(session);
        }
        for (id, available) in availability {
            self.set_session_worktree_available(&id, available);
        }

        Ok(())
    }

    /// Shows saved-prompt readiness and setup failures without flashing a
    /// preparation notice while the user is still composing their first prompt.
    pub(crate) fn apply_workspace_preparation(
        session: &mut Session,
        preparation: Option<&SessionPreparationRow>,
    ) {
        session
            .transient_messages
            .retract(TransientMessageSlot::WorkspacePreparation);
        let Some(preparation) = preparation else {
            return;
        };
        let mut text = match preparation.state {
            SessionPreparationState::Preparing if preparation.prompt.is_none() => String::new(),
            SessionPreparationState::Preparing => "Preparing workspace. You can keep typing; \
                                                   submitted prompts wait for setup."
                .to_string(),
            SessionPreparationState::Failed => format!(
                "Workspace setup failed: {}\nPress s to retry. Your saved prompt is retained.",
                preparation.error.as_deref().unwrap_or("Unknown error")
            ),
            SessionPreparationState::Ready | SessionPreparationState::Canceled => return,
        };
        if let Some(prompt) = preparation.prompt.as_deref().and_then(|value| {
            serde_json::from_str::<crate::domain::turn_prompt::TurnPrompt>(value).ok()
        }) {
            text.push_str("\n\nSaved prompt:\n");
            text.push_str(&prompt.transcript_text());
        }
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Plain(text),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::WorkspacePreparation,
            turn_position: None,
        });
    }

    /// Loads session models from the database using the provided filesystem
    /// boundary to decide which session folders exist.
    ///
    /// Existing handles are reused in place to preserve `Arc` identity so
    /// that background workers holding cloned references continue to work.
    ///
    /// When a handle already exists, live handle output is treated as
    /// authoritative for the returned in-memory snapshot to avoid clobbering
    /// fresh runtime output with stale persisted rows. Active statuses are also
    /// preserved from live handles, while terminal persisted statuses (`Done`,
    /// `Canceled`) override stale in-memory status.
    ///
    /// Retired persisted model ids are upgraded to their current replacement
    /// models while rows are loaded. Sessions that are still active also have
    /// the replacement persisted so future turns run on the current model;
    /// terminal sessions keep their retired model id in the database as a
    /// historical record.
    ///
    /// New handles are inserted for sessions that don't have entries yet.
    ///
    /// Transcript-scale fields are loaded only for `active_session_id`; other
    /// rows receive empty detail fields until the session is opened.
    ///
    /// Rows with unsupported permission modes are logged and skipped so one
    /// corrupt session cannot hide valid siblings or appear write-capable.
    ///
    /// Returns loaded sessions, local-day activity counts aggregated from
    /// persisted session-creation activity history, and cached worktree
    /// availability keyed by session id.
    pub(crate) async fn load_sessions_with_fs_client(
        input: SessionLoadInput<'_>,
        handles: &mut HashMap<SessionId, SessionHandles>,
    ) -> (Vec<Session>, Vec<DailyActivity>, HashMap<SessionId, bool>) {
        Self::try_load_sessions_with_fs_client(input, handles)
            .await
            .unwrap_or_default()
    }

    /// Loads session snapshots while preserving a session-list read failure.
    ///
    /// Refresh callers use this fallible path so a transient database error
    /// cannot be mistaken for an empty project and tear down live workers.
    ///
    /// # Errors
    /// Returns an error when the project's session rows cannot be loaded.
    pub(crate) async fn try_load_sessions_with_fs_client(
        input: SessionLoadInput<'_>,
        handles: &mut HashMap<SessionId, SessionHandles>,
    ) -> Result<(Vec<Session>, Vec<DailyActivity>, HashMap<SessionId, bool>), DbError> {
        let SessionLoadInput {
            active_project_id,
            active_session_id,
            base,
            clock,
            db,
            fs_client,
            working_dir,
        } = input;
        let project_name = working_dir
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_string();

        let db_rows = db
            .sessions()
            .load_sessions_for_project(active_project_id)
            .await?;
        let activity_timestamps = db
            .activity()
            .load_session_activity_timestamps()
            .await
            .unwrap_or_default();
        let stats_activity = Self::daily_activity_from_timestamps(activity_timestamps, clock);
        let orchestration_metadata =
            orchestration::session_metadata_for_project(db, active_project_id).await;
        let mut sessions: Vec<Session> = Vec::new();
        let mut session_worktree_availability = HashMap::new();

        let preparations = db
            .sessions()
            .load_session_preparations(active_project_id)
            .await?
            .into_iter()
            .map(|row| (row.session_id.clone(), row))
            .collect();
        let mut load_context = LoadSessionContext {
            preparations: &preparations,
            base,
            db,
            project_name: &project_name,
            handles,
            fs_client,
            active_session_id,
            orchestration_metadata: &orchestration_metadata,
            sessions: &mut sessions,
            session_worktree_availability: &mut session_worktree_availability,
        };
        for row in db_rows {
            let Ok(permission_mode) = row.permission_mode.parse() else {
                warn!(
                    session_id = %row.id,
                    permission_mode = %row.permission_mode,
                    "skipping session with unsupported permission mode"
                );

                continue;
            };
            Self::push_loaded_session_row(&mut load_context, row, permission_mode).await;
        }

        Ok((sessions, stats_activity, session_worktree_availability))
    }

    /// Aggregates persisted activity timestamps using the clock-provided
    /// offset active for each event.
    fn daily_activity_from_timestamps(
        timestamps: Vec<i64>,
        clock: &dyn Clock,
    ) -> Vec<DailyActivity> {
        let mut activity_by_day = BTreeMap::<i64, u32>::new();
        for timestamp_seconds in timestamps {
            let utc_offset_seconds = clock.local_utc_offset_seconds(timestamp_seconds);
            let day_key = activity_day_key_with_offset(timestamp_seconds, utc_offset_seconds);
            let session_count = activity_by_day.entry(day_key).or_default();
            *session_count = session_count.saturating_add(1);
        }

        activity_by_day
            .into_iter()
            .map(|(day_key, session_count)| DailyActivity {
                day_key,
                session_count,
            })
            .collect()
    }

    /// Loads one persisted session row into `sessions`, reusing existing
    /// handles when present and registering a new handle otherwise.
    async fn push_loaded_session_row(
        load_context: &mut LoadSessionContext<'_>,
        row: SessionListRow,
        permission_mode: PermissionMode,
    ) {
        let LoadSessionContext {
            base,
            db,
            project_name,
            handles,
            orchestration_metadata,
            fs_client,
            active_session_id,
            sessions,
            session_worktree_availability,
            preparations,
        } = load_context;
        let session_id = SessionId::from(row.id.clone());
        let folder = session_folder(base, &session_id);
        let persisted_status = row.status.parse::<Status>().unwrap_or(Status::Done);
        let persisted_size = row.size.parse::<SessionSize>().unwrap_or_default();
        let has_session_folder = fs_client.is_dir(folder.clone());
        let live_handle_status = handles
            .get(&session_id)
            .and_then(|existing| existing.status.lock().ok().map(|status| *status));

        if should_skip_missing_folder_session(
            has_session_folder || preparations.contains_key(&row.id),
            row.is_draft,
            persisted_status,
            live_handle_status,
        ) {
            return;
        }

        let workspace_ready = preparations
            .get(&row.id)
            .is_none_or(|preparation| preparation.state == SessionPreparationState::Ready);
        session_worktree_availability
            .insert(session_id.clone(), has_session_folder && workspace_ready);

        let (session_detail, session_status, session_transcript) =
            Self::load_session_detail_and_transcript(
                db,
                *active_session_id,
                handles,
                &session_id,
                &row.id,
                persisted_status,
            )
            .await;
        let session_agent =
            migrate_session_off_retired_model(db, &row.id, &row.agent, &row.model, session_status)
                .await;
        let draft_attachments =
            draft::load_staged_draft_attachments(*fs_client, base, &session_id).await;
        let questions = Self::loaded_session_questions(session_detail.as_ref());
        let reasoning_level_override = row
            .reasoning_level_override
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok());
        let speed_mode = row.speed_mode.parse::<SpeedMode>().unwrap_or_default();
        let response_style = row
            .response_style
            .parse::<ResponseStyle>()
            .unwrap_or_default();
        let (session_queued_messages, session_queued_actions) =
            Self::loaded_queue_snapshots(handles.get(&session_id));
        let (role, orchestration_metadata) =
            Self::loaded_orchestration_metadata(&row, orchestration_metadata);
        let preparation = preparations.get(&row.id);
        let mut session = Self::build_loaded_session(LoadedSessionInput {
            controller_session_id: orchestration_metadata.controller_session_id,
            draft_attachments,
            follow_up_tasks: Vec::new(),
            folder,
            parent_session_id: row.parent_session_id.clone().map(SessionId::from),
            orchestration_progress: orchestration_metadata.progress,
            permission_mode,
            project_name: (*project_name).to_string(),
            reasoning_level_override,
            response_style,
            review_request: parse_review_request(&row),
            role,
            row,
            session_agent,
            session_id,
            session_prompt: session_detail
                .as_ref()
                .map(|detail| detail.prompt.clone())
                .unwrap_or_default(),
            session_queued_actions,
            session_queued_messages,
            session_questions: questions,
            session_status,
            session_transcript,
            size: persisted_size,
            speed_mode,
        });
        Self::apply_workspace_preparation(&mut session, preparation);
        sessions.push(session);
    }

    async fn load_session_detail_and_transcript(
        db: &AppRepositories,
        active_session_id: Option<&str>,
        handles: &mut HashMap<SessionId, SessionHandles>,
        session_id: &SessionId,
        row_id: &str,
        persisted_status: Status,
    ) -> (Option<SessionDetailRow>, Status, Option<SessionTranscript>) {
        let (session_detail, loaded_transcript) =
            load_active_session_detail(db, active_session_id, row_id).await;
        let (session_status, session_transcript) =
            if let Some(existing_handle) = handles.get(session_id) {
                status_and_transcript_from_existing_handle(
                    existing_handle,
                    persisted_status,
                    loaded_transcript.as_ref(),
                )
            } else {
                let transcript = insert_loaded_session_handle(
                    handles,
                    session_id.clone(),
                    persisted_status,
                    loaded_transcript,
                );

                (persisted_status, transcript)
            };

        (session_detail, session_status, session_transcript)
    }

    fn loaded_session_questions(session_detail: Option<&SessionDetailRow>) -> Vec<QuestionItem> {
        session_detail
            .and_then(|detail| detail.questions.as_deref())
            .and_then(parse_questions_json)
            .unwrap_or_default()
    }

    fn loaded_queue_snapshots(
        handles: Option<&SessionHandles>,
    ) -> (Vec<QueuedMessage>, Vec<TransientMessage>) {
        handles
            .map(|handles| {
                (
                    handles.queued_message_snapshot(),
                    handles.queued_action_snapshot(),
                )
            })
            .unwrap_or_default()
    }

    fn loaded_orchestration_metadata(
        row: &SessionListRow,
        metadata: &HashMap<String, orchestration::OrchestrationSessionMetadata>,
    ) -> (SessionRole, orchestration::OrchestrationSessionMetadata) {
        let role = row
            .role
            .as_deref()
            .and_then(|value| value.parse::<SessionRole>().ok())
            .unwrap_or_default();
        let metadata = metadata.get(&row.id).cloned().unwrap_or_default();

        (role, metadata)
    }

    /// Computes diff-derived session metadata from one worktree folder using
    /// the injected filesystem and Git boundaries.
    ///
    /// Missing folders and Git failures return [`SessionDiffStats::Unknown`]
    /// so callers retain diagnostic diff access without overwriting the last
    /// known line totals.
    pub(crate) async fn session_diff_stats_for_folder(
        fs_client: &dyn FsClient,
        git_client: &dyn GitClient,
        folder: &Path,
        base_branch: &str,
    ) -> SessionDiffStats {
        if !fs_client.is_dir(folder.to_path_buf()) {
            return SessionDiffStats::Unknown;
        }

        let folder = folder.to_path_buf();
        let base_branch = base_branch.to_string();
        let Ok(diff) = git_client.diff(folder, base_branch).await else {
            return SessionDiffStats::Unknown;
        };

        SessionDiffStats::from_diff(&diff)
    }

    /// Loads transcript-scale detail for one session into the in-memory
    /// snapshot and runtime handles when the user opens that session.
    pub(crate) async fn load_session_detail_into_state(
        &mut self,
        db: &AppRepositories,
        session_id: &str,
    ) {
        let Some(detail) = db
            .sessions()
            .load_session_detail(session_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Ok(transcript) = load_session_transcript(db, session_id).await else {
            return;
        };

        self.apply_session_detail(session_id, detail, transcript);
    }

    /// Builds one in-memory session snapshot from a database row plus the
    /// transient fields computed during reload.
    fn build_loaded_session(input: LoadedSessionInput) -> Session {
        let mut session = Session {
            agent: input.session_agent,
            base_branch: input.row.base_branch,
            created_at: input.row.created_at,
            controller_session_id: input.controller_session_id,
            draft_attachments: input.draft_attachments,
            folder: input.folder,
            follow_up_tasks: input.follow_up_tasks,
            id: input.session_id,
            in_progress_started_at: input.row.in_progress_started_at,
            in_progress_total_seconds: input.row.in_progress_total_seconds,
            is_draft: input.row.is_draft,
            orchestration_progress: input.orchestration_progress,
            parent_session_id: input.parent_session_id,
            permission_mode: input.permission_mode,
            personality_id: input.row.personality_id,
            project_name: input.project_name,
            prompt: input.session_prompt,
            queued_messages: input.session_queued_messages,
            reasoning_level_override: input.reasoning_level_override,
            response_style: input.response_style,
            published_upstream_ref: input.row.published_upstream_ref,
            questions: input.session_questions,
            review_request: input.review_request,
            role: input.role,
            size: input.size,
            speed_mode: input.speed_mode,
            stats: SessionStats {
                added_lines: input.row.added_lines.cast_unsigned(),
                deleted_lines: input.row.deleted_lines.cast_unsigned(),
                diff_state: match input.row.has_diff {
                    Some(true) => SessionDiffState::Present,
                    Some(false) => SessionDiffState::Empty,
                    None => SessionDiffState::Unknown,
                },
                input_tokens: input.row.input_tokens.cast_unsigned(),
                output_tokens: input.row.output_tokens.cast_unsigned(),
            },
            status: input.session_status,
            title: input.row.title,
            transcript: input.session_transcript,
            updated_at: input.row.updated_at,
            transient_messages: TransientMessageStore::default(),
        };
        for queued_action in input.session_queued_actions {
            session.transient_messages.upsert(queued_action);
        }
        session
    }

    /// Applies one lazily loaded detail row and message transcript to the
    /// session snapshot and its shared runtime handle without clobbering live
    /// in-process transcript messages.
    fn apply_session_detail(
        &mut self,
        session_id: &str,
        detail: SessionDetailRow,
        transcript: SessionTranscript,
    ) {
        let session_transcript = self
            .state
            .handle(session_id)
            .and_then(|handles| sync_handle_transcript_with_loaded(handles, Some(&transcript)))
            .or_else(|| Some(transcript).filter(|transcript| !transcript.is_empty()));

        let Some(session) = self.state.session_mut_for_id(session_id) else {
            return;
        };

        session.prompt = detail.prompt;
        if let Some(questions) = detail.questions {
            session.questions = parse_questions_json(&questions).unwrap_or_default();
        }
        session.transcript = session_transcript;
    }
}

/// Loads active-session detail metadata and transcript text for the selected
/// row only.
async fn load_active_session_detail(
    db: &AppRepositories,
    active_session_id: Option<&str>,
    row_id: &str,
) -> (Option<SessionDetailRow>, Option<SessionTranscript>) {
    if active_session_id.is_none_or(|active_id| active_id != row_id) {
        return (None, None);
    }

    let Some(detail) = db
        .sessions()
        .load_session_detail(row_id)
        .await
        .ok()
        .flatten()
    else {
        return (None, None);
    };
    let transcript = load_session_transcript(db, row_id).await.ok();

    (Some(detail), transcript)
}

/// Reads status/transcript from an existing handle while hydrating an empty
/// transcript from lazily loaded detail when the session has become active.
fn status_and_transcript_from_existing_handle(
    existing_handle: &SessionHandles,
    persisted_status: Status,
    loaded_transcript: Option<&SessionTranscript>,
) -> (Status, Option<SessionTranscript>) {
    let status_from_handle = existing_handle
        .status
        .lock()
        .ok()
        .map_or(persisted_status, |status| *status);
    let merged_status = merge_loaded_session_status(persisted_status, status_from_handle);

    if let Ok(mut handle_status) = existing_handle.status.lock() {
        *handle_status = merged_status;
    }
    let transcript_from_handle =
        sync_handle_transcript_with_loaded(existing_handle, loaded_transcript);

    (merged_status, transcript_from_handle)
}

/// Inserts a new runtime handle using active-session detail when it is
/// available and returns the transcript snapshot stored in that handle.
fn insert_loaded_session_handle(
    handles: &mut HashMap<SessionId, SessionHandles>,
    session_id: SessionId,
    persisted_status: Status,
    loaded_transcript: Option<SessionTranscript>,
) -> Option<SessionTranscript> {
    let session_transcript = loaded_transcript.filter(|transcript| !transcript.is_empty());
    let session_handle = if let Some(transcript) = session_transcript.clone() {
        SessionHandles::new_with_transcript(persisted_status, transcript)
    } else {
        SessionHandles::new_unloaded(persisted_status)
    };
    handles.insert(session_id, session_handle);

    session_transcript
}

/// Loads ordered session messages into the render transcript snapshot.
pub(crate) async fn load_session_transcript(
    db: &AppRepositories,
    session_id: &str,
) -> Result<SessionTranscript, DbError> {
    let messages = db.sessions().load_session_messages(session_id).await?;

    Ok(SessionTranscript::new(session_messages_from_rows(messages)))
}

/// Synchronizes a handle from loaded rows while preserving complete live
/// transcripts and replacing partial unhydrated snapshots.
fn sync_handle_transcript_with_loaded(
    handles: &SessionHandles,
    loaded_transcript: Option<&SessionTranscript>,
) -> Option<SessionTranscript> {
    handles.transcript_snapshot_with_loaded(loaded_transcript)
}

/// Converts database message rows into domain messages, skipping unknown
/// message kinds left by older database revisions.
fn session_messages_from_rows(rows: Vec<SessionMessageRow>) -> Vec<SessionMessage> {
    rows.into_iter()
        .filter_map(|row| {
            row.kind
                .parse::<SessionMessageKind>()
                .ok()
                .map(|kind| SessionMessage::new(row.position, kind, row.content))
        })
        .collect()
}

/// Returns whether one persisted session row should be skipped because its
/// worktree folder is missing and no merge-cleanup transition is still active.
fn should_skip_missing_folder_session(
    has_session_folder: bool,
    is_draft_session: bool,
    persisted_status: Status,
    live_handle_status: Option<Status>,
) -> bool {
    if has_session_folder {
        return false;
    }

    if matches!(
        persisted_status,
        Status::Merged | Status::Done | Status::Canceled
    ) {
        return false;
    }

    if is_draft_session && persisted_status == Status::Draft {
        return false;
    }

    !matches!(
        live_handle_status,
        Some(Status::Merging | Status::Merged | Status::Done | Status::Canceled)
    )
}

/// Merges one loaded status with the existing live-handle status.
///
/// Existing handle status is kept for active transitions to prevent stale DB
/// snapshots from clobbering in-memory updates. Persisted read-only and
/// terminal statuses (`Merged`, `Done`, `Canceled`) take precedence so remote
/// merge truth and explicit terminal transitions still appear after refresh.
fn merge_loaded_session_status(status_from_db: Status, status_from_handle: Status) -> Status {
    if matches!(
        status_from_db,
        Status::Merged | Status::Done | Status::Canceled
    ) {
        return status_from_db;
    }

    status_from_handle
}

/// Parses normalized review-request metadata from one loaded database row.
///
/// Incomplete or invalid persisted metadata is ignored so stale partial rows do
/// not block session loading.
fn parse_review_request(row: &SessionListRow) -> Option<ReviewRequest> {
    let review_request_row = row.review_request.as_ref()?;
    let forge_kind = parse_optional_enum(Some(review_request_row.forge_kind.as_str())).ok()?;
    let state = parse_optional_enum(Some(review_request_row.state.as_str())).ok()?;

    Some(ReviewRequest {
        last_refreshed_at: review_request_row.last_refreshed_at,
        summary: ReviewRequestSummary {
            display_id: review_request_row.display_id.clone(),
            forge_kind,
            source_branch: review_request_row.source_branch.clone(),
            state,
            status_summary: review_request_row.status_summary.clone(),
            target_branch: review_request_row.target_branch.clone(),
            title: review_request_row.title.clone(),
            web_url: review_request_row.web_url.clone(),
        },
    })
}

/// Converts one optional persisted string into a parsed enum value.
fn parse_optional_enum<T>(value: Option<&str>) -> Result<T, ()>
where
    T: std::str::FromStr,
{
    value.ok_or(())?.parse().map_err(|_| ())
}

/// Parses persisted question JSON with backward compatibility.
///
/// Attempts to deserialize as `Vec<QuestionItem>` first (new format). Falls
/// back to `Vec<String>` (legacy format) and converts each entry into a
/// `QuestionItem` without predefined options.
fn parse_questions_json(raw_json: &str) -> Option<Vec<QuestionItem>> {
    if raw_json.is_empty() {
        return None;
    }

    if let Ok(items) = serde_json::from_str::<Vec<QuestionItem>>(raw_json) {
        return Some(items);
    }

    serde_json::from_str::<Vec<String>>(raw_json)
        .ok()
        .map(|texts| {
            texts
                .into_iter()
                .map(|text| QuestionItem {
                    options: Vec::new(),
                    text,
                })
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::{Instant, SystemTime};

    use ag_git::{GitError, MockGitClient};

    use super::*;
    use crate::domain::session::{ForgeKind, ReviewRequestState, ReviewRequestSummary};
    use crate::domain::transient_message::{
        QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
        TransientMessageLifecycle, TransientMessageSlot,
    };
    use crate::infra::clock::RealClock;
    use crate::infra::db::SessionReviewRequestRow;
    use crate::infra::fs;

    #[test]
    fn test_workspace_preparation_shows_failures_and_clears_them_on_retry() {
        // Arrange
        let mut session = crate::test_support::session_fixture("preparing", Status::Draft);
        let mut preparation = SessionPreparationRow {
            error: Some("setup failed".to_string()),
            prompt: None,
            session_id: session.id.to_string(),
            start_ref: "main".to_string(),
            state: SessionPreparationState::Failed,
        };

        // Act
        SessionManager::apply_workspace_preparation(&mut session, Some(&preparation));

        // Assert
        let notice = session
            .transient_messages
            .get(TransientMessageSlot::WorkspacePreparation)
            .expect("setup failure remains visible");
        assert!(matches!(&notice.body, TransientMessageBody::Plain(text)
            if text.contains("setup failed") && text.contains("Press s to retry")));

        // Act
        preparation.state = SessionPreparationState::Preparing;
        SessionManager::apply_workspace_preparation(&mut session, Some(&preparation));

        // Assert
        let notice = session
            .transient_messages
            .get(TransientMessageSlot::WorkspacePreparation)
            .expect("preparation remains available to lifecycle actions");
        assert!(matches!(&notice.body, TransientMessageBody::Plain(text) if text.is_empty()));
        assert!(session.allows_cancel_action());
    }

    /// Clock fixture that supplies event-specific offsets for activity rows.
    struct ActivityOffsetClock;

    impl Clock for ActivityOffsetClock {
        fn local_utc_offset_seconds(&self, timestamp_seconds: i64) -> i64 {
            if timestamp_seconds < 86_400 {
                3_600
            } else {
                -3_600
            }
        }

        fn now_instant(&self) -> Instant {
            Instant::now()
        }

        fn now_system_time(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    fn session_replay_text(session: &Session) -> String {
        session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .unwrap_or_default()
    }

    fn assistant_transcript(content: impl AsRef<str>) -> SessionTranscript {
        SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            content.as_ref(),
        )])
    }

    fn assistant_replay_text(content: impl AsRef<str>) -> String {
        assistant_transcript(content)
            .replay_text()
            .expect("assistant transcript should have replay text")
    }

    #[tokio::test]
    async fn load_sessions_skips_invalid_permission_mode_without_hiding_valid_siblings() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("project should be created");
        for session_id in ["valid-mode", "invalid-mode"] {
            db.sessions()
                .insert_draft_session(session_id, "gpt-5.6-sol", "main", "Draft", project_id)
                .await
                .expect("session should be created");
        }
        sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'invalid-mode'")
            .execute(&pool)
            .await
            .expect("permission mode should be corrupted");
        let mock_fs_client = create_folder_lookup_mock(Vec::new());
        let mut handles = HashMap::new();

        // Act
        let (sessions, _, session_worktree_availability) =
            SessionManager::load_sessions_with_fs_client(
                SessionLoadInput {
                    active_project_id: project_id,
                    active_session_id: None,
                    base: Path::new("/virtual/session-base"),
                    clock: &RealClock,
                    db: &db,
                    fs_client: &mock_fs_client,
                    working_dir: Path::new("/tmp/test"),
                },
                &mut handles,
            )
            .await;

        // Assert
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "valid-mode");
        assert_eq!(sessions[0].permission_mode, PermissionMode::AutoEdit);
        assert!(handles.contains_key("valid-mode"));
        assert!(!handles.contains_key("invalid-mode"));
        assert_eq!(
            session_worktree_availability.get("valid-mode"),
            Some(&false)
        );
        assert!(!session_worktree_availability.contains_key("invalid-mode"));
    }

    #[test]
    fn daily_activity_uses_clock_offset_for_each_timestamp() {
        // Arrange
        let timestamps = vec![86_399, 86_400, 86_399];
        let clock = ActivityOffsetClock;

        // Act
        let activity = SessionManager::daily_activity_from_timestamps(timestamps, &clock);
        let monotonic_time = clock.now_instant();
        let system_time = clock.now_system_time();

        // Assert
        assert!(monotonic_time <= Instant::now());
        assert_eq!(system_time, SystemTime::UNIX_EPOCH);
        assert_eq!(
            activity,
            vec![
                DailyActivity {
                    day_key: 0,
                    session_count: 1,
                },
                DailyActivity {
                    day_key: 1,
                    session_count: 2,
                },
            ]
        );
    }

    #[tokio::test]
    async fn session_diff_stats_preserve_binary_presence_and_git_errors() {
        // Arrange
        let folder = PathBuf::from("/tmp/session");
        let existing_folder_client = create_folder_lookup_mock(vec![folder.clone()]);
        let missing_folder_client = create_folder_lookup_mock(Vec::new());
        let mut binary_diff_client = MockGitClient::new();
        binary_diff_client.expect_diff().times(1).returning(|_, _| {
            Box::pin(async {
                Ok("diff --git a/image.png b/image.png\nBinary files differ\n".to_string())
            })
        });
        let mut failing_diff_client = MockGitClient::new();
        failing_diff_client
            .expect_diff()
            .times(1)
            .returning(|_, _| {
                Box::pin(async { Err(GitError::OutputParse("diff failed".to_string())) })
            });

        // Act
        let binary_stats = SessionManager::session_diff_stats_for_folder(
            &existing_folder_client,
            &binary_diff_client,
            &folder,
            "main",
        )
        .await;
        let error_stats = SessionManager::session_diff_stats_for_folder(
            &existing_folder_client,
            &failing_diff_client,
            &folder,
            "main",
        )
        .await;
        let missing_folder_stats = SessionManager::session_diff_stats_for_folder(
            &missing_folder_client,
            &MockGitClient::new(),
            &folder,
            "main",
        )
        .await;

        // Assert
        assert_eq!(
            binary_stats,
            SessionDiffStats::Known {
                added_lines: 0,
                deleted_lines: 0,
                has_diff: true,
                session_size: SessionSize::Xs,
            }
        );
        assert_eq!(error_stats, SessionDiffStats::Unknown);
        assert_eq!(missing_folder_stats, SessionDiffStats::Unknown);
    }

    /// Returns a filesystem mock that reports the supplied directories as
    /// existing and treats missing staged-draft metadata files as absent.
    fn create_folder_lookup_mock(existing_folders: Vec<PathBuf>) -> fs::MockFsClient {
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_is_dir()
            .times(0..)
            .returning(move |path| existing_folders.contains(&path));
        mock_fs_client.expect_read_file().times(0..).returning(|_| {
            Box::pin(async {
                Err(fs::FsError::Io(std::io::Error::from(
                    std::io::ErrorKind::NotFound,
                )))
            })
        });

        mock_fs_client
    }

    /// Ensures reload keeps live handle output and active status when
    /// persisted row data is stale.
    #[tokio::test]
    async fn test_load_sessions_preserves_live_handle_output_and_status() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.sessions()
            .insert_session(
                session_id,
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.sessions()
            .append_session_message(session_id, SessionMessageKind::AssistantAnswer, "DB Output")
            .await
            .expect("failed to append persisted message");
        db.sessions()
            .mark_session_diff_unknown(session_id)
            .await
            .expect("failed to mark session diff unknown");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
        let live_output = "Live Output".to_string();
        let live_status = Status::Review;
        handles.insert(
            session_id.to_string().into(),
            SessionHandles::new_with_transcript(live_status, assistant_transcript(&live_output)),
        );

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(
            session_replay_text(session),
            assistant_replay_text(&live_output)
        );
        assert_eq!(session.status, live_status);
        assert_eq!(session.stats.diff_state, SessionDiffState::Unknown);

        let handle = handles
            .get(session_id)
            .expect("missing existing runtime handle");
        let handle_output = handle
            .transcript
            .lock()
            .expect("failed to lock handle transcript")
            .replay_text()
            .unwrap_or_default();
        let handle_status = *handle.status.lock().expect("failed to lock handle status");
        assert_eq!(handle_output, assistant_replay_text(&live_output));
        assert_eq!(handle_status, live_status);
    }

    /// Ensures project-scoped reloads reconstruct queued workflow rows from
    /// the live session handles that still own their worker commands.
    #[tokio::test]
    async fn test_load_sessions_restores_queued_actions_from_live_handles() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let session_id = SessionId::from("queued-session");
        db.sessions()
            .insert_session(
                &session_id,
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client =
            create_folder_lookup_mock(vec![session_folder(base_path, &session_id)]);
        let handles = SessionHandles::new(Status::InProgress);
        handles.upsert_queued_action(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Queued(QueuedAction::new(
                3,
                "sync after this turn".to_string(),
            )),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::SyncQueue,
            turn_position: Some(0),
        });
        let mut handles_by_session = HashMap::from([(session_id.clone(), handles)]);

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: Some(&session_id),
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles_by_session,
        )
        .await;

        // Assert
        let queued_action = sessions[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .expect("queued sync should be restored");
        assert!(matches!(
            &queued_action.body,
            TransientMessageBody::Queued(action)
                if action.order == 3 && action.text == "sync after this turn"
        ));
    }

    /// Ensures reload caches worktree availability alongside loaded session
    /// rows.
    #[tokio::test]
    async fn test_load_sessions_reports_worktree_availability() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let session_with_worktree_id = "worktree-available";
        let session_without_worktree_id = "draft-missing";
        db.sessions()
            .insert_session(
                session_with_worktree_id,
                "gemini-3.8-flash",
                "main",
                "Draft",
                project_id,
            )
            .await
            .expect("failed to insert session with worktree");
        db.sessions()
            .insert_draft_session(
                session_without_worktree_id,
                "gemini-3.8-flash",
                "main",
                "Draft",
                project_id,
            )
            .await
            .expect("failed to insert draft session");

        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client =
            create_folder_lookup_mock(vec![session_folder(base_path, session_with_worktree_id)]);
        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

        // Act
        let (_, _, session_worktree_availability) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        assert_eq!(
            session_worktree_availability.get(session_with_worktree_id),
            Some(&true)
        );
        assert_eq!(
            session_worktree_availability.get(session_without_worktree_id),
            Some(&false)
        );
    }

    /// Ensures reload reads persisted detail for active sessions.
    #[tokio::test]
    async fn test_load_sessions_reads_persisted_detail_for_active_session() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.sessions()
            .insert_session(session_id, "gemini-3.8-flash", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_prompt(session_id, "persisted prompt")
            .await
            .expect("failed to update session prompt");
        db.sessions()
            .update_session_questions(
                session_id,
                r#"[{"text":"persisted question?","options":["Yes"]}]"#,
            )
            .await
            .expect("failed to update session questions");
        db.sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::AssistantAnswer,
                "persisted output",
            )
            .await
            .expect("failed to append session message");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
        handles.insert(
            session_id.to_string().into(),
            SessionHandles::new_with_transcript(
                Status::Review,
                assistant_transcript("Live Output"),
            ),
        );

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: Some(session_id),
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(
            session_replay_text(session),
            assistant_replay_text("Live Output")
        );
        assert_eq!(session.prompt, "persisted prompt");
        assert_eq!(
            session.questions,
            vec![QuestionItem {
                options: vec!["Yes".to_string()],
                text: "persisted question?".to_string(),
            }]
        );
    }

    /// Ensures inactive session refresh skips transcript-scale fields.
    #[tokio::test]
    async fn test_load_sessions_defers_persisted_detail_for_inactive_session() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "inactive-session";
        db.sessions()
            .insert_session(session_id, "gemini-3.8-flash", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_prompt(session_id, "large prompt")
            .await
            .expect("failed to update prompt");
        db.sessions()
            .update_session_questions(session_id, r#"["Need detail?"]"#)
            .await
            .expect("failed to update questions");
        db.sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::AssistantAnswer,
                "large output",
            )
            .await
            .expect("failed to append message");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);
        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session_replay_text(session), "");
        assert_eq!(session.prompt, "");
        assert_eq!(session.questions, [] as [ag_protocol::QuestionItem; 0]);
        let handle = handles.get(session_id).expect("missing runtime handle");
        let handle_output = handle
            .transcript
            .lock()
            .expect("failed to lock transcript")
            .replay_text();
        assert_eq!(handle_output, None);
    }

    /// Ensures active reload hydrates an existing empty handle from persisted
    /// transcript detail.
    #[tokio::test]
    async fn test_load_sessions_hydrates_empty_handle_for_active_session() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "active-session";
        db.sessions()
            .insert_session(session_id, "gemini-3.8-flash", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        db.sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::AssistantAnswer,
                "persisted output",
            )
            .await
            .expect("failed to append message");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);
        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
        handles.insert(
            session_id.to_string().into(),
            SessionHandles::new_unloaded(Status::Review),
        );

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: Some(session_id),
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(
            session_replay_text(session),
            assistant_replay_text("persisted output")
        );

        let handle = handles.get(session_id).expect("missing runtime handle");
        let handle_output = handle
            .transcript
            .lock()
            .expect("failed to lock transcript")
            .replay_text()
            .unwrap_or_default();
        assert_eq!(handle_output, assistant_replay_text("persisted output"));
    }

    /// Ensures transcript loading returns database failures instead of
    /// converting them into empty transcript text.
    #[tokio::test]
    async fn test_load_session_transcript_returns_query_errors() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        sqlx::query!("DROP TABLE session_message")
            .execute(&pool)
            .await
            .expect("failed to drop session_message table");

        // Act
        let error = load_session_transcript(&db, "missing-session")
            .await
            .expect_err("transcript load should fail");

        // Assert
        assert!(matches!(error, DbError::Query(_)));
    }

    /// Ensures terminal persisted statuses replace stale active handle status
    /// during reload.
    #[tokio::test]
    async fn test_load_sessions_terminal_db_status_overrides_handle_status() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");

        let session_id = "test-session";
        db.sessions()
            .insert_session(session_id, "gemini-3.8-flash", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        let base_path = Path::new("/virtual/session-base");
        let session_dir = session_folder(base_path, session_id);
        let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
        handles.insert(
            session_id.to_string().into(),
            SessionHandles::new_with_transcript(Status::Review, assistant_transcript("output")),
        );

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.status, Status::Done);

        let handle = handles
            .get(session_id)
            .expect("missing existing runtime handle");
        let handle_status = *handle.status.lock().expect("failed to lock handle status");
        assert_eq!(handle_status, Status::Done);
    }

    /// Ensures still-active sessions on a retired model are switched to the
    /// replacement model both in memory and in the database.
    #[tokio::test]
    async fn test_load_sessions_switches_active_session_off_retired_model() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let session_id = "retired-active-session";
        db.sessions()
            .insert_session(session_id, "gemini-3.1-pro", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client = create_folder_lookup_mock(vec![session_folder(base_path, session_id)]);
        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.agent.model(), AgentModel::Gemini31Pro);
        let row = db
            .sessions()
            .load_session(session_id)
            .await
            .expect("failed to load session row")
            .expect("missing session row");
        assert_eq!(row.model, "gemini-3.1-pro-preview");
        assert_eq!(row.agent, "antigravity");
    }

    /// Ensures automatic model migration does not make an old session appear
    /// recently active.
    #[tokio::test]
    async fn test_migrate_session_preserves_updated_at() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let session_id = "retired-timestamp-session";
        db.sessions()
            .insert_session(session_id, "claude-opus-4-6", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(123_i64)
        .bind(session_id)
        .execute(&pool)
        .await
        .expect("failed to set historical timestamp");

        // Act
        migrate_session_off_retired_model(
            &db,
            session_id,
            "claude",
            "claude-opus-4-6",
            Status::Review,
        )
        .await;

        // Assert
        let row = db
            .sessions()
            .load_session(session_id)
            .await
            .expect("failed to load migrated session")
            .expect("missing migrated session");
        assert_eq!(row.agent, "claude");
        assert_eq!(row.model, "claude-opus-5");
        assert_eq!(row.updated_at, 123);
    }

    /// Ensures startup migration covers active sessions outside the currently
    /// loaded project while retaining terminal-session history.
    #[tokio::test]
    async fn test_migrate_active_sessions_off_retired_models_covers_inactive_projects() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let active_project_id = db
            .projects()
            .upsert_project("/tmp/active", None)
            .await
            .expect("failed to upsert active project");
        let inactive_project_id = db
            .projects()
            .upsert_project("/tmp/inactive", None)
            .await
            .expect("failed to upsert inactive project");
        db.sessions()
            .insert_session(
                "active-project-session",
                "claude-opus-4-6",
                "main",
                "Review",
                active_project_id,
            )
            .await
            .expect("failed to insert active-project session");
        db.sessions()
            .insert_session(
                "inactive-project-session",
                "gemini-3.5-flash",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("failed to insert inactive-project session");
        db.sessions()
            .insert_session(
                "inactive-project-finished",
                "gemini-3.5-flash",
                "main",
                "Done",
                inactive_project_id,
            )
            .await
            .expect("failed to insert finished inactive-project session");

        // Act
        migrate_active_sessions_off_retired_models(&db).await;

        // Assert
        let active_project_row = db
            .sessions()
            .load_session("active-project-session")
            .await
            .expect("failed to load active-project session")
            .expect("missing active-project session");
        let inactive_project_row = db
            .sessions()
            .load_session("inactive-project-session")
            .await
            .expect("failed to load inactive-project session")
            .expect("missing inactive-project session");
        let finished_row = db
            .sessions()
            .load_session("inactive-project-finished")
            .await
            .expect("failed to load finished inactive-project session")
            .expect("missing finished inactive-project session");
        assert_eq!(active_project_row.model, "claude-opus-5");
        assert_eq!(active_project_row.agent, "claude");
        assert_eq!(inactive_project_row.model, "gemini-3.5-flash-lite");
        assert_eq!(inactive_project_row.agent, "antigravity");
        assert_eq!(finished_row.model, "gemini-3.5-flash");
    }

    /// Ensures a terminal transition after the migration read wins the race
    /// and preserves the retired model id as history.
    #[tokio::test]
    async fn test_migrate_session_preserves_retired_model_after_terminal_transition() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let race_cases = [
            ("race-merged", "Merged"),
            ("race-done", "Done"),
            ("race-canceled", "Canceled"),
        ];
        for (session_id, _) in race_cases {
            db.sessions()
                .insert_session(session_id, "claude-opus-4-6", "main", "Review", project_id)
                .await
                .expect("failed to insert active session");
        }
        let stale_rows = db
            .sessions()
            .load_active_session_agent_models()
            .await
            .expect("failed to load active sessions");
        for (session_id, terminal_status) in race_cases {
            db.sessions()
                .update_session_status_with_timing_at(session_id, terminal_status, 1)
                .await
                .expect("failed to persist terminal transition");
        }

        // Act
        for row in stale_rows {
            let stale_status = row
                .status
                .parse::<Status>()
                .expect("active status should parse");
            migrate_session_off_retired_model(&db, &row.id, &row.agent, &row.model, stale_status)
                .await;
        }

        // Assert
        for (session_id, terminal_status) in race_cases {
            let row = db
                .sessions()
                .load_session(session_id)
                .await
                .expect("failed to load transitioned session")
                .expect("missing transitioned session");
            assert_eq!(row.status, terminal_status);
            assert_eq!(row.agent, "claude");
            assert_eq!(row.model, "claude-opus-4-6");
        }
    }

    /// Ensures startup remains usable when active-session migration cannot
    /// query a degraded database.
    #[tokio::test]
    async fn test_migrate_active_sessions_off_retired_models_ignores_query_failures() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        sqlx::query("DROP TABLE session")
            .execute(&pool)
            .await
            .expect("session table should be dropped");

        // Act
        migrate_active_sessions_off_retired_models(&db).await;

        // Assert
        assert!(
            db.sessions()
                .load_active_session_agent_models()
                .await
                .is_err()
        );
    }

    /// Ensures finished sessions keep their retired model id in the database
    /// as history while loading with the replacement model in memory.
    #[tokio::test]
    async fn test_load_sessions_keeps_retired_model_in_db_for_finished_session() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let session_id = "retired-finished-session";
        db.sessions()
            .insert_session(session_id, "claude-opus-4-6", "main", "Done", project_id)
            .await
            .expect("failed to insert session");
        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client = create_folder_lookup_mock(Vec::new());
        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.agent.model(), AgentModel::ClaudeOpus5);
        let row = db
            .sessions()
            .load_session(session_id)
            .await
            .expect("failed to load session row")
            .expect("missing session row");
        assert_eq!(row.model, "claude-opus-4-6");
    }

    /// Ensures persisted review-request metadata is mapped onto loaded session
    /// snapshots.
    #[tokio::test]
    async fn test_load_sessions_maps_review_request_metadata() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        let review_request = ReviewRequest {
            last_refreshed_at: 999,
            summary: ReviewRequestSummary {
                display_id: "#17".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Closed,
                status_summary: Some("closed by maintainer".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/team/project/pull/17".to_string(),
            },
        };

        let session_id = "test-session";
        db.sessions()
            .insert_session(session_id, "gemini-3.8-flash", "main", "Done", project_id)
            .await
            .expect("failed to insert session");
        db.reviews()
            .update_session_review_request(session_id, Some(review_request.clone()))
            .await
            .expect("failed to persist review request metadata");

        let base_path = Path::new("/virtual/session-base");
        let mock_fs_client = create_folder_lookup_mock(Vec::new());
        let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

        // Act
        let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: base_path,
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing reloaded session");
        assert_eq!(session.review_request, Some(review_request));
    }

    #[test]
    /// Verifies read-only and terminal DB statuses override stale in-memory
    /// handle statuses.
    fn merge_loaded_session_status_prefers_read_only_and_terminal_status_from_db() {
        // Arrange
        let status_from_handle = Status::Draft;

        // Act
        let merged_status = merge_loaded_session_status(Status::Merged, status_from_handle);
        let done_status = merge_loaded_session_status(Status::Done, status_from_handle);

        // Assert
        assert_eq!(merged_status, Status::Merged);
        assert_eq!(done_status, Status::Done);
    }

    #[test]
    /// Verifies non-terminal DB statuses do not overwrite in-memory status.
    fn merge_loaded_session_status_prefers_handle_for_non_terminal_db_status() {
        // Arrange
        let status_from_db = Status::Review;
        let status_from_handle = Status::InProgress;

        // Act
        let merged_status = merge_loaded_session_status(status_from_db, status_from_handle);

        // Assert
        assert_eq!(merged_status, Status::InProgress);
    }

    #[test]
    /// Verifies loaded message rows do not replace an existing live
    /// transcript snapshot.
    fn sync_handle_transcript_with_loaded_keeps_existing_live_transcript() {
        // Arrange
        let live_transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "answer"),
        ]);
        let handles = SessionHandles::new_with_transcript(Status::Review, live_transcript.clone());
        let loaded_transcript = assistant_transcript("loaded answer");

        // Act
        let transcript = sync_handle_transcript_with_loaded(&handles, Some(&loaded_transcript));

        // Assert
        assert_eq!(transcript, Some(live_transcript.clone()));
        assert_eq!(
            handles.transcript.lock().ok().as_deref(),
            Some(&live_transcript)
        );
    }

    #[test]
    /// Verifies persisted history merges with a partial workflow notice
    /// appended before a lazy transcript was hydrated.
    fn sync_handle_transcript_with_loaded_merges_partial_unloaded_transcript() {
        // Arrange
        let handles = SessionHandles::new_unloaded(Status::Review);
        handles
            .transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .clone_from(&SessionTranscript::new(vec![SessionMessage::new(
                2,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced onto main\n",
            )]));
        let loaded_transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
        ]);
        let expected_transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
            SessionMessage::new(
                2,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced onto main\n",
            ),
        ]);

        // Act
        let transcript = sync_handle_transcript_with_loaded(&handles, Some(&loaded_transcript));

        // Assert
        assert_eq!(transcript, Some(expected_transcript.clone()));
        assert_eq!(
            handles.transcript.lock().ok().as_deref(),
            Some(&expected_transcript)
        );
    }

    #[test]
    /// Verifies hydration deduplicates a persisted live append while retaining
    /// an unpersisted message whose temporary position conflicts.
    fn sync_handle_transcript_with_loaded_merges_matching_and_conflicting_messages() {
        // Arrange
        let handles = SessionHandles::new_unloaded(Status::Review);
        let persisted_notice = SessionMessage::new(
            2,
            SessionMessageKind::WorkflowNotice,
            "\n[Sync] Successfully synced onto main\n",
        );
        handles
            .transcript
            .lock()
            .expect("transcript lock should not be poisoned")
            .clone_from(&SessionTranscript::new(vec![
                SessionMessage::new(
                    0,
                    SessionMessageKind::WorkflowNotice,
                    "\n[Sync Error] persistence failed\n",
                ),
                persisted_notice.clone(),
            ]));
        let loaded_transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
            persisted_notice.clone(),
        ]);

        // Act
        let transcript = sync_handle_transcript_with_loaded(&handles, Some(&loaded_transcript))
            .expect("merged transcript should be available");

        // Assert
        assert_eq!(
            transcript.messages(),
            &[
                SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
                SessionMessage::conversation(
                    1,
                    SessionMessageKind::AssistantAnswer,
                    "original answer"
                ),
                persisted_notice,
                SessionMessage::new(
                    3,
                    SessionMessageKind::WorkflowNotice,
                    "\n[Sync Error] persistence failed\n"
                ),
            ]
        );
    }

    #[test]
    /// Verifies missing-folder rows stay visible while merge cleanup has
    /// removed the worktree before `Done` persistence finishes.
    fn should_skip_missing_folder_session_keeps_live_merging_session() {
        // Arrange
        let has_session_folder = false;
        let persisted_status = Status::Merging;
        let live_handle_status = Some(Status::Merging);

        // Act
        let should_skip = should_skip_missing_folder_session(
            has_session_folder,
            false,
            persisted_status,
            live_handle_status,
        );

        // Assert
        assert!(!should_skip);
    }

    #[test]
    /// Verifies missing-folder rows stay visible when either persistence or
    /// live state has already recorded a remote merge.
    fn should_skip_missing_folder_session_keeps_merged_session() {
        // Arrange, Act
        let persisted_merged_should_skip =
            should_skip_missing_folder_session(false, false, Status::Merged, Some(Status::Review));
        let live_merged_should_skip =
            should_skip_missing_folder_session(false, false, Status::Review, Some(Status::Merged));

        // Assert
        assert!(!persisted_merged_should_skip);
        assert!(!live_merged_should_skip);
    }

    #[test]
    /// Verifies missing-folder non-terminal rows are still filtered when no
    /// merge-cleanup transition is active.
    fn should_skip_missing_folder_session_skips_orphaned_active_session() {
        // Arrange
        let has_session_folder = false;
        let persisted_status = Status::Review;
        let live_handle_status = None;

        // Act
        let should_skip = should_skip_missing_folder_session(
            has_session_folder,
            false,
            persisted_status,
            live_handle_status,
        );

        // Assert
        assert!(should_skip);
    }

    #[test]
    /// Verifies missing-folder draft sessions stay visible before their
    /// deferred worktree is created.
    fn should_skip_missing_folder_session_keeps_new_draft_session() {
        // Arrange
        let has_session_folder = false;
        let persisted_status = Status::Draft;
        let live_handle_status = None;

        // Act
        let should_skip = should_skip_missing_folder_session(
            has_session_folder,
            true,
            persisted_status,
            live_handle_status,
        );

        // Assert
        assert!(!should_skip);
    }

    #[test]
    /// Verifies invalid review-request rows are ignored during session load.
    fn parse_review_request_returns_none_for_invalid_row() {
        // Arrange
        let row = SessionListRow {
            added_lines: 0,
            agent: "codex".to_string(),
            base_branch: "main".to_string(),
            created_at: 0,
            deleted_lines: 0,
            has_diff: Some(false),
            id: "session-a".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            input_tokens: 0,
            is_draft: false,
            model: "gpt-5.6-sol".to_string(),
            output_tokens: 0,
            parent_session_id: None,
            permission_mode: "auto_edit".to_string(),
            personality_id: None,
            project_id: Some(1),
            reasoning_level_override: None,
            response_style: "balanced".to_string(),
            published_upstream_ref: None,
            review_request: Some(SessionReviewRequestRow {
                display_id: "#42".to_string(),
                forge_kind: "UnknownForge".to_string(),
                last_refreshed_at: 0,
                source_branch: "feature/forge".to_string(),
                state: "Open".to_string(),
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            }),
            role: None,
            size: "XS".to_string(),
            speed_mode: "normal".to_string(),
            status: "Review".to_string(),
            title: None,
            updated_at: 0,
        };

        // Act
        let review_request = parse_review_request(&row);

        // Assert
        assert_eq!(review_request, None);
    }

    #[test]
    fn test_parse_questions_json_new_format() {
        // Arrange
        let json = r#"[{"text":"Pick one?","options":["A","B"]}]"#;

        // Act
        let result = parse_questions_json(json);

        // Assert
        let items = result.expect("expected Some");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Pick one?");
        assert_eq!(items[0].options, vec!["A", "B"]);
    }

    #[test]
    fn test_parse_questions_json_legacy_format() {
        // Arrange
        let json = r#"["Need target?","Need tests?"]"#;

        // Act
        let result = parse_questions_json(json);

        // Assert
        let items = result.expect("expected Some");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "Need target?");
        assert_eq!(items[0].options, [] as [std::string::String; 0]);
        assert_eq!(items[1].text, "Need tests?");
        assert_eq!(items[1].options, [] as [std::string::String; 0]);
    }

    #[test]
    fn test_parse_questions_json_empty_string_returns_none() {
        // Arrange / Act
        let result = parse_questions_json("");

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_questions_json_invalid_json_returns_none() {
        // Arrange / Act
        let result = parse_questions_json("{not valid json");

        // Assert
        assert!(result.is_none());
    }
}
