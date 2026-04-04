//! Session lifecycle workflows and direct user actions.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ag_forge as forge;
use askama::Template;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::worker::SessionCommand;
use super::{
    SessionTaskService, draft, session_branch, session_folder, unix_timestamp_from_system_time,
};
use crate::app::session::SessionError;
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager, agentty_home, setting};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{ReviewRequest, SESSION_DATA_DIR, Session, Status};
use crate::domain::setting::SettingName;
use crate::infra::channel::{AgentRequestKind, TurnPrompt, TurnPromptAttachment};
use crate::infra::fs::FsClient;
use crate::infra::{agent, db, git};
use crate::ui::page::session_list::grouped_session_indexes;

/// Maximum stored length for generated session titles.
const GENERATED_SESSION_TITLE_MAX_CHARACTERS: usize = 72;
const USER_PROMPT_PREFIX: &str = " › ";
const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";

/// Input bag for constructing a queued session command.
struct BuildSessionCommandInput {
    is_first_message: bool,
    prompt: TurnPrompt,
    session_model: AgentModel,
    session_output: Option<String>,
}

/// Intermediate values captured while preparing a session reply.
type ReplyContext = (Option<String>, bool, String, Option<String>);

/// Cleanup payload for a deleted session's git and filesystem resources.
struct DeletedSessionCleanup {
    branch_name: String,
    folder: PathBuf,
    has_git_branch: bool,
    session_id: String,
    working_dir: PathBuf,
}

/// Askama view model for rendering one-shot title-generation prompts.
#[derive(Template)]
#[template(path = "session_title_generation_prompt.md", escape = "none")]
struct SessionTitleGenerationPromptTemplate<'a> {
    prompt: &'a str,
}

impl SessionManager {
    /// Moves selection to the next selectable session in grouped list order.
    ///
    /// Group header rows are non-selectable and are skipped by design.
    pub fn next(&mut self) {
        let grouped_indexes = grouped_session_indexes(&self.state.sessions);
        if grouped_indexes.is_empty() {
            return;
        }

        let index = match self
            .state
            .table_state
            .selected()
            .and_then(|selected_index| {
                grouped_indexes
                    .iter()
                    .position(|session_index| *session_index == selected_index)
            }) {
            Some(position) => {
                if position >= grouped_indexes.len() - 1 {
                    0
                } else {
                    position + 1
                }
            }
            None => 0,
        };

        self.state.table_state.select(Some(grouped_indexes[index]));
    }

    /// Moves selection to the previous selectable session in grouped list
    /// order.
    ///
    /// Group header rows are non-selectable and are skipped by design.
    pub fn previous(&mut self) {
        let grouped_indexes = grouped_session_indexes(&self.state.sessions);
        if grouped_indexes.is_empty() {
            return;
        }

        let index = match self
            .state
            .table_state
            .selected()
            .and_then(|selected_index| {
                grouped_indexes
                    .iter()
                    .position(|session_index| *session_index == selected_index)
            }) {
            Some(position) => {
                if position == 0 {
                    grouped_indexes.len() - 1
                } else {
                    position - 1
                }
            }
            None => 0,
        };

        self.state.table_state.select(Some(grouped_indexes[index]));
    }

