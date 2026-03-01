//! Session lifecycle workflows and direct user actions.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use askama::Template;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::access::SESSION_NOT_FOUND_ERROR;
use super::{SessionTaskService, session_branch, session_folder};
use crate::app::session::worker::SessionCommand;
use crate::app::settings::SettingName;
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::AgentModel;
use crate::domain::session::{SESSION_DATA_DIR, Session, Status};
use crate::infra::app_server::AppServerTurnRequest;
use crate::infra::channel::TurnMode;
use crate::ui::pages::session_list::grouped_session_indexes;

/// Maximum stored length for generated session titles.
const GENERATED_SESSION_TITLE_MAX_CHARACTERS: usize = 72;

/// Input bag for constructing a queued session command.
struct BuildSessionCommandInput {
    is_first_message: bool,
    prompt: String,
    session_model: AgentModel,
    session_output: Option<String>,
}

/// Intermediate values captured while preparing a session reply.
type ReplyContext = (PathBuf, Option<String>, bool, String, Option<String>);

/// Cleanup payload for a deleted session's git and filesystem resources.
struct DeletedSessionCleanup {
    branch_name: String,
    folder: PathBuf,
    has_git_branch: bool,
    working_dir: PathBuf,
}

/// JSON response shape used by model-generated session titles.
#[derive(serde::Deserialize)]
struct GeneratedSessionTitleResponse {
    title: String,
}

