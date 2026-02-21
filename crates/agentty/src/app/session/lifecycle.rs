//! Session lifecycle workflows and direct user actions.

use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use uuid::Uuid;

use super::access::SESSION_NOT_FOUND_ERROR;
use super::{session_branch, session_folder};
use crate::app::session::worker::SessionCommand;
use crate::app::settings::SettingName;
use crate::app::task::TaskService;
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::AgentModel;
use crate::domain::permission::PermissionMode;
use crate::domain::session::{SESSION_DATA_DIR, Session, Status};
use crate::infra::agent::AgentBackend;
use crate::infra::git;
use crate::ui::pages::session_list::grouped_session_indexes;

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
        let session_permission_mode = self.default_session_permission_mode;

        let session_id = Uuid::new_v4().to_string();
        let folder = session_folder(services.base_path(), &session_id);
        if folder.exists() {
            return Err(format!("Session folder {session_id} already exists"));
        }

        let worktree_branch = session_branch(&session_id);
        let working_dir = projects.working_dir().to_path_buf();
        let repo_root = git::find_git_repo_root(working_dir)
            .await
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        {
            let folder = folder.clone();
            let repo_root = repo_root.clone();
            let worktree_branch = worktree_branch.clone();
            let base_branch = base_branch.to_string();
            git::create_worktree(repo_root, folder, worktree_branch, base_branch)
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

        if session_permission_mode != PermissionMode::AutoEdit
            && let Err(error) = services
                .db()
                .update_session_permission_mode(&session_id, session_permission_mode.label())
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

            return Err(format!("Failed to save session permission mode: {error}"));
        }

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
        let (folder, permission_mode, persisted_session_id, session_model, title) = {
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
                session.permission_mode,
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
        TaskService::append_session_output(
            &output,
            services.db(),
            &app_event_tx,
            &persisted_session_id,
            &initial_output,
        )
        .await;

        let _ = TaskService::update_status(
            &status,
            services.db(),
            &app_event_tx,
            &persisted_session_id,
            Status::InProgress,
        )
        .await;

        let is_initial_plan_prompt = permission_mode == PermissionMode::Plan;
        let command = crate::infra::agent::create_backend(session_model.kind()).build_start_command(
            &folder,
            &prompt,
            session_model.as_str(),
            permission_mode,
            is_initial_plan_prompt,
        );
        let operation_id = Uuid::new_v4().to_string();
        self.enqueue_session_command(
            services,
            &persisted_session_id,
            SessionCommand::StartPrompt {
                command,
                operation_id,
                permission_mode,
                session_model,
            },
        )
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
        services.emit_app_event(AppEvent::SessionModelUpdated {
            session_id: session_id.to_string(),
            session_model,
        });

        if model_changed {
            self.pending_history_replay.insert(session_id.to_string());
        }

        Ok(())
    }

    /// Toggles the permission mode for a session and persists the change.
    ///
    /// # Errors
    /// Returns an error if the session is not found or persistence fails.
    pub async fn toggle_session_permission_mode(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), String> {
        let session = self.session_or_err(session_id)?;
        let permission_mode = session.permission_mode.toggle();
        services
            .db()
            .update_session_permission_mode(session_id, permission_mode.label())
            .await?;
        services.emit_app_event(AppEvent::SessionPermissionModeUpdated {
            permission_mode,
            session_id: session_id.to_string(),
        });

        Ok(())
    }

    /// Sets the permission mode for a session and persists the change.
    ///
    /// # Errors
    /// Returns an error if the session is not found or persistence fails.
    pub async fn set_session_permission_mode(
        &mut self,
        services: &AppServices,
        session_id: &str,
        permission_mode: PermissionMode,
    ) -> Result<(), String> {
        let _ = self.session_or_err(session_id)?;
        services
            .db()
            .update_session_permission_mode(session_id, permission_mode.label())
            .await?;
        services.emit_app_event(AppEvent::SessionPermissionModeUpdated {
            permission_mode,
            session_id: session_id.to_string(),
        });

        Ok(())
    }

    /// Clears a session's chat history and resets it to a fresh state.
    ///
    /// Preserves the session identity, worktree, agent, model, and accumulated
    /// token statistics. Resets output, prompt, title, and status so the next
    /// prompt starts the agent without `--resume`.
    ///
    /// # Errors
    /// Returns an error if the session is not found or the database update
    /// fails.
    pub async fn clear_session_history(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), String> {
        let _ = self.session_index_or_err(session_id)?;

        services.db().clear_session_history(session_id).await?;
        services.emit_app_event(AppEvent::SessionHistoryCleared {
            session_id: session_id.to_string(),
        });
        self.update_sessions_metadata_cache(services).await;

        Ok(())
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
    /// reload through [`AppEvent::RefreshSessions`].
    pub async fn delete_selected_session(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        let Some(index) = self.table_state.selected() else {
            return;
        };
        if index >= self.sessions.len() {
            return;
        }

        let session = self.sessions.remove(index);
        self.handles.remove(&session.id);
        self.pending_history_replay.remove(&session.id);

        let _ = services
            .db()
            .request_cancel_for_session_operations(&session.id)
            .await;
        self.clear_session_worker(&session.id);
        let _ = services.db().delete_session(&session.id).await;

        if projects.has_git_branch() {
            let branch_name = session_branch(&session.id);
            let working_dir = projects.working_dir().to_path_buf();

            if let Some(repo_root) = git::find_git_repo_root(working_dir).await {
                let folder = session.folder.clone();
                let _ = git::remove_worktree(folder).await;
                let _ = git::delete_branch(repo_root, branch_name).await;
            } else {
                let folder = session.folder.clone();
                let _ = git::remove_worktree(folder).await;
            }
        }

        let _ = tokio::fs::remove_dir_all(&session.folder).await;
        services.emit_app_event(AppEvent::RefreshSessions);
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
        let permission_mode = self
            .sessions
            .get(session_index)
            .map_or(PermissionMode::default(), |session| session.permission_mode);
        let should_replay_history = self.pending_history_replay.contains(session_id);
        let mut blocked_session_id = None;
        let reply_context = {
            let Some(session) = self.sessions.get_mut(session_index) else {
                return;
            };

            let is_first_message = session.prompt.is_empty();
            let allowed = session.status == Status::Review
                || (is_first_message && session.status == Status::New);
            if allowed {
                let mut title_to_save = None;
                if is_first_message {
                    session.prompt = prompt.to_string();
                    let title = prompt.to_string();
                    session.title = Some(title.clone());
                    title_to_save = Some(title);
                }

                Some((
                    session.folder.clone(),
                    if should_replay_history && !is_first_message {
                        Some(session.output.clone())
                    } else {
                        None
                    },
                    is_first_message,
                    session.id.clone(),
                    title_to_save,
                ))
            } else {
                blocked_session_id = Some(session.id.clone());

                None
            }
        };
        if let Some(session_id) = blocked_session_id {
            self.append_reply_status_error(services, &session_id).await;

            return;
        }
        let Some((folder, session_output, is_first_message, persisted_session_id, title_to_save)) =
            reply_context
        else {
            return;
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

        let reply_line = format!("\n › {prompt}\n\n");
        TaskService::append_session_output(
            &output,
            services.db(),
            &app_event_tx,
            &persisted_session_id,
            &reply_line,
        )
        .await;

        let command = Self::build_session_command(
            backend,
            &folder,
            prompt,
            session_model,
            permission_mode,
            is_first_message,
            session_output,
        );
        self.enqueue_reply_command(
            services,
            &output,
            &app_event_tx,
            &persisted_session_id,
            command,
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
        let _ = TaskService::update_status(
            status,
            services.db(),
            app_event_tx,
            session_id,
            Status::InProgress,
        )
        .await;
    }

    fn build_session_command(
        backend: &dyn AgentBackend,
        folder: &Path,
        prompt: &str,
        session_model: AgentModel,
        permission_mode: PermissionMode,
        is_first_message: bool,
        session_output: Option<String>,
    ) -> SessionCommand {
        let operation_id = Uuid::new_v4().to_string();
        let is_initial_plan_prompt = is_first_message && permission_mode == PermissionMode::Plan;

        if is_first_message {
            SessionCommand::StartPrompt {
                command: backend.build_start_command(
                    folder,
                    prompt,
                    session_model.as_str(),
                    permission_mode,
                    is_initial_plan_prompt,
                ),
                operation_id,
                permission_mode,
                session_model,
            }
        } else {
            SessionCommand::Reply {
                command: backend.build_resume_command(
                    folder,
                    prompt,
                    session_model.as_str(),
                    permission_mode,
                    is_initial_plan_prompt,
                    session_output,
                ),
                operation_id,
                permission_mode,
                session_model,
            }
        }
    }

    async fn append_reply_status_error(&self, services: &AppServices, session_id: &str) {
        let status_error = "\n[Reply Error] Session must be in review status\n".to_string();
        let Ok(handles) = self.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = services.event_sender();

        TaskService::append_session_output(
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
            TaskService::append_session_output(
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
            let folder = folder.to_path_buf();
            let repo_root = repo_root.to_path_buf();
            let worktree_branch = worktree_branch.to_string();
            let _ = git::remove_worktree(folder).await;
            let _ = git::delete_branch(repo_root, worktree_branch).await;
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

        TaskService::append_session_output(
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

        let _ = TaskService::update_status(
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