    /// Creates a blank session with an empty prompt and output.
    ///
    /// Returns the identifier of the newly created session.
    /// The session is created with `New` status and no agent is started —
    /// call [`SessionManager::start_session`] to submit a prompt and launch
    /// the agent.
    ///
    /// # Errors
    /// Returns an error if the worktree, session files, database record, or
    /// backend setup cannot be created.
    pub async fn create_session(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<String, SessionError> {
        self.create_session_with_draft_mode(projects, services, false)
            .await
    }

    /// Creates a blank draft session that stages prompts until explicitly
    /// started.
    ///
    /// Returns the identifier of the newly created session.
    ///
    /// # Errors
    /// Returns an error if the worktree, session files, database record, or
    /// backend setup cannot be created.
    pub async fn create_draft_session(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<String, SessionError> {
        self.create_session_with_draft_mode(projects, services, true)
            .await
    }

    /// Creates one blank session using either immediate-start or explicit
    /// staged-draft first-turn behavior.
    ///
    /// # Errors
    /// Returns an error if the worktree, session files, database record, or
    /// backend setup cannot be created.
    async fn create_session_with_draft_mode(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
        is_draft: bool,
    ) -> Result<String, SessionError> {
        let base_branch = projects.git_branch().ok_or_else(|| {
            SessionError::Workflow("Git branch is required to create a session".to_string())
        })?;
        let session_model = self
            .resolve_default_session_model(services, projects.active_project_id())
            .await;
        self.default_session_model = session_model;

        let session_id = Uuid::new_v4().to_string();
        let folder = session_folder(services.base_path(), &session_id);
        if folder.exists() {
            return Err(SessionError::Workflow(format!(
                "Session folder {session_id} already exists"
            )));
        }

        let worktree_branch = session_branch(&session_id);
        let working_dir = projects.working_dir().to_path_buf();
        let git_client = services.git_client();
        let repo_root = git_client
            .find_git_repo_root(working_dir)
            .await
            .ok_or_else(|| {
                SessionError::Workflow("Failed to find git repository root".to_string())
            })?;

        {
            let folder = folder.clone();
            let repo_root = repo_root.clone();
            let worktree_branch = worktree_branch.clone();
            let base_branch = base_branch.to_string();
            git_client
                .create_worktree(repo_root, folder, worktree_branch, base_branch)
                .await
                .map_err(|error| {
                    SessionError::Workflow(format!("Failed to create git worktree: {error}"))
                })?;
        }

        let data_dir = folder.join(SESSION_DATA_DIR);
        if let Err(error) = services.fs_client().create_dir_all(data_dir).await {
            self.rollback_failed_session_creation(
                services,
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(SessionError::Workflow(format!(
                "Failed to create session metadata directory: {error}"
            )));
        }

        let insert_result = if is_draft {
            services
                .db()
                .insert_draft_session(
                    &session_id,
                    session_model.as_str(),
                    base_branch,
                    &Status::New.to_string(),
                    projects.active_project_id(),
                )
                .await
        } else {
            services
                .db()
                .insert_session(
                    &session_id,
                    session_model.as_str(),
                    base_branch,
                    &Status::New.to_string(),
                    projects.active_project_id(),
                )
                .await
        };

        if let Err(error) = insert_result {
            self.rollback_failed_session_creation(
                services,
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(SessionError::Workflow(format!(
                "Failed to save session metadata: {error}"
            )));
        }

        // Best-effort: activity tracking is non-critical.
        let _ = services
            .db()
            .insert_session_creation_activity_now(&session_id)
            .await;

        if let Err(error) = agent::create_backend(session_model.kind()).setup(&folder) {
            self.rollback_failed_session_creation(
                services,
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                true,
            )
            .await;

            return Err(SessionError::Workflow(format!(
                "Failed to setup session backend: {error}"
            )));
        }
        services.emit_app_event(AppEvent::RefreshSessions);

        Ok(session_id)
    }

    /// Appends one staged draft message to a `New` session without launching
    /// the agent yet.
    ///
    /// # Errors
    /// Returns an error if the session is missing, was not created as a draft
    /// session, is no longer `New`, or the staged bundle cannot be persisted.
    pub async fn stage_draft_message(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), SessionError> {
        let prompt = prompt.into();
        let session_index = self.session_index_or_err(session_id)?;
        let (persisted_session_id, title_to_save, staged_prompt, staged_attachments) = {
            let session = self
                .sessions
                .get(session_index)
                .ok_or(SessionError::NotFound)?;
            if !session.is_draft_session() {
                return Err(SessionError::Workflow(
                    "Only draft sessions can stage drafts".to_string(),
                ));
            }
            if session.status != Status::New {
                return Err(SessionError::Workflow(
                    "Only `New` sessions can stage drafts".to_string(),
                ));
            }

            let title_to_save = if session.title.is_none() {
                Some(prompt.transcript_text())
            } else {
                None
            };
            let next_attachment_number = session.draft_attachments.len().saturating_add(1);
            let staged_prompt =
                Self::append_staged_prompt(&session.prompt, &prompt, next_attachment_number);
            let mut staged_attachments = session.draft_attachments.clone();
            staged_attachments.extend(Self::renumbered_attachments(
                &prompt,
                next_attachment_number,
            ));

            (
                session.id.clone(),
                title_to_save,
                staged_prompt,
                staged_attachments,
            )
        };

        draft::store_staged_draft_attachments(
            services.fs_client().as_ref(),
            services.base_path(),
            &persisted_session_id,
            &staged_attachments,
        )
        .await?;
        services
            .db()
            .update_session_prompt(&persisted_session_id, &staged_prompt)
            .await?;

        if let Some(title_to_save) = title_to_save.as_deref() {
            services
                .db()
                .update_session_title(&persisted_session_id, title_to_save)
                .await?;
        }

        if let Some(session) = self.sessions.get_mut(session_index) {
            if let Some(title_to_save) = title_to_save {
                session.title = Some(title_to_save);
            }

            session.prompt = staged_prompt;
            session.draft_attachments = staged_attachments;
        }

        Ok(())
    }

    /// Starts a `New` session from its persisted staged draft bundle.
    ///
    /// # Errors
    /// Returns an error if the session is missing, is not a draft session, no
    /// drafts are staged, or launching the first turn fails.
    pub async fn start_staged_session(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let prompt = {
            let session = self.session_or_err(session_id)?;
            if !session.is_draft_session() {
                return Err(SessionError::Workflow(
                    "Only draft sessions can be started from staged drafts".to_string(),
                ));
            }
            if session.status != Status::New {
                return Err(SessionError::Workflow(
                    "Only `New` sessions can be started from staged drafts".to_string(),
                ));
            }
            if session.prompt.is_empty() {
                return Err(SessionError::Workflow(
                    "Stage at least one draft before starting the session".to_string(),
                ));
            }

            TurnPrompt {
                attachments: session.draft_attachments.clone(),
                text: session.prompt.clone(),
            }
        };

        self.start_session(services, session_id, prompt).await?;

        if let Ok(session_index) = self.session_index_or_err(session_id)
            && let Some(session) = self.sessions.get_mut(session_index)
        {
            session.draft_attachments.clear();
        }

        // Best-effort: the live session has already started successfully.
        let _ = draft::store_staged_draft_attachments(
            services.fs_client().as_ref(),
            services.base_path(),
            session_id,
            &[],
        )
        .await;

        Ok(())
    }

    /// Submits the first prompt for a blank session and starts the agent.
    ///
    /// The first prompt is persisted as both session prompt and session title.
    /// A detached one-shot title-generation task from the start-turn worker
    /// may replace that initial title once.
    ///
    /// # Errors
    /// Returns an error if the session is missing or prompt persistence fails.
    pub async fn start_session(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), SessionError> {
        let prompt = prompt.into();
        let session_index = self.session_index_or_err(session_id)?;
        let (persisted_session_id, session_model, title) = {
            let session = self
                .sessions
                .get_mut(session_index)
                .ok_or(SessionError::NotFound)?;

            session.prompt.clone_from(&prompt.text);

            let title = prompt.text.clone();
            session.title = Some(title.clone());
            let session_model = session.model;

            (session.id.clone(), session_model, title)
        };

        let handles = self.session_handles_or_err(&persisted_session_id)?;
        let output = Arc::clone(&handles.output);
        let status = Arc::clone(&handles.status);
        let app_event_tx = services.event_sender();

        self.persist_first_message_metadata(services, &persisted_session_id, &prompt.text, &title)
            .await;

        let initial_output = Self::formatted_prompt_output(&prompt, false);
        SessionTaskService::append_session_output(
            &output,
            services.db(),
            &app_event_tx,
            &persisted_session_id,
            &initial_output,
        )
        .await;
        self.set_active_prompt_output(&persisted_session_id, initial_output);

        // Best-effort: status transition failure is non-critical.
        let _ = SessionTaskService::update_status(
            &status,
            services.clock().as_ref(),
            services.db(),
            &app_event_tx,
            &persisted_session_id,
            Status::InProgress,
        )
        .await;

        let operation_id = Uuid::new_v4().to_string();
        let command = SessionCommand::Run {
            operation_id,
            request_kind: AgentRequestKind::SessionStart,
            prompt: prompt.clone(),
            session_model,
        };
        if let Err(error) = self
            .enqueue_session_command(services, &persisted_session_id, command)
            .await
        {
            self.cleanup_prompt_attachment_files(services, &prompt)
                .await;

            return Err(error);
        }

        Ok(())
    }

    /// Submits a follow-up prompt to an existing session.
    pub async fn reply(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) {
        let prompt = prompt.into();
        let Ok(session) = self.session_or_err(session_id) else {
            return;
        };
        let session_model = session.model;
        self.reply_impl(services, session_id, prompt, session_model)
            .await;
    }

    /// Updates and persists the model for a single session.
    ///
    /// When `LastUsedModelAsDefault` is enabled, this also persists the chosen
    /// session model as `DefaultSmartModel`.
    ///
    /// When the model changes, this also clears any persisted provider-native
    /// conversation identifier so incompatible runtimes do not attempt resume
    /// with stale ids, and drops the existing session worker so the next turn
    /// creates a fresh worker with the correct [`AgentChannel`] type.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub async fn set_session_model(
        &mut self,
        services: &AppServices,
        session_id: &str,
        session_model: AgentModel,
    ) -> Result<(), SessionError> {
        let session_index = self.session_index_or_err(session_id)?;
        let model_changed = self
            .sessions
            .get(session_index)
            .is_some_and(|session| session.model != session_model);

        services
            .db()
            .update_session_model(session_id, session_model.as_str())
            .await?;
        if model_changed {
            services
                .db()
                .update_session_provider_conversation_id(session_id, None)
                .await?;
            services
                .db()
                .update_session_instruction_conversation_id(session_id, None)
                .await?;

            self.clear_session_worker(session_id);
        }

        let session_project_id = services.db().load_session_project_id(session_id).await?;

        if Self::should_persist_last_used_model_as_default(services, session_project_id).await?
            && let Some(project_id) = session_project_id
        {
            services
                .db()
                .upsert_project_setting(
                    project_id,
                    SettingName::DefaultSmartModel,
                    session_model.as_str(),
                )
                .await?;
        }

        services.emit_app_event(AppEvent::SessionModelUpdated {
            session_id: session_id.to_string(),
            session_model,
        });

        if model_changed {
            self.mark_history_replay_pending(session_id);
        }

        Ok(())
    }

    /// Returns whether session model switches should also persist
    /// `DefaultSmartModel`.
    async fn should_persist_last_used_model_as_default(
        services: &AppServices,
        project_id: Option<i64>,
    ) -> Result<bool, SessionError> {
        let Some(project_id) = project_id else {
            return Ok(false);
        };

        let should_persist = services
            .db()
            .get_project_setting(project_id, SettingName::LastUsedModelAsDefault)
            .await?
            .and_then(|setting_value| setting_value.parse::<bool>().ok())
            .unwrap_or(false);

        Ok(should_persist)
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.state
            .table_state
            .selected()
            .and_then(|index| self.state.sessions.get(index))
    }

    /// Returns the session identifier for the given list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<String> {
        self.state
            .sessions
            .get(session_index)
            .map(|session| session.id.clone())
    }

    /// Resolves a stable session identifier to the current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
    }

    /// Publishes a review-ready session branch and links one forge review
    /// request.
    ///
    /// Stored links are reused without pushing or creating duplicates. `Done`
    /// and `Canceled` sessions keep that stored link for later open and
    /// refresh flows, but creating a first link is limited to `Review`
    /// sessions because terminal sessions may no longer have a live worktree
    /// branch.
    ///
    /// # Errors
    /// Returns an error if the session is missing, not eligible for first-time
    /// publication, git push fails, forge detection fails, or persistence
    /// fails.
    pub async fn publish_review_request(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<ReviewRequest, SessionError> {
        let session_index = self.session_index_or_err(session_id)?;
        let Some(session) = self.state.sessions.get(session_index) else {
            return Err(SessionError::NotFound);
        };

        if let Some(review_request) = session.review_request.clone() {
            return Ok(review_request);
        }

        if !session.status.allows_review_actions() {
            return Err(SessionError::Workflow(
                "Session must be in review to create a review request".to_string(),
            ));
        }

        let folder = session.folder.clone();
        let source_branch = session_branch(session_id);
        let create_input = Self::review_request_create_input(session, source_branch.clone());
        let git_client = services.git_client();
        let published_upstream_ref = git_client
            .push_current_branch(folder.clone())
            .await
            .map_err(|error| {
                SessionError::Workflow(format!("Failed to publish session branch: {error}"))
            })?;
        self.store_published_upstream_ref(services, session_id, published_upstream_ref)
            .await?;

        let repo_url = git_client.repo_url(folder).await.map_err(|error| {
            SessionError::Workflow(format!(
                "Failed to resolve repository remote for review request: {error}"
            ))
        })?;
        let review_request_client = services.review_request_client();
        let remote = review_request_client
            .detect_remote(repo_url)
            .map_err(|error| SessionError::Workflow(error.detail_message()))?;
        let review_request_summary = match review_request_client
            .find_by_source_branch(remote.clone(), source_branch)
            .await
            .map_err(|error| SessionError::Workflow(error.detail_message()))?
        {
            Some(existing_review_request) => existing_review_request,
            None => review_request_client
                .create_review_request(remote, create_input)
                .await
                .map_err(|error| SessionError::Workflow(error.detail_message()))?,
        };
        self.store_review_request_summary(services, session_id, review_request_summary)
            .await
    }

    /// Returns the browser-openable URL for one linked review request.
    ///
    /// # Errors
    /// Returns an error if the session is missing, has no linked review
    /// request, or the stored summary is missing a usable web URL.
    pub fn review_request_web_url(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<String, SessionError> {
        let session = self.session_or_err(session_id)?;
        let review_request = session.review_request.as_ref().ok_or_else(|| {
            SessionError::Workflow("Session has no linked review request".to_string())
        })?;

        services
            .review_request_client()
            .review_request_web_url(&review_request.summary)
            .map_err(|error| SessionError::Workflow(error.detail_message()))
    }

    /// Deletes the currently selected session and cleans related resources.
    ///
    /// After persistence and filesystem cleanup, this triggers a full list
    /// reload through [`AppEvent::RefreshSessions`].
    pub async fn delete_selected_session(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        let Some(cleanup) = self
            .remove_selected_session_from_state_and_db(projects, services)
            .await
        else {
            return;
        };

        Self::cleanup_deleted_session_resources(
            services.fs_client(),
            services.git_client(),
            cleanup,
        )
        .await;
    }

    /// Deletes the selected session while deferring filesystem cleanup to a
    /// background task.
    pub async fn delete_selected_session_deferred_cleanup(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        let Some(cleanup) = self
            .remove_selected_session_from_state_and_db(projects, services)
            .await
        else {
            return;
        };

        let fs_client = services.fs_client();
        let git_client = services.git_client();
        tokio::spawn(async move {
            SessionManager::cleanup_deleted_session_resources(fs_client, git_client, cleanup).await;
        });
    }

    /// Removes the selected session from app state and persistence, returning
    /// deferred cleanup instructions for git and filesystem resources.
    async fn remove_selected_session_from_state_and_db(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Option<DeletedSessionCleanup> {
        let selected_index = self.state.table_state.selected()?;
        if selected_index >= self.state.sessions.len() {
            return None;
        }

        let session = self.state.sessions.remove(selected_index);
        self.state.handles.remove(&session.id);
        self.clear_history_replay_pending(&session.id);

        // Best-effort: cancellation failure is non-critical during session deletion.
        let _ = services
            .db()
            .request_cancel_for_session_operations(&session.id)
            .await;
        self.clear_session_worker(&session.id);
        // Best-effort: session record removal failure is non-critical during session
        // deletion.
        let _ = services.db().delete_session(&session.id).await;
        services.emit_app_event(AppEvent::RefreshSessions);

        Some(DeletedSessionCleanup {
            branch_name: session_branch(&session.id),
            folder: session.folder,
            has_git_branch: projects.has_git_branch(),
            session_id: session.id,
            working_dir: projects.working_dir().to_path_buf(),
        })
    }

    /// Deletes worktree resources for a previously removed session.
    async fn cleanup_deleted_session_resources(
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn git::GitClient>,
        cleanup: DeletedSessionCleanup,
    ) {
        let repo_root = if cleanup.has_git_branch {
            git_client.find_git_repo_root(cleanup.working_dir).await
        } else {
            None
        };

        Self::cleanup_session_worktree_resources(
            fs_client.clone(),
            git_client,
            cleanup.folder,
            cleanup.branch_name,
            repo_root,
            cleanup.has_git_branch,
        )
        .await;
        Self::cleanup_session_temp_directory(fs_client, &cleanup.session_id).await;
    }

    /// Builds one normalized create-request payload from session metadata.
    fn review_request_create_input(
        session: &Session,
        source_branch: String,
    ) -> forge::CreateReviewRequestInput {
        forge::CreateReviewRequestInput {
            body: Self::review_request_body(session),
            source_branch,
            target_branch: session.base_branch.clone(),
            title: Self::review_request_title(session),
        }
    }

    /// Returns the title used for a newly created review request.
    fn review_request_title(session: &Session) -> String {
        session
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let prompt = session.prompt.trim();

                (!prompt.is_empty()).then(|| prompt.to_string())
            })
            .unwrap_or_else(|| "Agentty review request".to_string())
    }

    /// Returns optional body copy for one created review request.
    fn review_request_body(session: &Session) -> Option<String> {
        session
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .map(str::to_string)
    }

    /// Converts one refreshed summary into persisted review-request metadata.
    pub(super) fn build_review_request(
        &self,
        summary: forge::ReviewRequestSummary,
    ) -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: unix_timestamp_from_system_time(self.state.clock.now_system_time()),
            summary,
        }
    }