/// Askama view model for rendering title-generation instruction prompts.
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
        let grouped_indexes = grouped_session_indexes(&self.sessions);
        if grouped_indexes.is_empty() {
            return;
        }

        let index = match self.table_state.selected().and_then(|selected_index| {
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

        self.table_state.select(Some(grouped_indexes[index]));
    }

    /// Moves selection to the previous selectable session in grouped list
    /// order.
    ///
    /// Group header rows are non-selectable and are skipped by design.
    pub fn previous(&mut self) {
        let grouped_indexes = grouped_session_indexes(&self.sessions);
        if grouped_indexes.is_empty() {
            return;
        }

        let index = match self.table_state.selected().and_then(|selected_index| {
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

        self.table_state.select(Some(grouped_indexes[index]));
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
        let session_model = self.resolve_default_session_model(services).await;
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
        if let Err(error) = tokio::fs::create_dir_all(&data_dir).await {
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

        if let Err(error) = crate::infra::agent::create_backend(session_model.kind()).setup(&folder)
        {
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
    /// After enqueueing the session command, this also schedules a detached
    /// fast-model task that can rewrite the initial prompt-based title with a
    /// concise generated title.
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
        let (folder, persisted_session_id, session_model, title) = {
            let session = self
                .sessions
                .get_mut(session_index)
                .ok_or_else(|| SESSION_NOT_FOUND_ERROR.to_string())?;

            session.prompt.clone_from(&prompt);

            let title = prompt.clone();
            session.title = Some(title.clone());
            let session_model = session.model;

            (
                session.folder.clone(),
                session.id.clone(),
                session_model,
                title,
            )
        };

        let handles = self.session_handles_or_err(&persisted_session_id)?;
        let output = Arc::clone(&handles.output);
        let status = Arc::clone(&handles.status);
        let app_event_tx = services.event_sender();

        let _ = services
            .db()
            .update_session_title(&persisted_session_id, &title)
            .await;
        let _ = services
            .db()
            .update_session_prompt(&persisted_session_id, &prompt)
            .await;

        let title_model =
            crate::app::settings::load_default_fast_model_setting(services, session_model).await;
        Self::spawn_session_title_refresh_task(
            services,
            &persisted_session_id,
            folder.as_path(),
            &prompt,
            title_model,
        );

        let initial_output = format!(" › {prompt}\n\n");
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

    /// Sends SIGINT to the running agent process and cancels queued operations.
    ///
    /// # Errors
    /// Returns an error if the session is not found, not in `InProgress`
    /// status, or the agent process is not running.
    pub async fn stop_session(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), String> {
        let session = self.session_or_err(session_id)?;
        if session.status != Status::InProgress {
            return Err("Session is not in progress".to_string());
        }

        let handles = self.session_handles_or_err(session_id)?;
        let pid = handles
            .child_pid
            .lock()
            .ok()
            .and_then(|guard| *guard)
            .ok_or_else(|| "No running agent process".to_string())?;

        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(
                i32::try_from(pid).map_err(|error| format!("Invalid PID: {error}"))?,
            ),
            nix::sys::signal::Signal::SIGINT,
        )
        .map_err(|error| format!("Failed to send SIGINT: {error}"))?;

        let _ = services
            .db()
            .request_cancel_for_session_operations(session_id)
            .await;

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
        backend: std::sync::Arc<dyn crate::infra::agent::AgentBackend>,
        session_model: AgentModel,
    ) {
        let channel: std::sync::Arc<dyn crate::infra::channel::AgentChannel> =
            std::sync::Arc::new(crate::infra::channel::cli::CliAgentChannel::with_backend(
                backend,
                session_model.kind(),
            ));
        self.test_agent_channels
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
    /// with stale ids.
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
        }

        if Self::should_persist_last_used_model_as_default(services).await? {
            services
                .db()
                .upsert_setting(
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
            self.pending_history_replay.insert(session_id.to_string());
        }

        Ok(())
    }

    /// Returns whether session model switches should also persist
    /// `DefaultSmartModel`.
    async fn should_persist_last_used_model_as_default(
        services: &AppServices,
    ) -> Result<bool, String> {
        let should_persist = services
            .db()
            .get_setting(SettingName::LastUsedModelAsDefault.as_str())
            .await?
            .and_then(|setting_value| setting_value.parse::<bool>().ok())
            .unwrap_or(false);

        Ok(should_persist)
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.table_state
            .selected()
            .and_then(|index| self.sessions.get(index))
    }

    /// Returns the session identifier for the given list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<String> {
        self.sessions
            .get(session_index)
            .map(|session| session.id.clone())
    }

    /// Resolves a stable session identifier to the current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.id == session_id)
    }

    /// Deletes the currently selected session and cleans related resources.
    ///
    /// After persistence and filesystem cleanup, this triggers a full list
    /// reload through [`AppEvent::RefreshSessions`]. When deleting a session,
    /// the all-time longest duration setting is updated if needed.
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

        Self::cleanup_deleted_session_resources(services.git_client(), cleanup).await;
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

        let git_client = services.git_client();
        tokio::spawn(async move {
            SessionManager::cleanup_deleted_session_resources(git_client, cleanup).await;
        });
    }

    /// Persists the deleted session duration when it exceeds the current
    /// all-time maximum.
    async fn persist_deleted_session_duration(
        &mut self,
        services: &AppServices,
        session: &Session,
    ) {
        let duration_seconds = session.updated_at.saturating_sub(session.created_at).max(0);
        let duration_seconds = u64::try_from(duration_seconds).unwrap_or_default();
        if duration_seconds <= self.longest_session_duration_seconds {
            return;
        }

        let updated = services
            .db()
            .upsert_setting(
                SettingName::LongestSessionDurationSeconds.as_str(),
                &duration_seconds.to_string(),
            )
            .await;
        if updated.is_err() {
            return;
        }

        self.longest_session_duration_seconds = duration_seconds;
    }

    /// Removes the selected session from app state and persistence, returning
    /// deferred cleanup instructions for git and filesystem resources.
    async fn remove_selected_session_from_state_and_db(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Option<DeletedSessionCleanup> {
        let selected_index = self.table_state.selected()?;
        if selected_index >= self.sessions.len() {
            return None;
        }

        let session = self.sessions.remove(selected_index);
        self.handles.remove(&session.id);
        self.pending_history_replay.remove(&session.id);
        self.persist_deleted_session_duration(services, &session)
            .await;

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
        git_client: Arc<dyn crate::infra::git::GitClient>,
        cleanup: DeletedSessionCleanup,
    ) {
        if cleanup.has_git_branch {
            if let Some(repo_root) = git_client.find_git_repo_root(cleanup.working_dir).await {
                let _ = git_client.remove_worktree(cleanup.folder.clone()).await;
                let _ = git_client
                    .delete_branch(repo_root, cleanup.branch_name)
                    .await;
            } else {
                let _ = git_client.remove_worktree(cleanup.folder.clone()).await;
            }
        }

        let _ = tokio::fs::remove_dir_all(cleanup.folder).await;
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
        let should_replay_history = self.pending_history_replay.contains(session_id);
        let (folder, session_output, is_first_message, persisted_session_id, title_to_save) =
            match self.prepare_reply_context(
                session_index,
                prompt,
                session_model,
                should_replay_history,
            ) {
                Ok(Some(reply_context)) => reply_context,
                Ok(None) => return,
                Err(blocked_session_id) => {
                    self.append_reply_status_error(services, &blocked_session_id)
                        .await;

                    return;
                }
            };

        if should_replay_history {
            self.pending_history_replay.remove(&persisted_session_id);
        }

        let app_event_tx = services.event_sender();

        let Ok(handles) = self.session_handles_or_err(&persisted_session_id) else {
            return;
        };

        let output = Arc::clone(&handles.output);
        let status = Arc::clone(&handles.status);

        if let Some(title) = title_to_save {
            self.persist_first_reply_metadata(
                services,
                &persisted_session_id,
                prompt,
                &title,
                folder.as_path(),
                session_model,
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
            prompt: prompt.to_string(),
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
        let Some(session) = self.sessions.get_mut(session_index) else {
            return Ok(None);
        };

        let is_first_message = session.prompt.is_empty();
        let allowed =
            session.status == Status::Review || (is_first_message && session.status == Status::New);
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
                || crate::infra::agent::transport_mode(session_model.kind()).uses_app_server())
        {
            Some(session.output.clone())
        } else {
            None
        };

        Ok(Some((
            session.folder.clone(),
            session_output,
            is_first_message,
            session.id.clone(),
            title_to_save,
        )))
    }

    /// Persists first-message prompt/title metadata before queueing execution.
    ///
    /// This writes the initial prompt/title and schedules asynchronous title
    /// refinement using the configured fast model.
    async fn persist_first_reply_metadata(
        &self,
        services: &AppServices,
        session_id: &str,
        prompt: &str,
        title: &str,
        folder: &Path,
        session_model: AgentModel,
    ) {
        let _ = services.db().update_session_title(session_id, title).await;
        let _ = services
            .db()
            .update_session_prompt(session_id, prompt)
            .await;

        let title_model =
            crate::app::settings::load_default_fast_model_setting(services, session_model).await;
        Self::spawn_session_title_refresh_task(services, session_id, folder, prompt, title_model);
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
        let reply_line = format!("\n › {prompt}\n\n");
        SessionTaskService::append_session_output(
            output,
            services.db(),
            app_event_tx,
            session_id,
            &reply_line,
        )
        .await;
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

    /// Spawns a detached fast-model turn that refines the first-message title.
    ///
    /// The generated title is persisted when parsing succeeds, then a
    /// `RefreshSessions` event is emitted so list-mode snapshots pick up the
    /// new title.
    fn spawn_session_title_refresh_task(
        services: &AppServices,
        session_id: &str,
        folder: &Path,
        prompt: &str,
        session_model: AgentModel,
    ) {
        if !crate::infra::agent::transport_mode(session_model.kind()).uses_app_server() {
            return;
        }

        let app_server_client = services.app_server_client();
        let app_event_tx = services.event_sender();
        let db = services.db().clone();
        let folder = folder.to_path_buf();
        let prompt = prompt.to_string();
        let persisted_session_id = session_id.to_string();

        tokio::spawn(async move {
            let Ok(title_generation_prompt) =
                SessionManager::session_title_generation_prompt(&prompt)
            else {
                return;
            };
            let title_task_session_id = format!("session-title-{persisted_session_id}");
            let request = AppServerTurnRequest {
                live_session_output: None,
                folder,
                model: session_model.as_str().to_string(),
                prompt: title_generation_prompt,
                provider_conversation_id: None,
                session_id: title_task_session_id.clone(),
                session_output: None,
            };
            let (stream_tx, _stream_rx) = mpsc::unbounded_channel();
            let turn_result = app_server_client.run_turn(request, stream_tx).await;
            app_server_client
                .shutdown_session(title_task_session_id)
                .await;

            let Ok(turn_response) = turn_result else {
                return;
            };
            let Some(generated_title) =
                SessionManager::parse_generated_session_title(&turn_response.assistant_message)
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
    fn parse_generated_session_title(content: &str) -> Option<String> {
        let content = content.trim();
        if content.is_empty() {
            return None;
        }

        if let Ok(parsed_response) = serde_json::from_str::<GeneratedSessionTitleResponse>(content)
        {
            return Self::normalize_generated_session_title(&parsed_response.title);
        }

        if let Some(json) = Self::embedded_json_object(content)
            && let Ok(parsed_response) = serde_json::from_str::<GeneratedSessionTitleResponse>(json)
        {
            return Self::normalize_generated_session_title(&parsed_response.title);
        }

        let first_line = content
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(content);

        Self::normalize_generated_session_title(first_line)
    }

    /// Returns the first embedded JSON object slice in a mixed model response.
    fn embedded_json_object(content: &str) -> Option<&str> {
        let json_start = content.find('{')?;
        let json_end = content.rfind('}')?;
        if json_end < json_start {
            return None;
        }

        Some(&content[json_start..=json_end])
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

    async fn resolve_default_session_model(&self, services: &AppServices) -> AgentModel {
        crate::app::settings::load_default_smart_model_setting(services, self.default_session_model)
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

        let _ = tokio::fs::remove_dir_all(folder).await;
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

    /// Cancels a session that is currently in review.
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

        let handles = self.session_handles_or_err(session_id)?;
        let status = Arc::clone(&handles.status);
        let app_event_tx = services.event_sender();

        let _ = SessionTaskService::update_status(
            &status,
            services.db(),
            &app_event_tx,
            session_id,
            Status::Canceled,
        )
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Ensures title-generation prompt rendering includes commit-style,
    /// high-level title guidance and the original user request.
    fn test_session_title_generation_prompt_includes_request() {
        // Arrange
        let request_prompt = "Refactor session lifecycle updates";

        // Act
        let title_prompt = SessionManager::session_title_generation_prompt(request_prompt)
            .expect("title generation prompt should render");

        // Assert
        assert!(title_prompt.contains("Generate a concise, commit-style title"));
        assert!(title_prompt.contains("Keep it high-level and intent-focused."));
        assert!(title_prompt.contains("Do not include long file names"));
        assert!(title_prompt.contains(request_prompt));
    }

    #[test]
    fn test_parse_generated_session_title_prefers_json_title() {
        // Arrange
        let response_content = r#"{"title":"Refine session startup flow"}"#;

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(
            parsed_title,
            Some("Refine session startup flow".to_string())
        );
    }

    #[test]
    fn test_parse_generated_session_title_reads_embedded_json_object() {
        // Arrange
        let response_content = "answer:\n{\"title\":\"Simplify prompt submit flow\"}\n";

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(
            parsed_title,
            Some("Simplify prompt submit flow".to_string())
        );
    }

    #[test]
    fn test_parse_generated_session_title_falls_back_to_first_line() {
        // Arrange
        let response_content = "Title: \"Polish merge queue behavior\"\nextra details";

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(
            parsed_title,
            Some("Polish merge queue behavior".to_string())
        );
    }

    #[test]
    fn test_parse_generated_session_title_returns_none_for_blank_response() {
        // Arrange
        let response_content = "   \n\n";

        // Act
        let parsed_title = SessionManager::parse_generated_session_title(response_content);

        // Assert
        assert_eq!(parsed_title, None);
    }
}
