//! Session lifecycle workflows and direct user actions.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use uuid::Uuid;

use super::access::SESSION_NOT_FOUND_ERROR;
use super::{SessionTaskService, session_branch, session_folder};
use crate::app::session::worker::SessionCommand;
use crate::app::settings::SettingName;
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::AgentModel;
use crate::domain::session::{SESSION_DATA_DIR, Session, Status};
use crate::infra::agent::AgentBackend;
use crate::ui::pages::session_list::grouped_session_indexes;

/// Input bag for constructing a queued session command.
struct BuildSessionCommandInput {
    folder: PathBuf,
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
    /// Returns an error if the worktree, session files, or database record
    /// cannot be created.
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

        crate::infra::agent::create_backend(session_model.kind()).setup(&folder);
        services.emit_app_event(AppEvent::RefreshSessions);

        Ok(session_id)
    }

    /// Submits the first prompt for a blank session and starts the agent.
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
        let command = if session_model.kind().supports_app_server() {
            SessionCommand::StartPromptAppServer {
                operation_id,
                prompt: prompt.clone(),
                session_model,
            }
        } else {
            let backend = crate::infra::agent::create_backend(session_model.kind());
            let command = backend.build_start_command(&folder, &prompt, session_model.as_str());
            SessionCommand::StartPrompt {
                command,
                operation_id,
                session_model,
            }
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
        let backend = crate::infra::agent::create_backend(session_model.kind());
        self.reply_with_backend(
            services,
            session_id,
            prompt,
            backend.as_ref(),
            session_model,
        )
        .await;
    }

    /// Updates and persists the model for a single session.
    ///
    /// When `LastUsedModelAsDefault` is enabled, this also persists the chosen
    /// session model as `DefaultModel`.
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

        if Self::should_persist_last_used_model_as_default(services).await? {
            services
                .db()
                .upsert_setting(SettingName::DefaultModel.as_str(), session_model.as_str())
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
    /// `DefaultModel`.
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

    /// Queues a prompt using the provided backend for a session.
    pub(crate) async fn reply_with_backend(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: &str,
        backend: &dyn AgentBackend,
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
                &status,
                &app_event_tx,
                &persisted_session_id,
                prompt,
                &title,
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

        let command = Self::build_session_command(
            backend,
            BuildSessionCommandInput {
                folder,
                is_first_message,
                prompt: prompt.to_string(),
                session_model,
                session_output,
            },
        );
        let command = match command {
            Ok(command) => command,
            Err(error) => {
                self.append_reply_command_build_error(
                    services,
                    &output,
                    &app_event_tx,
                    &persisted_session_id,
                    &error,
                )
                .await;

                return;
            }
        };
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
            && (should_replay_history || session_model.kind().supports_app_server())
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

    /// Appends a reply error line when command construction fails.
    async fn append_reply_command_build_error(
        &self,
        services: &AppServices,
        output: &Arc<Mutex<String>>,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        session_id: &str,
        error: &str,
    ) {
        let error_line = format!("\n[Reply Error] {error}\n");
        SessionTaskService::append_session_output(
            output,
            services.db(),
            app_event_tx,
            session_id,
            &error_line,
        )
        .await;
    }

    async fn persist_first_reply_metadata(
        &self,
        services: &AppServices,
        status: &Arc<Mutex<Status>>,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        session_id: &str,
        prompt: &str,
        title: &str,
    ) {
        let _ = services.db().update_session_title(session_id, title).await;
        let _ = services
            .db()
            .update_session_prompt(session_id, prompt)
            .await;
        let _ = SessionTaskService::update_status(
            status,
            services.db(),
            app_event_tx,
            session_id,
            Status::InProgress,
        )
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
    /// # Errors
    /// Returns an error when resume-command rendering fails for non-app-server
    /// models.
    fn build_session_command(
        backend: &dyn AgentBackend,
        input: BuildSessionCommandInput,
    ) -> Result<SessionCommand, String> {
        let BuildSessionCommandInput {
            folder,
            is_first_message,
            prompt,
            session_model,
            session_output,
        } = input;
        let operation_id = Uuid::new_v4().to_string();

        if session_model.kind().supports_app_server() {
            if is_first_message {
                Ok(SessionCommand::StartPromptAppServer {
                    operation_id,
                    prompt,
                    session_model,
                })
            } else {
                Ok(SessionCommand::ReplyAppServer {
                    operation_id,
                    prompt,
                    session_output,
                    session_model,
                })
            }
        } else {
            let command = if is_first_message {
                Ok(backend.build_start_command(&folder, &prompt, session_model.as_str()))
            } else {
                backend.build_resume_command(
                    &folder,
                    &prompt,
                    session_model.as_str(),
                    session_output,
                )
            }?;

            if is_first_message {
                Ok(SessionCommand::StartPrompt {
                    command,
                    operation_id,
                    session_model,
                })
            } else {
                Ok(SessionCommand::Reply {
                    command,
                    operation_id,
                    session_model,
                })
            }
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

    async fn resolve_default_session_model(&self, services: &AppServices) -> AgentModel {
        services
            .db()
            .get_setting(SettingName::DefaultModel.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|setting_value| setting_value.parse().ok())
            .unwrap_or(self.default_session_model)
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