    /// Persists one normalized review-request summary for a session.
    ///
    /// # Errors
    /// Returns an error if the session disappears or persistence fails.
    pub(crate) async fn store_review_request_summary(
        &mut self,
        services: &AppServices,
        session_id: &str,
        summary: forge::ReviewRequestSummary,
    ) -> Result<ReviewRequest, SessionError> {
        let session_index = self.session_index_or_err(session_id)?;
        let review_request = self.build_review_request(summary);

        self.store_review_request(services, session_index, review_request)
            .await
    }

    /// Persists one linked review request in memory and the database.
    ///
    /// # Errors
    /// Returns an error if the session disappears or persistence fails.
    pub(super) async fn store_review_request(
        &mut self,
        services: &AppServices,
        session_index: usize,
        review_request: ReviewRequest,
    ) -> Result<ReviewRequest, SessionError> {
        let session_id = self
            .state
            .sessions
            .get(session_index)
            .map(|session| session.id.clone())
            .ok_or(SessionError::NotFound)?;
        services
            .db()
            .update_session_review_request(&session_id, Some(&review_request))
            .await?;

        let Some(session) = self.state.sessions.get_mut(session_index) else {
            return Err(SessionError::NotFound);
        };
        session.review_request = Some(review_request.clone());

        Ok(review_request)
    }

    /// Persists one published upstream reference in memory and the database.
    ///
    /// # Errors
    /// Returns an error if the session disappears or persistence fails.
    pub(super) async fn store_published_upstream_ref(
        &mut self,
        services: &AppServices,
        session_id: &str,
        published_upstream_ref: String,
    ) -> Result<(), SessionError> {
        services
            .db()
            .update_session_published_upstream_ref(session_id, Some(&published_upstream_ref))
            .await?;

        let session_index = self.session_index_or_err(session_id)?;
        let Some(session) = self.state.sessions.get_mut(session_index) else {
            return Err(SessionError::NotFound);
        };
        session.published_upstream_ref = Some(published_upstream_ref);

        Ok(())
    }

    /// Validates and queues a follow-up prompt for an existing session.
    ///
    /// Gathers reply context, appends the prompt line to session output, builds
    /// a [`SessionCommand::Run`] with the appropriate [`AgentRequestKind`],
    /// and enqueues it on the session worker.
    async fn reply_impl(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: TurnPrompt,
        session_model: AgentModel,
    ) {
        let Ok(session_index) = self.session_index_or_err(session_id) else {
            return;
        };
        let should_replay_history = self.should_replay_history(session_id);
        let (session_output, is_first_message, persisted_session_id, title_to_save) = match self
            .prepare_reply_context(session_index, &prompt, session_model, should_replay_history)
        {
            Ok(Some(reply_context)) => reply_context,
            Ok(None) => return,
            Err(error) => {
                self.append_reply_status_error(services, session_id, &error)
                    .await;

                return;
            }
        };

        if should_replay_history {
            self.clear_history_replay_pending(&persisted_session_id);
        }

        let app_event_tx = services.event_sender();

        let Ok(handles) = self.session_handles_or_err(&persisted_session_id) else {
            return;
        };

        let output = Arc::clone(&handles.output);
        let status = Arc::clone(&handles.status);

        let effective_prompt = prompt;

        if let Some(title) = title_to_save {
            self.persist_first_message_metadata(
                services,
                &persisted_session_id,
                &effective_prompt.text,
                &title,
            )
            .await;

            // Best-effort: status transition failure is non-critical.
            let _ = SessionTaskService::update_status(
                &status,
                services.clock().as_ref(),
                services.db(),
                &app_event_tx,
                &persisted_session_id,
                Status::InProgress,
            )
            .await;
        }

        self.append_reply_prompt_line(
            services,
            &output,
            &app_event_tx,
            &persisted_session_id,
            &effective_prompt,
        )
        .await;

        let command = Self::build_session_command(BuildSessionCommandInput {
            is_first_message,
            prompt: effective_prompt.clone(),
            session_model,
            session_output,
        });
        self.enqueue_reply_command(
            services,
            &output,
            &app_event_tx,
            &persisted_session_id,
            &effective_prompt,
            command,
        )
        .await;
    }

