//! Session lifecycle workflows and direct user actions.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use askama::Template;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::access::SESSION_NOT_FOUND_ERROR;
use super::worker::SessionCommand;
use super::{SessionTaskService, session_branch, session_folder, unix_timestamp_from_system_time};
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager, setting};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{ReviewRequest, SESSION_DATA_DIR, Session, Status};
use crate::domain::setting::SettingName;
use crate::infra::channel::TurnMode;
use crate::infra::fs::FsClient;
use crate::infra::{agent, db, forge, git};
use crate::ui::page::session_list::grouped_session_indexes;

/// Maximum stored length for generated session titles.
const GENERATED_SESSION_TITLE_MAX_CHARACTERS: usize = 72;
const USER_PROMPT_PREFIX: &str = " › ";
const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";

/// Input bag for constructing a queued session command.
struct BuildSessionCommandInput {
    is_first_message: bool,
    prompt: String,
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
    ) -> Result<String, String> {
        let base_branch = projects
            .git_branch()
            .ok_or_else(|| "Git branch is required to create a session".to_string())?;
        let session_model = self
            .resolve_default_session_model(services, projects.active_project_id())
            .await;
        self.default_session_model = session_model;

        let session_id = Uuid::new_v4().to_string();
        let folder = session_folder(services.base_path(), &session_id);
        if folder.exists() {
            return Err(format!("Session folder {session_id} already exists"));
        }

        let worktree_branch = session_branch(&session_id);
        let working_dir = projects.working_dir().to_path_buf();
        let git_client = services.git_client();
        let repo_root = git_client
            .find_git_repo_root(working_dir)
            .await
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        {
            let folder = folder.clone();
            let repo_root = repo_root.clone();
            let worktree_branch = worktree_branch.clone();
            let base_branch = base_branch.to_string();
            git_client
                .create_worktree(repo_root, folder, worktree_branch, base_branch)
                .await
                .map_err(|error| format!("Failed to create git worktree: {error}"))?;
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

            return Err(format!(
                "Failed to create session metadata directory: {error}"
            ));
        }

        if let Err(error) = services
            .db()
            .insert_session(
                &session_id,
                session_model.as_str(),
                base_branch,
                &Status::New.to_string(),
                projects.active_project_id(),
            )
            .await
        {
            self.rollback_failed_session_creation(
                services,
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!("Failed to save session metadata: {error}"));
        }

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

            return Err(format!("Failed to setup session backend: {error}"));
        }
        services.emit_app_event(AppEvent::RefreshSessions);

        Ok(session_id)
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
        prompt: String,
    ) -> Result<(), String> {
        let session_index = self.session_index_or_err(session_id)?;
        let (persisted_session_id, session_model, title) = {
            let session = self
                .sessions
                .get_mut(session_index)
                .ok_or_else(|| SESSION_NOT_FOUND_ERROR.to_string())?;

            session.prompt.clone_from(&prompt);

            let title = prompt.clone();
            session.title = Some(title.clone());
            let session_model = session.model;

            (session.id.clone(), session_model, title)
        };

        let handles = self.session_handles_or_err(&persisted_session_id)?;
        let output = Arc::clone(&handles.output);
        let status = Arc::clone(&handles.status);
        let app_event_tx = services.event_sender();

        self.persist_first_message_metadata(services, &persisted_session_id, &prompt, &title)
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

        let _ = SessionTaskService::update_status(
            &status,
            services.db(),
            &app_event_tx,
            &persisted_session_id,
            Status::InProgress,
        )
        .await;

        let operation_id = Uuid::new_v4().to_string();
        let command = SessionCommand::Run {
            operation_id,
            mode: TurnMode::Start,
            prompt: prompt.clone(),
            session_model,
        };
        self.enqueue_session_command(services, &persisted_session_id, command)
            .await?;

        Ok(())
    }

    /// Submits a follow-up prompt to an existing session.
    pub async fn reply(&mut self, services: &AppServices, session_id: &str, prompt: &str) {
        let Ok(session) = self.session_or_err(session_id) else {
            return;
        };
        let session_model = session.model;
        self.reply_impl(services, session_id, prompt, session_model)
            .await;
    }

    /// Submits a follow-up prompt using a pre-built backend for deterministic
    /// test execution.
    ///
    /// Creates a [`CliAgentChannel`] backed by the given [`AgentBackend`] and
    /// registers it in the session-local channel map so the worker uses it
    /// instead of the default factory. This allows tests to control spawned
    /// process commands without relying on a real provider binary.
    #[cfg(test)]
    pub(crate) async fn reply_with_backend(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: &str,
        backend: std::sync::Arc<dyn agent::AgentBackend>,
        session_model: AgentModel,
    ) {
        let channel: std::sync::Arc<dyn crate::infra::channel::AgentChannel> =
            std::sync::Arc::new(crate::infra::channel::cli::CliAgentChannel::with_backend(
                backend,
                session_model.kind(),
            ));
        self.worker_service
            .test_agent_channels
            .insert(session_id.to_string(), channel);
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
    ) -> Result<(), String> {
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
                    SettingName::DefaultSmartModel.as_str(),
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
    ) -> Result<bool, String> {
        let Some(project_id) = project_id else {
            return Ok(false);
        };

        let should_persist = services
            .db()
            .get_project_setting(project_id, SettingName::LastUsedModelAsDefault.as_str())
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
    ) -> Result<ReviewRequest, String> {
        let session_index = self.session_index_or_err(session_id)?;
        let Some(session) = self.state.sessions.get(session_index) else {
            return Err(SESSION_NOT_FOUND_ERROR.to_string());
        };

        if let Some(review_request) = session.review_request.clone() {
            return Ok(review_request);
        }

        if session.status != Status::Review {
            return Err("Session must be in review to create a review request".to_string());
        }

        let folder = session.folder.clone();
        let source_branch = session_branch(session_id);
        let create_input = Self::review_request_create_input(session, source_branch.clone());
        let git_client = services.git_client();
        git_client
            .push_current_branch(folder.clone())
            .await
            .map_err(|error| format!("Failed to publish session branch: {error}"))?;

        let repo_url = git_client.repo_url(folder).await.map_err(|error| {
            format!("Failed to resolve repository remote for review request: {error}")
        })?;
        let review_request_client = services.review_request_client();
        let remote = review_request_client
            .detect_remote(repo_url)
            .map_err(|error| error.detail_message())?;
        let review_request_summary = match review_request_client
            .find_by_source_branch(remote.clone(), source_branch)
            .await
            .map_err(|error| error.detail_message())?
        {
            Some(existing_review_request) => existing_review_request,
            None => review_request_client
                .create_review_request(remote, create_input)
                .await
                .map_err(|error| error.detail_message())?,
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
    ) -> Result<String, String> {
        let session = self.session_or_err(session_id)?;
        let review_request = session
            .review_request
            .as_ref()
            .ok_or_else(|| "Session has no linked review request".to_string())?;

        services
            .review_request_client()
            .review_request_web_url(&review_request.summary)
            .map_err(|error| error.detail_message())
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

        let _ = services
            .db()
            .request_cancel_for_session_operations(&session.id)
            .await;
        self.clear_session_worker(&session.id);
        let _ = services.db().delete_session(&session.id).await;
        services.emit_app_event(AppEvent::RefreshSessions);

        Some(DeletedSessionCleanup {
            branch_name: session_branch(&session.id),
            folder: session.folder,
            has_git_branch: projects.has_git_branch(),
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
            fs_client,
            git_client,
            cleanup.folder,
            cleanup.branch_name,
            repo_root,
            cleanup.has_git_branch,
        )
        .await;
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
    ) -> Result<ReviewRequest, String> {
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
    ) -> Result<ReviewRequest, String> {
        let session_id = self
            .state
            .sessions
            .get(session_index)
            .map(|session| session.id.clone())
            .ok_or_else(|| SESSION_NOT_FOUND_ERROR.to_string())?;
        services
            .db()
            .update_session_review_request(&session_id, Some(&review_request))
            .await?;

        let Some(session) = self.state.sessions.get_mut(session_index) else {
            return Err(SESSION_NOT_FOUND_ERROR.to_string());
        };
        session.review_request = Some(review_request.clone());

        Ok(review_request)
    }

    /// Validates and queues a follow-up prompt for an existing session.
    ///
    /// Gathers reply context, appends the prompt line to session output, builds
    /// a [`SessionCommand::Run`] with the appropriate [`TurnMode`], and
    /// enqueues it on the session worker.
    async fn reply_impl(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: &str,
        session_model: AgentModel,
    ) {
        let Ok(session_index) = self.session_index_or_err(session_id) else {
            return;
        };
        let should_replay_history = self.should_replay_history(session_id);
        let (session_output, is_first_message, persisted_session_id, title_to_save) = match self
            .prepare_reply_context(session_index, prompt, session_model, should_replay_history)
        {
            Ok(Some(reply_context)) => reply_context,
            Ok(None) => return,
            Err(blocked_session_id) => {
                self.append_reply_status_error(services, &blocked_session_id)
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
                effective_prompt,
                &title,
            )
            .await;

            let _ = SessionTaskService::update_status(
                &status,
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
            prompt,
        )
        .await;

        let command = Self::build_session_command(BuildSessionCommandInput {
            is_first_message,
            prompt: effective_prompt.to_string(),
            session_model,
            session_output,
        });
        self.enqueue_reply_command(
            services,
            &output,
            &app_event_tx,
            &persisted_session_id,
            command,
        )
        .await;
    }

    /// Validates reply eligibility and gathers per-session values needed for
    /// queueing a reply command.
    ///
    /// # Errors
    /// Returns the blocked session id when session status does not allow
    /// replying.
    fn prepare_reply_context(
        &mut self,
        session_index: usize,
        prompt: &str,
        session_model: AgentModel,
        should_replay_history: bool,
    ) -> Result<Option<ReplyContext>, String> {
        let Some(session) = self.state.sessions.get_mut(session_index) else {
            return Ok(None);
        };

        let is_first_message = session.prompt.is_empty();
        let allowed = session.status == Status::Review
            || session.status == Status::Question
            || (is_first_message && session.status == Status::New);
        if !allowed {
            return Err(session.id.clone());
        }

        let mut title_to_save = None;
        if is_first_message {
            session.prompt = prompt.to_string();
            let title = prompt.to_string();
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
        let _ = services.db().update_session_title(session_id, title).await;
        let _ = services
            .db()
            .update_session_prompt(session_id, prompt)
            .await;
    }

    /// Appends the user reply marker line to session output.
    async fn append_reply_prompt_line(
        &self,
        services: &AppServices,
        output: &Arc<Mutex<String>>,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        session_id: &str,
        prompt: &str,
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
    }

    /// Formats one user prompt block for persisted session output.
    ///
    /// The first line uses `USER_PROMPT_PREFIX`; continuation lines use
    /// `USER_PROMPT_CONTINUATION_PREFIX` so embedded blank lines remain inside
    /// the prompt block instead of being interpreted as prompt terminators.
    fn formatted_prompt_output(prompt: &str, prepend_newline: bool) -> String {
        let prompt_lines = prompt.split('\n').collect::<Vec<_>>();
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

    /// Builds a queued command for starting or resuming a session interaction.
    ///
    /// Creates a [`SessionCommand::Run`] with [`TurnMode::Start`] for first
    /// messages and [`TurnMode::Resume`] with optional transcript replay for
    /// subsequent replies.
    fn build_session_command(input: BuildSessionCommandInput) -> SessionCommand {
        let BuildSessionCommandInput {
            is_first_message,
            prompt,
            session_model,
            session_output,
        } = input;
        let operation_id = Uuid::new_v4().to_string();
        let mode = if is_first_message {
            TurnMode::Start
        } else {
            TurnMode::Resume { session_output }
        };

        SessionCommand::Run {
            operation_id,
            mode,
            prompt,
            session_model,
        }
    }

    async fn append_reply_status_error(&self, services: &AppServices, session_id: &str) {
        let status_error = "\n[Reply Error] Session must be in review status\n".to_string();
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
        command: SessionCommand,
    ) {
        if let Err(error) = self
            .enqueue_session_command(services, persisted_session_id, command)
            .await
        {
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
    fn session_title_generation_prompt(prompt: &str) -> Result<String, String> {
        let template = SessionTitleGenerationPromptTemplate { prompt };

        template.render().map_err(|error| {
            format!("Failed to render `session_title_generation_prompt.md`: {error}")
        })
    }

    /// Parses model output into a normalized one-line session title.
    ///
    /// Accepts either a plain-text title line or a protocol-wrapped response
    /// (`{"messages":[...]}`) whose first `answer` line contains the title.
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
    /// messages.
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
            let _ = services.db().delete_session(session_id).await;
        }

        {
            let git_client = services.git_client();
            let folder = folder.to_path_buf();
            let repo_root = repo_root.to_path_buf();
            let worktree_branch = worktree_branch.to_string();
            let _ = git_client.remove_worktree(folder).await;
            let _ = git_client.delete_branch(repo_root, worktree_branch).await;
        }

        let _ = services
            .fs_client()
            .remove_dir_all(folder.to_path_buf())
            .await;
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
    ) -> Result<(), String> {
        let session = self.session_or_err(session_id)?;
        if session.status != Status::Review {
            return Err("Session must be in review to be canceled".to_string());
        }

        let branch_name = session_branch(&session.id);
        let folder = session.folder.clone();
        let handles = self.session_handles_or_err(session_id)?;
        let status = Arc::clone(&handles.status);
        let app_event_tx = services.event_sender();

        let status_updated = SessionTaskService::update_status(
            &status,
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
                services.fs_client(),
                services.git_client(),
                folder,
                branch_name,
                repo_root,
                true,
            )
            .await;
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
            let _ = git_client.remove_worktree(folder.clone()).await;

            if let Some(repo_root) = repo_root {
                let _ = git_client.delete_branch(repo_root, branch_name).await;
            }
        }

        let _ = fs_client.remove_dir_all(folder).await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use ratatui::widgets::TableState;

    use super::*;
    use crate::app::session::{RealClock, SessionDefaults};
    use crate::app::{AppServices, SessionState};
    use crate::domain::session::{
        ForgeKind, ReviewRequestState, ReviewRequestSummary, SessionHandles, SessionSize,
        SessionStats,
    };
    use crate::infra::db::{self, Database};
    use crate::infra::{app_server, forge, fs};

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
            folder: PathBuf::from("/tmp/session"),
            id: "session-id".to_string(),
            model: AgentModel::ClaudeSonnet46,
            output: output.to_string(),
            project_name: "project".to_string(),
            prompt: prompt.to_string(),
            review_request: None,
            questions: Vec::new(),
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
            .returning(|path| std::fs::read(path).map_err(|error| error.to_string()));
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

    /// Builds app services with caller-provided git and forge boundaries.
    fn test_services(
        database: Database,
        git_client: Arc<dyn git::GitClient>,
        review_request_client: Arc<dyn forge::ReviewRequestClient>,
    ) -> AppServices {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        AppServices::new(
            PathBuf::from("/tmp/agentty-tests"),
            database,
            event_tx,
            Arc::new(create_passthrough_mock_fs_client()),
            git_client,
            review_request_client,
            mock_app_server(),
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
            .returning(|_| Box::pin(async { Ok(()) }));
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
        let prompt = "first line\n\n\nafter gap";

        // Act
        let formatted_prompt = SessionManager::formatted_prompt_output(prompt, false);

        // Assert
        assert_eq!(
            formatted_prompt,
            " › first line\n   \n   \n   after gap\n\n"
        );
    }

    #[test]
    fn test_formatted_prompt_output_prepends_newline_for_replies() {
        // Arrange
        let prompt = "reply line";

        // Act
        let formatted_prompt = SessionManager::formatted_prompt_output(prompt, true);

        // Assert
        assert_eq!(formatted_prompt, "\n › reply line\n\n");
    }

    #[test]
    /// Ensures first replies persist the full prompt as the one-time title.
    fn test_prepare_reply_context_first_message_sets_title_from_prompt() {
        // Arrange
        let prompt = "Implement optimistic retry path";
        let session = test_session("", Status::New, None, "");
        let mut session_manager = session_manager_with_one_session(session);

        // Act
        let context = session_manager
            .prepare_reply_context(0, prompt, AgentModel::ClaudeSonnet46, false)
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

        // Act
        let context = session_manager
            .prepare_reply_context(0, "Follow-up prompt", AgentModel::ClaudeSonnet46, false)
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
            r#"{"messages":[{"type":"answer","text":"Polish Gemini title parsing"}]}"#;

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
    /// Ensures protocol payloads without `answer` messages do not update
    /// titles.
    fn test_parse_generated_session_title_returns_none_for_question_only_protocol_payload() {
        // Arrange
        let response_content = r#"{"messages":[{"type":"question","text":"Need confirmation?"}]}"#;

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