    /// Validates reply eligibility and gathers per-session values needed for
    /// queueing a reply command.
    ///
    /// # Errors
    /// Returns a [`SessionError::Workflow`] when session status does not allow
    /// replying.
    fn prepare_reply_context(
        &mut self,
        session_index: usize,
        prompt: &TurnPrompt,
        session_model: AgentModel,
        should_replay_history: bool,
    ) -> Result<Option<ReplyContext>, SessionError> {
        let Some(session) = self.state.sessions.get_mut(session_index) else {
            return Ok(None);
        };

        let is_first_message = session.prompt.is_empty();
        let allowed = session.status.allows_review_actions()
            || session.status == Status::Question
            || (is_first_message && session.status == Status::New);
        if !allowed {
            return Err(SessionError::Workflow(
                "Session must be in review status".to_string(),
            ));
        }

        let mut title_to_save = None;
        if is_first_message {
            session.prompt.clone_from(&prompt.text);
            let title = prompt.text.clone();
            session.title = Some(title.clone());
            title_to_save = Some(title);
        }

        let session_output = if !is_first_message
            && (should_replay_history
                || agent::transport_mode(session_model.kind()).uses_app_server())
        {
            Some(session.output.clone())
        } else {
            None
        };

        Ok(Some((
            session_output,
            is_first_message,
            session.id.clone(),
            title_to_save,
        )))
    }

    /// Persists first-message prompt/title metadata before queueing execution.
    ///
    /// This writes the initial prompt/title.
    ///
    /// Title generation itself is triggered once from the start-turn worker
    /// path as soon as the first turn starts running.
    async fn persist_first_message_metadata(
        &self,
        services: &AppServices,
        session_id: &str,
        prompt: &str,
        title: &str,
    ) {
        // Best-effort: title persistence failure is non-critical.
        let _ = services.db().update_session_title(session_id, title).await;
        // Best-effort: prompt persistence failure is non-critical.
        let _ = services
            .db()
            .update_session_prompt(session_id, prompt)
            .await;
    }

    /// Appends the user reply marker line to session output.
    async fn append_reply_prompt_line(
        &mut self,
        services: &AppServices,
        output: &Arc<Mutex<String>>,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        session_id: &str,
        prompt: &TurnPrompt,
    ) {
        let reply_line = Self::formatted_prompt_output(prompt, true);
        SessionTaskService::append_session_output(
            output,
            services.db(),
            app_event_tx,
            session_id,
            &reply_line,
        )
        .await;
        self.set_active_prompt_output(session_id, reply_line);
    }

    /// Formats one user prompt block for persisted session output.
    ///
    /// The first line uses `USER_PROMPT_PREFIX`; continuation lines use
    /// `USER_PROMPT_CONTINUATION_PREFIX` so embedded blank lines remain inside
    /// the prompt block instead of being interpreted as prompt terminators.
    fn formatted_prompt_output(prompt: &TurnPrompt, prepend_newline: bool) -> String {
        let prompt_text = prompt.transcript_text();
        let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
        let mut formatted_lines = Vec::with_capacity(prompt_lines.len());

        for (index, prompt_line) in prompt_lines.into_iter().enumerate() {
            let prefix = if index == 0 {
                USER_PROMPT_PREFIX
            } else {
                USER_PROMPT_CONTINUATION_PREFIX
            };

            formatted_lines.push(format!("{prefix}{prompt_line}"));
        }

        let prompt_block = formatted_lines.join("\n");
        if prepend_newline {
            return format!("\n{prompt_block}\n\n");
        }

        format!("{prompt_block}\n\n")
    }

    /// Appends one newly staged prompt onto the persisted draft-session
    /// prompt text stored in `session.prompt`.
    ///
    /// Attachment placeholders are renumbered sequentially so draft sessions
    /// can keep one flat prompt string while preserving a stable attachment
    /// order across multiple staging passes.
    fn append_staged_prompt(
        existing_prompt: &str,
        prompt: &TurnPrompt,
        next_attachment_number: usize,
    ) -> String {
        let staged_prompt = Self::renumbered_prompt_text(prompt, next_attachment_number);
        if existing_prompt.is_empty() {
            return staged_prompt;
        }

        format!("{existing_prompt}\n\n{staged_prompt}")
    }

    /// Returns the staged prompt text after renumbering any attachment
    /// placeholders to their global draft-session positions.
    fn renumbered_prompt_text(prompt: &TurnPrompt, next_attachment_number: usize) -> String {
        let mut prompt_text = prompt.text.clone();

        for (offset, attachment) in prompt.attachments.iter().enumerate() {
            let placeholder = format!("[Image #{}]", next_attachment_number.saturating_add(offset));
            prompt_text = replace_first(&prompt_text, &attachment.placeholder, &placeholder);
        }

        prompt_text
    }

    /// Returns the prompt attachments rewritten to the global draft-session
    /// placeholder sequence.
    fn renumbered_attachments(
        prompt: &TurnPrompt,
        next_attachment_number: usize,
    ) -> Vec<TurnPromptAttachment> {
        prompt
            .attachments
            .iter()
            .enumerate()
            .map(|(offset, attachment)| TurnPromptAttachment {
                placeholder: format!("[Image #{}]", next_attachment_number.saturating_add(offset)),
                local_image_path: attachment.local_image_path.clone(),
            })
            .collect()
    }

    /// Builds a queued command for starting or resuming a session interaction.
    ///
    /// Creates a [`SessionCommand::Run`] with
    /// [`AgentRequestKind::SessionStart`] for first messages and
    /// [`AgentRequestKind::SessionResume`] with optional transcript replay
    /// for subsequent replies.
    fn build_session_command(input: BuildSessionCommandInput) -> SessionCommand {
        let BuildSessionCommandInput {
            is_first_message,
            prompt,
            session_model,
            session_output,
        } = input;
        let operation_id = Uuid::new_v4().to_string();
        let request_kind = if is_first_message {
            AgentRequestKind::SessionStart
        } else {
            AgentRequestKind::SessionResume { session_output }
        };

        SessionCommand::Run {
            operation_id,
            request_kind,
            prompt,
            session_model,
        }
    }

    /// Appends a reply-error notice to the session output so the user sees
    /// why the reply was rejected.
    async fn append_reply_status_error(
        &self,
        services: &AppServices,
        session_id: &str,
        error: &SessionError,
    ) {
        let status_error = format!("\n[Reply Error] {error}\n");
        let Ok(handles) = self.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = services.event_sender();

        SessionTaskService::append_session_output(
            &handles.output,
            services.db(),
            &app_event_tx,
            session_id,
            &status_error,
        )
        .await;
    }

    async fn enqueue_reply_command(
        &mut self,
        services: &AppServices,
        output: &Arc<Mutex<String>>,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        persisted_session_id: &str,
        prompt: &TurnPrompt,
        command: SessionCommand,
    ) {
        if let Err(error) = self
            .enqueue_session_command(services, persisted_session_id, command)
            .await
        {
            self.cleanup_prompt_attachment_files(services, prompt).await;

            let error_line = format!("\n[Reply Error] {error}\n");
            SessionTaskService::append_session_output(
                output,
                services.db(),
                app_event_tx,
                persisted_session_id,
                &error_line,
            )
            .await;
        }
    }

    /// Spawns one detached model command that generates a session title once.
    ///
    /// The generated title is persisted when parsing succeeds, then a
    /// `RefreshSessions` event is emitted so list-mode snapshots pick up the
    /// new title.
    pub(crate) fn spawn_session_title_generation_task(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        db: db::Database,
        session_id: &str,
        folder: &Path,
        prompt: &str,
        session_model: AgentModel,
    ) {
        let folder = folder.to_path_buf();
        let prompt = prompt.to_string();
        let persisted_session_id = session_id.to_string();

        tokio::spawn(async move {
            let Ok(title_generation_prompt) =
                SessionManager::session_title_generation_prompt(&prompt)
            else {
                return;
            };

            let Some(title_response) = SessionManager::run_title_generation_command(
                folder.as_path(),
                &title_generation_prompt,
                session_model,
            )
            .await
            else {
                return;
            };

            let Some(generated_title) =
                SessionManager::parse_generated_session_title(&title_response)
            else {
                return;
            };

            if generated_title == prompt {
                return;
            }

            if db
                .update_session_title(&persisted_session_id, &generated_title)
                .await
                .is_ok()
            {
                // Fire-and-forget: receiver may be dropped during shutdown.
                let _ = app_event_tx.send(AppEvent::RefreshSessions);
            }
        });
    }

    /// Executes one detached title-generation command and returns parsed
    /// content.
    async fn run_title_generation_command(
        folder: &Path,
        prompt: &str,
        model: AgentModel,
    ) -> Option<String> {
        let response = agent::submit_one_shot(agent::OneShotRequest {
            child_pid: None,
            folder,
            model,
            prompt,
            request_kind: AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
        })
        .await
        .ok()?;

        Some(response.to_answer_display_text())
    }

    /// Builds the title-generation instruction prompt from the user message.
    ///
    /// # Errors
    /// Returns an error if Askama template rendering fails.
    fn session_title_generation_prompt(prompt: &str) -> Result<String, SessionError> {
        let template = SessionTitleGenerationPromptTemplate { prompt };

        template.render().map_err(|error| {
            SessionError::Workflow(format!(
                "Failed to render `session_title_generation_prompt.md`: {error}"
            ))
        })
    }

    /// Parses model output into a normalized one-line session title.
    ///
    /// Accepts either a plain-text title line or a protocol-wrapped response
    /// (`{"answer":"..."}`) whose first answer line contains the title.
    ///
    /// Returns [`None`] when no usable title line is present.
    fn parse_generated_session_title(content: &str) -> Option<String> {
        let content = content.trim();
        if content.is_empty() {
            return None;
        }

        if let Ok(protocol_response) = agent::protocol::parse_agent_response_strict(content) {
            return Self::parse_generated_session_title_from_protocol_response(&protocol_response);
        }

        let first_line = Self::first_nonempty_line(content)?;

        Self::normalize_generated_session_title(first_line)
    }

    /// Extracts the first usable title candidate from protocol `answer`
    /// content.
    fn parse_generated_session_title_from_protocol_response(
        protocol_response: &agent::protocol::AgentResponse,
    ) -> Option<String> {
        for answer in protocol_response.answers() {
            if let Some(first_line) = Self::first_nonempty_line(&answer)
                && let Some(parsed_title) = Self::normalize_generated_session_title(first_line)
            {
                return Some(parsed_title);
            }
        }

        None
    }

    /// Returns the first non-empty line from model output content.
    fn first_nonempty_line(content: &str) -> Option<&str> {
        content.lines().find_map(|line| {
            let trimmed_line = line.trim();
            if trimmed_line.is_empty() {
                return None;
            }

            Some(trimmed_line)
        })
    }

    /// Normalizes one candidate title and applies storage-safe constraints.
    fn normalize_generated_session_title(candidate: &str) -> Option<String> {
        let mut title = candidate.trim().to_string();
        if let Some((prefix, remainder)) = title.split_once(':')
            && prefix.trim().eq_ignore_ascii_case("title")
        {
            title = remainder.trim().to_string();
        }

        title = title
            .trim_matches(|ch| matches!(ch, '"' | '\'' | '`'))
            .trim()
            .to_string();

        if title.is_empty() {
            return None;
        }

        let truncated = title
            .chars()
            .take(GENERATED_SESSION_TITLE_MAX_CHARACTERS)
            .collect::<String>();

        Some(truncated)
    }

    async fn resolve_default_session_model(
        &self,
        services: &AppServices,
        project_id: i64,
    ) -> AgentModel {
        setting::load_default_smart_model_setting(
            services,
            Some(project_id),
            self.default_session_model,
        )
        .await
    }

    /// Reverts filesystem and database changes after session creation failure.
    async fn rollback_failed_session_creation(
        &self,
        services: &AppServices,
        folder: &Path,
        repo_root: &Path,
        session_id: &str,
        worktree_branch: &str,
        session_saved: bool,
    ) {
        if session_saved {
            // Best-effort cleanup: session record may already be removed.
            let _ = services.db().delete_session(session_id).await;
        }

        {
            let git_client = services.git_client();
            let folder = folder.to_path_buf();
            let repo_root = repo_root.to_path_buf();
            let worktree_branch = worktree_branch.to_string();
            // Best-effort cleanup: worktree may already be removed.
            let _ = git_client.remove_worktree(folder).await;
            // Best-effort cleanup: branch may already be removed.
            let _ = git_client.delete_branch(repo_root, worktree_branch).await;
        }

        // Best-effort cleanup: worktree directory may already be removed.
        let _ = services
            .fs_client()
            .remove_dir_all(folder.to_path_buf())
            .await;
        Self::cleanup_session_temp_directory(services.fs_client(), session_id).await;
    }

    /// Appends text to a specific session output stream.
    pub(crate) async fn append_output_for_session(
        &self,
        services: &AppServices,
        session_id: &str,
        output: &str,
    ) {
        let Ok((session, handles)) = self.session_and_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = services.event_sender();

        SessionTaskService::append_session_output(
            &handles.output,
            services.db(),
            &app_event_tx,
            &session.id,
            output,
        )
        .await;
    }

    /// Removes prompt attachment files that are no longer owned by the
    /// composer or worker.
    ///
    /// Only Agentty-managed temp files under `AGENTTY_ROOT/tmp/` are removed.
    pub(crate) async fn cleanup_prompt_attachment_files(
        &self,
        services: &AppServices,
        prompt: &TurnPrompt,
    ) {
        Self::cleanup_prompt_attachment_paths(
            services.fs_client(),
            prompt.local_image_paths().cloned().collect(),
        )
        .await;
    }

    /// Cancels a session that is currently in review and drops its worktree.
    ///
    /// Persisted transcript metadata remains available after the worktree
    /// checkout and session branch are removed.
    ///
    /// # Errors
    /// Returns an error if the session is not found or not in review status.
    pub async fn cancel_session(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let session = self.session_or_err(session_id)?;
        if !session.status.allows_review_actions() {
            return Err(SessionError::Workflow(
                "Session must be in review to be canceled".to_string(),
            ));
        }

        let branch_name = session_branch(&session.id);
        let folder = session.folder.clone();
        let handles = self.session_handles_or_err(session_id)?;
        let status = Arc::clone(&handles.status);
        let app_event_tx = services.event_sender();

        let status_updated = SessionTaskService::update_status(
            &status,
            services.clock().as_ref(),
            services.db(),
            &app_event_tx,
            session_id,
            Status::Canceled,
        )
        .await;

        if status_updated {
            let repo_root = services
                .git_client()
                .main_repo_root(folder.clone())
                .await
                .ok();

            Self::cleanup_session_worktree_resources(
                services.fs_client().clone(),
                services.git_client(),
                folder,
                branch_name,
                repo_root,
                true,
            )
            .await;
            Self::cleanup_session_temp_directory(services.fs_client(), session_id).await;
        }

        Ok(())
    }

    /// Removes git and filesystem resources for one session worktree.
    ///
    /// This best-effort helper is shared by terminal-state cleanup and session
    /// deletion so both paths remove the linked worktree checkout, delete the
    /// session branch when the shared repository root is known, and finally
    /// remove the directory from disk.
    async fn cleanup_session_worktree_resources(
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn git::GitClient>,
        folder: PathBuf,
        branch_name: String,
        repo_root: Option<PathBuf>,
        remove_git_resources: bool,
    ) {
        if remove_git_resources {
            // Best-effort cleanup: worktree may already be removed.
            let _ = git_client.remove_worktree(folder.clone()).await;

            if let Some(repo_root) = repo_root {
                // Best-effort cleanup: branch may already be removed.
                let _ = git_client.delete_branch(repo_root, branch_name).await;
            }
        }

        // Best-effort cleanup: worktree directory may already be removed.
        let _ = fs_client.remove_dir_all(folder).await;
    }

    /// Removes Agentty-managed prompt attachment files and prunes their
    /// now-empty image directory when possible.
    pub(crate) async fn cleanup_prompt_attachment_paths(
        fs_client: Arc<dyn FsClient>,
        attachment_paths: Vec<PathBuf>,
    ) {
        Self::cleanup_prompt_attachment_paths_in_root(
            fs_client,
            &prompt_attachment_tmp_root(),
            attachment_paths,
        )
        .await;
    }

    /// Removes Agentty-managed prompt attachment files inside one explicit tmp
    /// root and prunes their shared image directory when safe.
    async fn cleanup_prompt_attachment_paths_in_root(
        fs_client: Arc<dyn FsClient>,
        managed_tmp_root: &Path,
        attachment_paths: Vec<PathBuf>,
    ) {
        if attachment_paths.is_empty() {
            return;
        }

        let image_directory =
            managed_prompt_attachment_directory(&attachment_paths, managed_tmp_root);

        for attachment_path in attachment_paths {
            if is_managed_prompt_attachment_path(&attachment_path, managed_tmp_root) {
                // Best-effort cleanup: attachment file may already be removed.
                let _ = fs_client.remove_file(attachment_path).await;
            }
        }

        if let Some(image_directory) = image_directory {
            // Best-effort cleanup: image directory may already be removed.
            let _ = fs_client.remove_dir_all(image_directory).await;
        }
    }

    /// Removes the session-scoped temp directory used for pasted prompt
    /// images.
    async fn cleanup_session_temp_directory(fs_client: Arc<dyn FsClient>, session_id: &str) {
        // Best-effort cleanup: temp directory may already be removed.
        let _ = fs_client
            .remove_dir_all(session_prompt_temp_directory(session_id))
            .await;
    }
}

/// Replaces only the first occurrence of `needle` in `haystack`.
///
/// If `needle` is absent, the original string is returned unchanged.
fn replace_first(haystack: &str, needle: &str, replacement: &str) -> String {
    let Some(match_index) = haystack.find(needle) else {
        return haystack.to_string();
    };

    let mut replaced = String::with_capacity(
        haystack
            .len()
            .saturating_sub(needle.len())
            .saturating_add(replacement.len()),
    );
    replaced.push_str(&haystack[..match_index]);
    replaced.push_str(replacement);
    replaced.push_str(&haystack[match_index + needle.len()..]);

    replaced
}

/// Returns the session-scoped temp directory used for pasted prompt images.
fn session_prompt_temp_directory(session_id: &str) -> PathBuf {
    agentty_home().join("tmp").join(session_id)
}

/// Returns the Agentty-owned tmp root used for pasted prompt attachments.
fn prompt_attachment_tmp_root() -> PathBuf {
    agentty_home().join("tmp")
}

/// Returns the shared managed image directory for the given attachment paths
/// when every path stays within the Agentty temp root.
fn managed_prompt_attachment_directory(
    attachment_paths: &[PathBuf],
    managed_tmp_root: &Path,
) -> Option<PathBuf> {
    let image_directory = attachment_paths.first()?.parent()?.to_path_buf();
    if !is_managed_prompt_attachment_directory(&image_directory, managed_tmp_root) {
        return None;
    }

    attachment_paths
        .iter()
        .all(|attachment_path| {
            attachment_path.parent() == Some(image_directory.as_path())
                && is_managed_prompt_attachment_path(attachment_path, managed_tmp_root)
        })
        .then_some(image_directory)
}

/// Returns whether one attachment path is owned by Agentty under the managed
/// prompt-image tmp root.
fn is_managed_prompt_attachment_path(path: &Path, managed_tmp_root: &Path) -> bool {
    path.parent().is_some_and(|parent| {
        is_managed_prompt_attachment_directory(parent, managed_tmp_root)
            && path.starts_with(managed_tmp_root)
    })
}

/// Returns whether one directory is an Agentty-managed prompt-image directory.
fn is_managed_prompt_attachment_directory(path: &Path, managed_tmp_root: &Path) -> bool {
    path.starts_with(managed_tmp_root) && path.ends_with("images")
}

#[cfg(test)]
mod test_support {
    use std::sync::Arc;

    use super::*;

    impl SessionManager {
        /// Submits a follow-up prompt using a pre-built backend for
        /// deterministic test execution.
        ///
        /// Creates a [`CliAgentChannel`] backed by the given
        /// [`agent::AgentBackend`] and registers it in the session-local
        /// channel map so the worker uses it instead of the default factory.
        /// This allows tests to control spawned process commands without
        /// relying on a real provider binary.
        pub(crate) async fn reply_with_backend(
            &mut self,
            services: &AppServices,
            session_id: &str,
            prompt: impl Into<TurnPrompt>,
            backend: Arc<dyn agent::AgentBackend>,
            session_model: AgentModel,
        ) {
            let prompt = prompt.into();
            let channel: Arc<dyn crate::infra::channel::AgentChannel> =
                Arc::new(crate::infra::channel::cli::CliAgentChannel::with_backend(
                    backend,
                    session_model.kind(),
                ));
            self.worker_service
                .test_agent_channels
                .insert(session_id.to_string(), channel);
            self.reply_impl(services, session_id, prompt, session_model)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use ag_forge as forge;
    use ratatui::widgets::TableState;

    use super::*;
    use crate::app::session::{RealClock, SessionDefaults};
    use crate::app::{AppServices, SessionState};
    use crate::domain::agent::AgentKind;
    use crate::domain::session::{
        ForgeKind, ReviewRequestState, ReviewRequestSummary, SessionHandles, SessionSize,
        SessionStats,
    };
    use crate::infra::channel::TurnPromptAttachment;
    use crate::infra::db::{self, Database};
    use crate::infra::{app_server, fs};

    /// Builds a session manager with one session for reply-context tests.
    fn session_manager_with_one_session(session: Session) -> SessionManager {
        let mut handles = HashMap::new();
        handles.insert(
            session.id.clone(),
            SessionHandles::new(session.output.clone(), session.status),
        );

        let state = SessionState::new(
            handles,
            vec![session],
            TableState::default(),
            Arc::new(RealClock),
            1,
            0,
        );

        SessionManager::new(
            SessionDefaults {
                model: AgentModel::Gpt53Codex,
            },
            Arc::new(git::MockGitClient::new()),
            state,
            Vec::new(),
        )
    }

    /// Builds a minimal in-memory session snapshot for lifecycle unit tests.
    fn test_session(prompt: &str, status: Status, title: Option<&str>, output: &str) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::from("/tmp/session"),
            follow_up_tasks: Vec::new(),
            id: "session-id".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::ClaudeSonnet46,
            output: output.to_string(),
            project_name: "project".to_string(),
            prompt: prompt.to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: title.map(ToString::to_string),
            updated_at: 0,
        }
    }

    /// Builds one mock app-server client wrapped in `Arc` for service
    /// fixtures.
    fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
        Arc::new(app_server::MockAppServerClient::new())
    }

    /// Builds a filesystem mock that delegates simple checks to local disk.
    fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_create_dir_all()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_remove_dir_all()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_read_file()
            .times(0..)
            .returning(|path| {
                Box::pin(async move { tokio::fs::read(path).await.map_err(fs::FsError::from) })
            });
        mock_fs_client
            .expect_remove_file()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_is_dir()
            .times(0..)
            .returning(|path| path.is_dir());

        mock_fs_client
    }

    /// Persists one session row that matches the in-memory fixture.
    async fn database_with_session(session: &Session) -> Database {
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        if session.is_draft {
            database
                .insert_draft_session(
                    &session.id,
                    session.model.as_str(),
                    &session.base_branch,
                    &session.status.to_string(),
                    project_id,
                )
                .await
                .expect("failed to insert draft session");
        } else {
            database
                .insert_session(
                    &session.id,
                    session.model.as_str(),
                    &session.base_branch,
                    &session.status.to_string(),
                    project_id,
                )
                .await
                .expect("failed to insert session");
        }
        database
            .update_session_prompt(&session.id, &session.prompt)
            .await
            .expect("failed to persist session prompt");
        if let Some(title) = &session.title {
            database
                .update_session_title(&session.id, title)
                .await
                .expect("failed to persist session title");
        }
        if let Some(review_request) = &session.review_request {
            database
                .update_session_review_request(&session.id, Some(review_request))
                .await
                .expect("failed to persist session review request");
        }

        database
    }

    /// Builds app services with caller-provided filesystem, git, and forge
    /// boundaries.
    fn test_services_with_fs_client(
        database: Database,
        fs_client: Arc<dyn fs::FsClient>,
        git_client: Arc<dyn git::GitClient>,
        review_request_client: Arc<dyn forge::ReviewRequestClient>,
    ) -> AppServices {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        AppServices::new(
            PathBuf::from("/tmp/agentty-tests"),
            Arc::new(crate::app::session::RealClock),
            database,
            event_tx,
            crate::app::service::AppServiceDeps {
                app_server_client_override: Some(mock_app_server()),
                fs_client,
                available_agent_kinds: AgentKind::ALL.to_vec(),
                git_client,
                review_request_client,
            },
        )
    }

    /// Builds app services with caller-provided git and forge boundaries.
    fn test_services(
        database: Database,
        git_client: Arc<dyn git::GitClient>,
        review_request_client: Arc<dyn forge::ReviewRequestClient>,
    ) -> AppServices {
        test_services_with_fs_client(
            database,
            Arc::new(create_passthrough_mock_fs_client()),
            git_client,
            review_request_client,
        )
    }

    /// Builds one normalized review-request summary for workflow tests.
    fn review_request_summary(display_id: &str) -> ReviewRequestSummary {
        ReviewRequestSummary {
            display_id: display_id.to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: session_branch("session-id"),
            state: ReviewRequestState::Open,
            status_summary: Some("Checks pending".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: format!(
                "https://github.com/agentty-xyz/agentty/pull/{}",
                &display_id[1..]
            ),
        }
    }

    /// Returns one GitHub forge-remote fixture for review-request tests.
    fn github_remote() -> forge::ForgeRemote {
        forge::ForgeRemote {
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Returns the expected create payload for one session review request.
    fn expected_create_input() -> forge::CreateReviewRequestInput {
        forge::CreateReviewRequestInput {
            body: None,
            source_branch: session_branch("session-id"),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        }
    }

    /// Configures git expectations for review-request publication.
    fn expect_published_session_branch(mock_git_client: &mut git::MockGitClient) {
        mock_git_client
            .expect_push_current_branch()
            .times(1)
            .returning(|_| Box::pin(async { Ok("origin/agentty/session-id".to_string()) }));
        mock_git_client.expect_repo_url().times(1).returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
    }

    /// Loads the persisted session row used by workflow assertions.
    async fn load_persisted_session_row(database: &Database) -> db::SessionRow {
        database
            .load_sessions()
            .await
            .expect("failed to load session rows")
            .into_iter()
            .find(|row| row.id == "session-id")
            .expect("session row should exist")
    }

    #[tokio::test]
    async fn test_publish_review_request_creates_and_persists_link_when_lookup_misses() {
        // Arrange
        let session = test_session(
            "Implement forge review support",
            Status::Review,
            Some("Add forge review support"),
            "",
        );
        let database = database_with_session(&session).await;
        let mut session_manager = session_manager_with_one_session(session);
        let source_branch = session_branch("session-id");
        let expected_create_input = expected_create_input();
        let remote = github_remote();
        let created_summary = review_request_summary("#42");
        let mut mock_git_client = git::MockGitClient::new();
        expect_published_session_branch(&mut mock_git_client);
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .returning({
                let remote = remote.clone();
                move |_| Ok(remote.clone())
            });
        mock_review_request_client
            .expect_find_by_source_branch()
            .times(1)
            .withf({
                let remote = remote.clone();
                let source_branch = source_branch.clone();
                move |candidate_remote, candidate_source_branch| {
                    candidate_remote == &remote && candidate_source_branch == &source_branch
                }
            })
            .returning(|_, _| Box::pin(async { Ok(None) }));
        mock_review_request_client
            .expect_create_review_request()
            .times(1)
            .withf({
                let remote = remote.clone();
                let expected_create_input = expected_create_input.clone();
                move |candidate_remote, candidate_input| {
                    candidate_remote == &remote && candidate_input == &expected_create_input
                }
            })
            .returning(move |_, _| {
                let created_summary = created_summary.clone();

                Box::pin(async move { Ok(created_summary) })
            });
        let services = test_services(
            database.clone(),
            Arc::new(mock_git_client),
            Arc::new(mock_review_request_client),
        );

        // Act
        let review_request = session_manager
            .publish_review_request(&services, "session-id")
            .await
            .expect("review request should be created");
        let persisted_row = load_persisted_session_row(&database).await;

        // Assert
        assert_eq!(review_request.summary.display_id, "#42");
        assert_eq!(
            session_manager.state.sessions[0].review_request,
            Some(review_request.clone())
        );
        assert_eq!(
            persisted_row
                .review_request
                .as_ref()
                .map(|row| row.display_id.as_str()),
            Some("#42")
        );
        assert_eq!(
            persisted_row
                .review_request
                .as_ref()
                .map(|row| row.last_refreshed_at),
            Some(review_request.last_refreshed_at)
        );
        assert_eq!(
            persisted_row.published_upstream_ref.as_deref(),
            Some("origin/agentty/session-id")
        );
        assert_eq!(
            session_manager.state.sessions[0]
                .published_upstream_ref
                .as_deref(),
            Some("origin/agentty/session-id")
        );
    }

    #[tokio::test]
    async fn test_stage_draft_message_preserves_persisted_prompt_when_attachment_write_fails() {
        // Arrange
        let mut session = test_session("", Status::New, None, "");
        session.is_draft = true;
        let database = database_with_session(&session).await;
        let mut session_manager = session_manager_with_one_session(session);
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_create_dir_all()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_remove_dir_all()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_read_file()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
        mock_fs_client
            .expect_remove_file()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_is_dir()
            .times(0..)
            .returning(|path| path.is_dir());
        mock_fs_client.expect_write_file().once().returning(|_, _| {
            Box::pin(async {
                Err(fs::FsError::Io(std::io::Error::other(
                    "simulated attachment write failure",
                )))
            })
        });
        let services = test_services_with_fs_client(
            database.clone(),
            Arc::new(mock_fs_client),
            Arc::new(git::MockGitClient::new()),
            Arc::new(forge::MockReviewRequestClient::new()),
        );
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-1.png"),
            }],
            text: "Review [Image #1]".to_string(),
        };

        // Act
        let error = session_manager
            .stage_draft_message(&services, "session-id", prompt)
            .await
            .expect_err("attachment metadata failure should abort draft staging");
        let persisted_session = load_persisted_session_row(&database).await;

        // Assert
        assert!(matches!(error, SessionError::Fs(_)));
        assert!(persisted_session.prompt.is_empty());
        assert!(session_manager.sessions[0].prompt.is_empty());
        assert!(session_manager.sessions[0].draft_attachments.is_empty());
    }

    #[tokio::test]
    async fn test_publish_review_request_reuses_existing_remote_link_before_create() {
        // Arrange
        let session = test_session(
            "Implement forge review support",
            Status::Review,
            Some("Add forge review support"),
            "",
        );
        let database = database_with_session(&session).await;
        let mut session_manager = session_manager_with_one_session(session);
        let source_branch = session_branch("session-id");
        let remote = github_remote();
        let existing_summary = review_request_summary("#24");
        let mut mock_git_client = git::MockGitClient::new();
        expect_published_session_branch(&mut mock_git_client);
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .returning({
                let remote = remote.clone();
                move |_| Ok(remote.clone())
            });
        mock_review_request_client
            .expect_find_by_source_branch()
            .times(1)
            .withf({
                let remote = remote.clone();
                let source_branch = source_branch.clone();
                move |candidate_remote, candidate_source_branch| {
                    candidate_remote == &remote && candidate_source_branch == &source_branch
                }
            })
            .returning(move |_, _| {
                let existing_summary = existing_summary.clone();

                Box::pin(async move { Ok(Some(existing_summary)) })
            });
        mock_review_request_client
            .expect_create_review_request()
            .times(0);
        let services = test_services(
            database,
            Arc::new(mock_git_client),
            Arc::new(mock_review_request_client),
        );

        // Act
        let review_request = session_manager
            .publish_review_request(&services, "session-id")
            .await
            .expect("existing remote review request should be reused");

        // Assert
        assert_eq!(review_request.summary.display_id, "#24");
        assert_eq!(
            session_manager.state.sessions[0]
                .review_request
                .as_ref()
                .map(|review_request| review_request.summary.display_id.as_str()),
            Some("#24")
        );
    }

    #[tokio::test]
    async fn test_publish_review_request_reuses_stored_link_without_git_or_forge_calls() {
        // Arrange
        let mut session = test_session(
            "Implement forge review support",
            Status::Done,
            Some("Add forge review support"),
            "",
        );
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 42,
            summary: review_request_summary("#11"),
        });
        let expected_review_request = session
            .review_request
            .clone()
            .expect("fixture should include a review request");
        let database = database_with_session(&session).await;
        let mut session_manager = session_manager_with_one_session(session);
        let mock_git_client = git::MockGitClient::new();
        let mock_review_request_client = forge::MockReviewRequestClient::new();
        let services = test_services(
            database,
            Arc::new(mock_git_client),
            Arc::new(mock_review_request_client),
        );

        // Act
        let review_request = session_manager
            .publish_review_request(&services, "session-id")
            .await
            .expect("stored review request should be reused");

        // Assert
        assert_eq!(review_request, expected_review_request);
    }

    #[tokio::test]
    async fn test_review_request_web_url_returns_linked_review_request_url() {
        // Arrange
        let mut session = test_session(
            "Implement forge review support",
            Status::Done,
            Some("Add forge review support"),
            "",
        );
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 42,
            summary: review_request_summary("#11"),
        });
        let session_manager = session_manager_with_one_session(session);
        let database = database_with_session(
            session_manager
                .state
                .sessions
                .first()
                .expect("fixture session should exist"),
        )
        .await;
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_review_request_web_url()
            .times(1)
            .returning(|summary| Ok(summary.web_url.clone()));
        let services = test_services(
            database,
            Arc::new(git::MockGitClient::new()),
            Arc::new(mock_review_request_client),
        );

        // Act
        let review_request_url = session_manager
            .review_request_web_url(&services, "session-id")
            .expect("linked review request URL should be returned");

        // Assert
        assert_eq!(
            review_request_url,
            "https://github.com/agentty-xyz/agentty/pull/11"
        );
    }

    #[test]
    fn test_formatted_prompt_output_formats_multiline_prompt_with_continuation_prefix() {
        // Arrange
        let prompt = TurnPrompt::from_text("first line\n\n\nafter gap".to_string());

        // Act
        let formatted_prompt = SessionManager::formatted_prompt_output(&prompt, false);

        // Assert
        assert_eq!(
            formatted_prompt,
            " › first line\n   \n   \n   after gap\n\n"
        );
    }

    #[test]
    fn test_formatted_prompt_output_prepends_newline_for_replies() {
        // Arrange
        let prompt = TurnPrompt::from_text("reply line".to_string());

        // Act
        let formatted_prompt = SessionManager::formatted_prompt_output(&prompt, true);

        // Assert
        assert_eq!(formatted_prompt, "\n › reply line\n\n");
    }

    #[test]
    /// Ensures transcript formatting keeps prompt image markers visible.
    fn test_formatted_prompt_output_preserves_image_placeholders_in_transcript() {
        // Arrange
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-1.png"),
            }],
            text: "Review [Image #1]".to_string(),
        };

        // Act
        let formatted_prompt = SessionManager::formatted_prompt_output(&prompt, false);

        // Assert
        assert_eq!(formatted_prompt, " › Review [Image #1]\n\n");
    }

    #[test]
    fn test_renumbered_prompt_text_rewrites_only_attachment_occurrences() {
        // Arrange
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-1.png"),
            }],
            text: "Attach [Image #1] but keep literal [Image #1] text".to_string(),
        };

        // Act
        let renumbered_prompt = SessionManager::renumbered_prompt_text(&prompt, 2);

        // Assert
        assert_eq!(
            renumbered_prompt,
            "Attach [Image #2] but keep literal [Image #1] text"
        );
    }

    #[tokio::test]
    /// Ensures prompt attachment cleanup removes temp files and their image
    /// directory after handoff.
    async fn test_cleanup_prompt_attachment_paths_removes_files_and_directory() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("temp dir should exist");
        let managed_tmp_root = temp_dir.path().join("tmp");
        let image_directory = managed_tmp_root.join("session-id").join("images");
        std::fs::create_dir_all(&image_directory).expect("image directory should exist");
        let first_image = image_directory.join("image-1.png");
        let second_image = image_directory.join("image-2.png");
        std::fs::write(&first_image, b"png").expect("first image should exist");
        std::fs::write(&second_image, b"png").expect("second image should exist");

        // Act
        SessionManager::cleanup_prompt_attachment_paths_in_root(
            Arc::new(fs::RealFsClient),
            &managed_tmp_root,
            vec![first_image.clone(), second_image.clone()],
        )
        .await;

        // Assert
        assert!(!first_image.exists());
        assert!(!second_image.exists());
        assert!(!image_directory.exists());
    }

    #[tokio::test]
    /// Ensures cleanup ignores attachment paths outside the managed Agentty
    /// temp root.
    async fn test_cleanup_prompt_attachment_paths_leaves_unmanaged_files_untouched() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("temp dir should exist");
        let managed_tmp_root = temp_dir.path().join("tmp");
        let image_directory = temp_dir.path().join("user-images");
        std::fs::create_dir_all(&image_directory).expect("image directory should exist");
        let image_path = image_directory.join("image-1.png");
        std::fs::write(&image_path, b"png").expect("image file should exist");

        // Act
        SessionManager::cleanup_prompt_attachment_paths_in_root(
            Arc::new(fs::RealFsClient),
            &managed_tmp_root,
            vec![image_path.clone()],
        )
        .await;

        // Assert
        assert!(image_path.exists());
        assert!(image_directory.exists());
    }

    #[test]
    /// Ensures first replies persist the full prompt as the one-time title.
    fn test_prepare_reply_context_first_message_sets_title_from_prompt() {
        // Arrange
        let prompt = "Implement optimistic retry path";
        let turn_prompt = TurnPrompt::from_text(prompt.to_string());
        let session = test_session("", Status::New, None, "");
        let mut session_manager = session_manager_with_one_session(session);

        // Act
        let context = session_manager
            .prepare_reply_context(0, &turn_prompt, AgentModel::ClaudeSonnet46, false)
            .expect("reply context should be available")
            .expect("session should produce reply context");

        // Assert
        assert_eq!(context.0, None);
        assert!(context.1);
        assert_eq!(context.2, "session-id");
        assert_eq!(context.3, Some(prompt.to_string()));
        assert_eq!(session_manager.sessions[0].prompt, prompt);
        assert_eq!(session_manager.sessions[0].title, Some(prompt.to_string()));
    }

    #[test]
    /// Ensures follow-up replies keep the existing title unchanged.
    fn test_prepare_reply_context_follow_up_keeps_existing_title() {
        // Arrange
        let session = test_session(
            "Initial prompt",
            Status::Review,
            Some("Initial prompt"),
            "existing output",
        );
        let mut session_manager = session_manager_with_one_session(session);
        let prompt = TurnPrompt::from_text("Follow-up prompt".to_string());

        // Act
        let context = session_manager
            .prepare_reply_context(0, &prompt, AgentModel::ClaudeSonnet46, false)
            .expect("reply context should be available")
            .expect("session should produce reply context");

        // Assert
        assert_eq!(context.0, None);
        assert!(!context.1);
        assert_eq!(context.2, "session-id");
        assert_eq!(context.3, None);
        assert_eq!(session_manager.sessions[0].prompt, "Initial prompt");
        assert_eq!(
            session_manager.sessions[0].title,
            Some("Initial prompt".to_string())
        );
    }

    #[test]
    /// Ensures replying to an in-progress session returns a typed
    /// [`SessionError::Workflow`] instead of a raw string.
    fn test_prepare_reply_context_returns_workflow_error_when_status_blocks_reply() {
        // Arrange
        let session = test_session("Initial prompt", Status::InProgress, Some("Title"), "");
        let mut session_manager = session_manager_with_one_session(session);
        let prompt = TurnPrompt::from_text("Another prompt".to_string());

        // Act
        let result =
            session_manager.prepare_reply_context(0, &prompt, AgentModel::ClaudeSonnet46, false);

        // Assert
        let error = result.expect_err("in-progress session should block reply");
        assert!(
            matches!(error, SessionError::Workflow(_)),
            "expected SessionError::Workflow, got: {error:?}"
        );
    }

    #[test]
    /// Ensures title-generation prompt rendering includes session request text.
    fn test_session_title_generation_prompt_includes_request() {
        // Arrange
        let request_prompt = "Refactor session lifecycle updates";

        // Act
        let title_prompt = SessionManager::session_title_generation_prompt(request_prompt)
            .expect("title generation prompt should render");

        // Assert
        assert!(title_prompt.contains("Generate a concise, commit-style title"));
        assert!(title_prompt.contains("Describe what the user wants to do"));
        assert!(title_prompt.contains("Keep it high-level and intent-focused."));
        assert!(title_prompt.contains("Do not include long file names"));
        assert!(title_prompt.contains("Return only the title text."));
        assert!(title_prompt.contains(request_prompt));
    }

    #[test]
    /// Ensures single-line title responses are normalized and accepted.
    fn test_parse_generated_session_title_accepts_plain_title() {
        // Arrange
        let response_content = "Refine session startup flow";

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(
            parsed_title,
            Some("Refine session startup flow".to_string())
        );
    }

    #[test]
    /// Ensures protocol-wrapped plain answer lines are accepted.
    fn test_parse_generated_session_title_accepts_protocol_answer_plain_text() {
        // Arrange
        let response_content =
            r#"{"answer":"Polish Gemini title parsing","questions":[],"summary":null}"#;

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(
            parsed_title,
            Some("Polish Gemini title parsing".to_string())
        );
    }

    #[test]
    /// Ensures plain-text responses with extra lines keep only the first
    /// non-empty title line.
    fn test_parse_generated_session_title_uses_first_nonempty_line_for_multiline_response() {
        // Arrange
        let response_content = "Polish Gemini title parsing\nExtra detail that should be ignored";

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(
            parsed_title,
            Some("Polish Gemini title parsing".to_string())
        );
    }

    #[test]
    /// Ensures protocol payloads without `answer` text do not update
    /// titles.
    fn test_parse_generated_session_title_returns_none_for_question_only_protocol_payload() {
        // Arrange
        let response_content = r#"{"answer":"","questions":[{"text":"Need confirmation?","options":[]}],"summary":null}"#;

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(parsed_title, None);
    }

    #[test]
    /// Ensures `Title:` prefixes are normalized before persistence.
    fn test_parse_generated_session_title_normalizes_title_prefix() {
        // Arrange
        let response_content = "Title: \"Polish merge queue behavior\"";

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(
            parsed_title,
            Some("Polish merge queue behavior".to_string())
        );
    }
}
