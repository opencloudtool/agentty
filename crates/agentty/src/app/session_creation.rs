//! Prepared session creation and foreground completion routing.

use ag_session::{CreateSessionRequest, SessionError, SessionId};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::app::session::{SessionCreationKind, SessionCreationSettings};
use crate::app::{App, AppError, AppEvent, SessionManager};
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::SessionPreparationState;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState};

/// Creation inputs captured before external work releases the foreground.
pub(super) enum PreparedSessionCreation {
    Materialized {
        base_branch: String,
        creation_kind: SessionCreationKind,
        project_id: i64,
        settings: SessionCreationSettings,
    },
    Persisted(String),
}

/// Recipient and project captured when a creation request is accepted.
pub(crate) struct PendingSessionCreation {
    project_id: i64,
    response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    session_id: Option<String>,
}

impl App {
    /// Opens the composer after reserving metadata; checkout runs
    /// independently.
    pub(crate) async fn start_session_creation(
        &mut self,
        request: CreateSessionRequest,
        response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    ) {
        let request_id = Uuid::new_v4().to_string();
        let interactive = response_tx.is_none();
        self.pending_session_creations.insert(
            request_id.clone(),
            PendingSessionCreation {
                project_id: request.project_id,
                response_tx,
                session_id: None,
            },
        );
        let result = self.prepare_api_session_creation(request).await;
        let session_id = match result {
            Ok(PreparedSessionCreation::Materialized {
                base_branch,
                creation_kind,
                project_id,
                settings,
            }) => {
                match SessionManager::reserve_session(
                    &self.services,
                    project_id,
                    &base_branch,
                    settings,
                    creation_kind,
                )
                .await
                {
                    Ok(session_id) => session_id,
                    Err(error) => {
                        self.complete_session_creation(
                            &request_id,
                            Err(SessionError::Operation(error.to_string())),
                        )
                        .await;
                        return;
                    }
                }
            }
            Ok(PreparedSessionCreation::Persisted(session_id)) => {
                self.finish_api_session_creation(&session_id).await;
                if interactive {
                    self.open_created_session_composer(&session_id);
                }
                self.complete_session_creation(&request_id, Ok(session_id))
                    .await;
                return;
            }
            Err(error) => {
                self.complete_session_creation(&request_id, Err(error))
                    .await;
                return;
            }
        };
        self.finish_api_session_creation(&session_id).await;
        if interactive {
            self.open_created_session_composer(&session_id);
        }
        if let Some(pending) = self.pending_session_creations.get_mut(&request_id) {
            pending.session_id = Some(session_id.clone());
        }
        self.spawn_workspace_preparation(request_id, session_id);
    }

    /// Selects the newly reserved conversation once, before accepting input.
    fn open_created_session_composer(&mut self, session_id: &str) {
        let index = self
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == session_id);
        self.sessions.select_session_index(index);
        self.mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            focus: ChatFocus::Input,
            history_state: PromptHistoryState::new(Vec::new()),
            slash_state: self.prompt_slash_state(),
            session_id: session_id.into(),
            input: InputState::default(),
            scroll_offset: None,
        };
        self.mark_dirty();
    }

    /// Runs filesystem work without borrowing the foreground application.
    fn spawn_workspace_preparation(&self, request_id: String, session_id: String) {
        let services = self.services.clone();
        let completion_request_id = request_id.clone();
        let task = tokio::spawn(async move {
            let result = SessionManager::prepare_reserved_session(&services, &session_id)
                .await
                .map(|()| session_id)
                .map_err(|error| error.to_string());
            services.emit_app_event(AppEvent::SessionCreationCompleted {
                request_id: completion_request_id,
                result,
            });
        });
        self.services.track_session_creation_task(request_id, task);
    }

    /// Applies worker results without replacing a composer or navigation state.
    pub(crate) async fn complete_session_creations(
        &mut self,
        results: Vec<(String, Result<String, String>)>,
    ) {
        for (request_id, result) in results {
            self.complete_session_creation(&request_id, result.map_err(SessionError::Operation))
                .await;
        }
    }

    /// Returns the reserved identity to API callers even if setup failed,
    /// allowing them to link and retry that session, and dispatches ready
    /// turns.
    pub(crate) async fn complete_session_creation(
        &mut self,
        request_id: &str,
        result: Result<String, SessionError>,
    ) {
        self.services.finish_session_creation_task(request_id).await;
        let Some(pending) = self.pending_session_creations.remove(request_id) else {
            return;
        };
        if let Some(session_id) = &pending.session_id {
            self.refresh_workspace_preparation(session_id).await;
            if result.is_ok() && self.sessions.session_for_id(session_id).is_some() {
                self.dispatch_prepared_prompt(session_id).await;
            }
        }
        if let Some(response_tx) = pending.response_tx {
            let result = pending.session_id.map_or(result, Ok);
            let _ = response_tx.send(result.map(SessionId::from));
        } else if pending.session_id.is_none()
            && pending.project_id == self.active_project_id()
            && let Err(error) = result
        {
            self.mode = AppMode::SyncBlockedPopup {
                default_branch: None,
                is_loading: false,
                message: error.to_string(),
                project_name: None,
                title: "Session creation unavailable".to_string(),
            };
        }
        self.mark_dirty();
    }

    /// Reflects durable setup state without reloading unrelated sessions.
    pub(crate) async fn refresh_workspace_preparation(&mut self, session_id: &str) {
        if let Ok(preparation) = self
            .services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await
        {
            let ready = preparation
                .as_ref()
                .is_some_and(|row| row.state == SessionPreparationState::Ready);
            if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
                SessionManager::apply_workspace_preparation(session, preparation.as_ref());
            }
            self.sessions
                .set_session_worktree_available(session_id, ready);
        }
        self.mark_dirty();
    }

    /// Saves an early submission once; a second distinct prompt stays in its
    /// composer.
    pub(crate) async fn queue_preparation_prompt(
        &mut self,
        session_id: &str,
        prompt: &TurnPrompt,
    ) -> Result<bool, AppError> {
        let Some(preparation) = self
            .services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
        else {
            return Ok(false);
        };
        if preparation.state == SessionPreparationState::Canceled {
            return Err(AppError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }
        if preparation.state == SessionPreparationState::Ready && preparation.prompt.is_none() {
            return Ok(false);
        }
        let prompt_json =
            serde_json::to_string(prompt).map_err(|error| AppError::Workflow(error.to_string()))?;
        if let Some(saved) = &preparation.prompt {
            if saved != &prompt_json {
                return Err(AppError::Workflow(
                    "A first prompt is already saved. Wait for it to start before sending another."
                        .to_string(),
                ));
            }
        } else if !self
            .services
            .db()
            .sessions()
            .save_preparation_prompt(session_id, &prompt_json)
            .await?
        {
            return Err(AppError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }
        if preparation.state == SessionPreparationState::Failed {
            self.retry_workspace_preparation(session_id).await?;
        }
        self.refresh_workspace_preparation(session_id).await;

        Ok(true)
    }

    /// Retries failed setup or starts lazy draft preparation, retaining its
    /// prompt.
    pub(crate) async fn retry_workspace_preparation(
        &mut self,
        session_id: &str,
    ) -> Result<(), AppError> {
        if !self.sessions.can_retry_workspace_preparation(session_id) {
            return Err(AppError::Workflow(
                "The saved first prompt is already queued or running".to_string(),
            ));
        }
        if self
            .pending_session_creations
            .values()
            .any(|pending| pending.session_id.as_deref() == Some(session_id))
        {
            return Ok(());
        }
        let project_id = self
            .services
            .db()
            .sessions()
            .load_session_project_id(session_id)
            .await?
            .ok_or(AppError::Workflow("Session project is missing".to_string()))?;
        self.ensure_project_checkout_available(project_id)?;
        if !self
            .services
            .db()
            .sessions()
            .update_session_preparation(session_id, SessionPreparationState::Preparing, None)
            .await?
        {
            return Err(AppError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }
        let request_id = Uuid::new_v4().to_string();
        self.pending_session_creations.insert(
            request_id.clone(),
            PendingSessionCreation {
                project_id,
                response_tx: None,
                session_id: Some(session_id.to_string()),
            },
        );
        self.spawn_workspace_preparation(request_id, session_id.to_string());
        self.refresh_workspace_preparation(session_id).await;

        Ok(())
    }

    /// Resumes ready prompts when their owning project becomes active again.
    pub(crate) async fn resume_ready_workspace_prompts(&mut self) {
        let Ok(preparations) = self
            .services
            .db()
            .sessions()
            .load_session_preparations(self.active_project_id())
            .await
        else {
            return;
        };
        for preparation in preparations {
            if preparation.state == SessionPreparationState::Ready
                && preparation.prompt.is_some()
                && self
                    .sessions
                    .session_for_id(&preparation.session_id)
                    .is_some()
            {
                self.dispatch_prepared_prompt(&preparation.session_id).await;
            }
        }
    }

    /// Hands off a saved prompt only after readiness, then acknowledges
    /// persistence.
    async fn dispatch_prepared_prompt(&mut self, session_id: &str) {
        let result = self.run_prepared_prompt(session_id).await;
        if let Err(error) = result {
            let _ = self
                .services
                .db()
                .sessions()
                .update_session_preparation(
                    session_id,
                    SessionPreparationState::Failed,
                    Some(&error.to_string()),
                )
                .await;
        }
        self.refresh_workspace_preparation(session_id).await;
    }

    /// Reuses the ordinary first-turn and fork-reply paths after workspace
    /// setup.
    async fn run_prepared_prompt(&mut self, session_id: &str) -> Result<(), AppError> {
        let Some(preparation) = self
            .services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
        else {
            return Ok(());
        };
        if preparation.state != SessionPreparationState::Ready {
            return Ok(());
        }
        let Some(prompt_json) = preparation.prompt else {
            return Ok(());
        };
        let prompt = serde_json::from_str::<TurnPrompt>(&prompt_json)
            .map_err(|error| AppError::Workflow(error.to_string()))?;
        self.services
            .db()
            .sessions()
            .reclaim_preparation_prompt_operation(session_id)
            .await?;
        if self
            .recover_accepted_preparation_prompt(session_id, &prompt)
            .await?
        {
            return Ok(());
        }
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(AppError::Workflow("Session is not loaded".to_string()))?;
        let was_draft = session.is_draft_session();
        if was_draft && !self.sessions.can_start_staged_session(session_id) {
            return Err(AppError::Workflow(
                "The parent stack is no longer ready to start this session".to_string(),
            ));
        }
        if session.status == Status::Draft {
            self.sessions
                .start_session(&self.services, session_id, prompt)
                .await?;
        } else if session.status == Status::Review {
            if !self
                .sessions
                .reply_to_coordinator_message(
                    &self.services,
                    session_id,
                    format!("workspace:{session_id}"),
                    true,
                    prompt,
                )
                .await
            {
                return Err(AppError::Workflow(
                    "Could not start the saved prompt".to_string(),
                ));
            }
        } else {
            return Err(AppError::Workflow(
                "Check the existing turn before retrying the saved prompt".to_string(),
            ));
        }
        self.sessions
            .state_mut()
            .sync_session_from_handle(session_id);
        if was_draft {
            self.sessions
                .clear_started_draft_attachments(&self.services, session_id)
                .await;
        }
        Ok(())
    }

    /// Restores a saved prompt after an interrupted handoff without replaying
    /// it.
    async fn recover_accepted_preparation_prompt(
        &mut self,
        session_id: &str,
        prompt: &TurnPrompt,
    ) -> Result<bool, AppError> {
        if let Some(status) = self
            .services
            .db()
            .sessions()
            .preparation_prompt_operation_status(session_id)
            .await?
        {
            // A live queued command still owns its gate. Keep its saved
            // payload for restart recovery without queueing a duplicate.
            if status == "queued" {
                return Ok(true);
            }
            let text = prompt.transcript_text();
            let messages = self
                .services
                .db()
                .sessions()
                .load_session_messages(session_id)
                .await?;
            if !messages
                .iter()
                .any(|message| message.kind == "user_prompt" && message.content == text)
            {
                self.services
                    .db()
                    .sessions()
                    .append_session_message(
                        session_id,
                        ag_session::SessionMessageKind::UserPrompt,
                        &text,
                    )
                    .await?;
            }
            self.services
                .db()
                .sessions()
                .clear_preparation_prompt(session_id)
                .await?;
            self.sessions
                .load_session_detail_into_state(self.services.db(), session_id)
                .await;
            self.append_output_for_session(
                session_id,
                "Previous submission recovered. Check its turn outcome before sending again.",
            )
            .await;

            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ag_git::{GitError, MockGitClient};
    use ag_session::CreateSessionMode;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::Notify;

    use super::*;
    use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
    use crate::app::{AppServiceDeps, AppServices, SessionRuntimeCommand};
    use crate::runtime::PresentationState;

    /// Pauses worktree setup at its injected Git boundary until released.
    async fn delayed_creation_app(repo_lookups: usize) -> (App, tempfile::TempDir, Arc<Notify>) {
        delayed_creation_app_with_attempts(repo_lookups, 1).await
    }

    /// Fails each released checkout attempt while retaining one app and store.
    async fn delayed_creation_app_with_attempts(
        repo_lookups: usize,
        attempts: usize,
    ) -> (App, tempfile::TempDir, Arc<Notify>) {
        let (mut app, directory) = crate::test_support::new_git_test_app().await;
        let release = Arc::new(Notify::new());
        let mut git = MockGitClient::new();
        git.expect_find_git_repo_root()
            .times(repo_lookups)
            .returning(|path| Box::pin(async move { Some(path) }));
        git.expect_create_worktree().times(attempts).returning({
            let release = Arc::clone(&release);
            move |_, _, _, _| {
                let release = Arc::clone(&release);

                Box::pin(async move {
                    release.notified().await;

                    Err(GitError::CommandFailed {
                        command: "git worktree add".to_string(),
                        stderr: "delayed creation failed".to_string(),
                    })
                })
            }
        });
        app.services = AppServices::new_with_agent_clis(
            app.services.base_path().to_path_buf(),
            app.services.clock(),
            app.services.event_sender(),
            AppServiceDeps {
                app_server_client_override: app.services.app_server_client_override(),
                available_agent_kinds: app.services.available_agent_kinds(),
                clipboard_image_client_override: Some(app.services.clipboard_image_client()),
                fs_client: app.services.fs_client(),
                git_client: Arc::new(git),
                one_shot_client_override: Some(app.services.one_shot_client()),
                personality_catalog_client_override: Some(
                    app.services.personality_catalog_client(),
                ),
                repositories: app.services.db().clone(),
                review_request_client: app.services.review_request_client(),
            },
            app.services.available_agent_clis(),
        );

        (app, directory, release)
    }

    fn regular_request(app: &App) -> CreateSessionRequest {
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id: app.active_project_id(),
        }
    }

    async fn finish_creation(app: &mut App) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !app.pending_session_creations.is_empty() {
                let event = app.next_app_event().await.expect("creation completion");
                app.apply_app_events(event).await;
            }
        })
        .await
        .expect("creation must finish");
    }

    #[tokio::test]
    async fn saved_submission_routes_preserve_the_first_prompt() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app(1).await;
        let request = regular_request(&app);
        app.start_session_creation(request, None).await;
        let id = app.sessions.sessions()[0].id.clone();
        let prompt = TurnPrompt::from_text("first submission".to_string());

        // Act
        app.start_session(&id, prompt.clone()).await.expect("save");
        assert!(app.reply(&id, prompt.clone()).await);
        assert!(!app.reply(&id, "replacement").await);
        app.start_staged_session(&id)
            .await
            .expect("setup already running");
        let accepted = app
            .submit_prompt(PromptSubmission {
                prompt: prompt.clone(),
                session_id: id.clone(),
                session_mode: PromptSessionMode::NewRegular,
            })
            .await;
        let rejected = app
            .submit_prompt(PromptSubmission {
                prompt: TurnPrompt::from_text("replacement".to_string()),
                session_id: id.clone(),
                session_mode: PromptSessionMode::NewRegular,
            })
            .await;
        release.notify_one();
        finish_creation(&mut app).await;

        // Assert
        assert!(matches!(
            accepted,
            PromptWorkflowOutcome::ShowSession { .. }
        ));
        assert_eq!(rejected, PromptWorkflowOutcome::KeepPrompt);
        assert!(
            app.services
                .db()
                .sessions()
                .preparation_prompt_operation_status(&id)
                .await
                .expect("operation")
                .is_none()
        );
    }

    #[tokio::test]
    async fn staged_prompt_cannot_be_replaced_while_workspace_is_pending() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let id = app.create_draft_session().await.expect("draft");
        app.stage_draft_message(&id, "saved draft")
            .await
            .expect("stage");
        app.services
            .db()
            .sessions()
            .insert_session_preparation(&id, "main")
            .await
            .expect("prepare");
        app.queue_preparation_prompt(&id, &TurnPrompt::from_text("saved draft".to_string()))
            .await
            .expect("queue");

        // Act
        let replacement = app.stage_draft_message(&id, "replacement").await;
        let submitted = app
            .submit_prompt(PromptSubmission {
                prompt: TurnPrompt::from_text("replacement".to_string()),
                session_id: id.clone().into(),
                session_mode: PromptSessionMode::NewDraft,
            })
            .await;
        let premature = app
            .sessions
            .start_session(&app.services, &id, "premature")
            .await;

        // Assert
        assert!(replacement.is_err());
        assert_eq!(submitted, PromptWorkflowOutcome::KeepPrompt);
        assert!(
            premature
                .expect_err("not ready")
                .to_string()
                .contains("not ready")
        );
        assert_eq!(
            app.sessions.session_for_id(&id).expect("draft").prompt,
            "saved draft"
        );
    }

    #[tokio::test]
    async fn failed_setup_retries_the_saved_prompt_without_a_second_checkout() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let id = app.create_session().await.expect("session");
        app.services
            .db()
            .sessions()
            .update_session_preparation(&id, SessionPreparationState::Failed, Some("interrupted"))
            .await
            .expect("fail");

        // Act
        app.start_session(&id, "retry saved prompt")
            .await
            .expect("retry");
        finish_creation(&mut app).await;
        let preparation = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let preparation = app
                    .services
                    .db()
                    .sessions()
                    .load_session_preparation(&id)
                    .await
                    .expect("load")
                    .expect("preparation");
                if preparation.prompt.is_none() {
                    break preparation;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker must accept the saved prompt");

        // Assert
        assert_eq!(preparation.state, SessionPreparationState::Ready);
        assert!(preparation.prompt.is_none());
        assert!(
            app.services
                .db()
                .sessions()
                .preparation_prompt_operation_status(&id)
                .await
                .expect("operation")
                .is_some()
        );
        assert!(
            app.sessions
                .session_for_id(&id)
                .expect("session")
                .folder
                .is_dir()
        );
    }

    #[tokio::test]
    async fn reservation_failure_returns_an_error_without_starting_checkout() {
        // Arrange
        let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        sqlx::query(
            "CREATE TRIGGER reject_reservation BEFORE INSERT ON session_preparation BEGIN SELECT \
             RAISE(ABORT, 'reservation rejected'); END",
        )
        .execute(&pool)
        .await
        .expect("trigger");
        let (sender, receiver) = oneshot::channel();
        let request = regular_request(&app);

        // Act
        app.start_session_creation(request, Some(sender)).await;
        let result = receiver.await.expect("response");

        // Assert
        assert!(
            result
                .expect_err("reservation")
                .to_string()
                .contains("reservation rejected")
        );
        assert!(app.sessions.sessions().is_empty());
        assert!(app.pending_session_creations.is_empty());
    }

    #[tokio::test]
    async fn preparation_prompt_save_rejects_a_concurrent_cancellation() {
        // Arrange
        let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let id = app.create_session().await.expect("session");
        app.services
            .db()
            .sessions()
            .update_session_preparation(&id, SessionPreparationState::Preparing, None)
            .await
            .expect("prepare");
        sqlx::query(
            "CREATE TRIGGER cancel_submission BEFORE UPDATE OF prompt ON session_preparation WHEN \
             NEW.prompt IS NOT NULL BEGIN UPDATE session_preparation SET state = 'canceled' WHERE \
             session_id = NEW.session_id; SELECT RAISE(IGNORE); END",
        )
        .execute(&pool)
        .await
        .expect("trigger");

        // Act
        let result = app
            .queue_preparation_prompt(&id, &TurnPrompt::from_text("unsent".to_string()))
            .await;

        // Assert
        assert!(
            result
                .expect_err("canceled")
                .to_string()
                .contains("canceled")
        );
        assert!(
            app.services
                .db()
                .sessions()
                .load_session_preparation(&id)
                .await
                .expect("load")
                .expect("preparation")
                .prompt
                .is_none()
        );
    }

    #[tokio::test]
    async fn incremental_registration_is_idempotent_and_rejects_invalid_metadata() {
        // Arrange
        let (mut app, directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let id = app.create_session().await.expect("session");

        // Act
        app.sessions
            .register_created_session(&app.services, &id, directory.path())
            .await
            .expect("existing");
        app.services
            .db()
            .sessions()
            .update_session_preparation(&id, SessionPreparationState::Failed, Some("interrupted"))
            .await
            .expect("fail");
        app.refresh_sessions_now().await;
        app.refresh_sessions_now().await;
        let notice = app
            .sessions
            .session_for_id(&id)
            .expect("session")
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::WorkspacePreparation)
            .cloned();
        app.sessions.state_mut().replace_sessions(Vec::new());
        sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .expect("metadata");
        let invalid = app
            .sessions
            .register_created_session(&app.services, &id, directory.path())
            .await;

        // Assert
        assert!(notice.is_some());
        assert!(
            invalid
                .expect_err("invalid metadata")
                .to_string()
                .contains("permission")
        );
        assert!(app.sessions.sessions().is_empty());
    }

    #[tokio::test]
    async fn prepared_fork_reply_uses_one_durable_handoff() {
        // Arrange
        let (mut app, directory) = crate::test_support::new_git_test_app().await;
        let source = app.create_session().await.expect("source");
        crate::test_support::set_session_status_for_test(&mut app, &source, Status::Review);
        let id = app
            .sessions
            .fork_session(&app.services, &source)
            .await
            .expect("fork");
        app.sessions
            .register_created_session(&app.services, &id, directory.path())
            .await
            .expect("register");
        let prompt = TurnPrompt::from_text("continue frozen history".to_string());
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(&id, &serde_json::to_string(&prompt).expect("json"))
            .await
            .expect("save");

        // Act
        app.run_prepared_prompt(&id).await.expect("reply");
        app.run_prepared_prompt(&id).await.expect("acknowledged");
        app.run_prepared_prompt("missing").await.expect("deleted");

        // Assert
        assert!(
            app.services
                .db()
                .sessions()
                .preparation_prompt_operation_status(&id)
                .await
                .expect("operation")
                .is_some()
        );
        assert!(
            app.services
                .db()
                .sessions()
                .load_session_preparation(&id)
                .await
                .expect("load")
                .expect("preparation")
                .prompt
                .is_none()
        );
    }

    #[tokio::test]
    async fn failed_fork_handoff_retains_prompt_for_retry() {
        // Arrange
        let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let id = app.create_session().await.expect("session");
        crate::test_support::set_session_status_for_test(&mut app, &id, Status::Review);
        let prompt = TurnPrompt::from_text("retained reply".to_string());
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(&id, &serde_json::to_string(&prompt).expect("json"))
            .await
            .expect("save");
        sqlx::query(
            "CREATE TRIGGER reject_handoff BEFORE INSERT ON session_operation BEGIN SELECT \
             RAISE(ABORT, 'handoff rejected'); END",
        )
        .execute(&pool)
        .await
        .expect("trigger");

        // Act
        let rejected = app.run_prepared_prompt(&id).await;
        crate::test_support::set_session_status_for_test(&mut app, &id, Status::Done);
        let finished = app.run_prepared_prompt(&id).await;

        // Assert
        assert!(
            rejected
                .expect_err("handoff")
                .to_string()
                .contains("Could not start")
        );
        assert!(
            finished
                .expect_err("finished")
                .to_string()
                .contains("existing turn")
        );
        assert!(
            app.services
                .db()
                .sessions()
                .load_session_preparation(&id)
                .await
                .expect("load")
                .expect("preparation")
                .prompt
                .is_some()
        );
    }

    #[tokio::test]
    async fn prepared_stacked_prompt_rechecks_parent_readiness() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let parent = app.create_session().await.expect("parent");
        crate::test_support::set_session_status_for_test(&mut app, &parent, Status::Review);
        let id = app
            .create_stacked_draft_session(&parent)
            .await
            .expect("child");
        app.stage_draft_message(&id, "child prompt")
            .await
            .expect("stage");
        app.services
            .db()
            .sessions()
            .insert_session_preparation(&id, "main")
            .await
            .expect("prepare");
        app.queue_preparation_prompt(&id, &TurnPrompt::from_text("child prompt".to_string()))
            .await
            .expect("save");
        app.services
            .db()
            .sessions()
            .update_session_preparation(&id, SessionPreparationState::Ready, None)
            .await
            .expect("ready");
        crate::test_support::set_session_status_for_test(&mut app, &parent, Status::InProgress);

        // Act
        let result = app.run_prepared_prompt(&id).await;

        // Assert
        assert!(
            result
                .expect_err("parent became busy")
                .to_string()
                .contains("parent stack")
        );
        assert!(
            app.services
                .db()
                .sessions()
                .preparation_prompt_operation_status(&id)
                .await
                .expect("operation")
                .is_none()
        );
    }

    #[tokio::test]
    async fn ready_staged_draft_keeps_the_existing_ready_workspace() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let id = app.create_draft_session().await.expect("draft");
        app.stage_draft_message(&id, "ready draft")
            .await
            .expect("stage");
        app.services
            .db()
            .sessions()
            .insert_session_preparation(&id, "main")
            .await
            .expect("prepare");
        SessionManager::prepare_reserved_session(&app.services, &id)
            .await
            .expect("checkout");

        // Act
        app.start_staged_session(&id).await.expect("start");

        // Assert
        assert!(app.pending_session_creations.is_empty());
        assert_eq!(
            app.sessions.session_for_id(&id).expect("session").prompt,
            "ready draft"
        );
    }

    #[tokio::test]
    async fn deletion_during_preparation_cancels_before_cleanup() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let first = app.create_session().await.expect("session");
        let second = app.create_session().await.expect("session");
        for id in [&first, &second] {
            app.services
                .db()
                .sessions()
                .update_session_preparation(id, SessionPreparationState::Preparing, None)
                .await
                .expect("preparing");
            app.refresh_workspace_preparation(id).await;
        }

        // Act
        let index = app
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == first);
        app.sessions.select_session_index(index);
        app.delete_selected_session_deferred_cleanup().await;
        let index = app
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == second);
        app.sessions.select_session_index(index);
        app.sessions
            .delete_selected_session_deferred_cleanup(&app.projects, &app.services)
            .await;

        // Assert
        for id in [&first, &second] {
            assert!(
                app.services
                    .db()
                    .sessions()
                    .load_session(id)
                    .await
                    .expect("load")
                    .is_some()
            );
            assert_eq!(
                app.services
                    .db()
                    .sessions()
                    .load_session_preparation(id)
                    .await
                    .expect("load")
                    .expect("preparation")
                    .state,
                SessionPreparationState::Canceled
            );
        }
    }

    #[tokio::test]
    async fn failed_cancellation_does_not_delete_a_preparing_session() {
        // Arrange
        let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let id = app.create_session().await.expect("session");
        app.services
            .db()
            .sessions()
            .update_session_preparation(&id, SessionPreparationState::Preparing, None)
            .await
            .expect("preparing");
        app.refresh_workspace_preparation(&id).await;
        sqlx::query(
            "CREATE TRIGGER reject_cancellation BEFORE UPDATE OF state ON session_preparation \
             BEGIN SELECT RAISE(ABORT, 'cancel rejected'); END",
        )
        .execute(&pool)
        .await
        .expect("trigger");

        // Act
        app.sessions
            .delete_selected_session_deferred_cleanup(&app.projects, &app.services)
            .await;

        // Assert
        assert!(
            app.services
                .db()
                .sessions()
                .load_session(&id)
                .await
                .expect("load")
                .is_some()
        );
        assert!(app.sessions.session_for_id(&id).is_some());
    }

    #[tokio::test]
    async fn late_ready_completion_cannot_dispatch_a_canceled_prompt() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let id = app.create_session().await.expect("session");
        let prompt = TurnPrompt::from_text("never dispatch".to_string());
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(&id, &serde_json::to_string(&prompt).expect("json"))
            .await
            .expect("save");
        app.services
            .db()
            .sessions()
            .update_session_preparation(&id, SessionPreparationState::Canceled, None)
            .await
            .expect("cancel");

        // Act
        app.dispatch_prepared_prompt(&id).await;

        // Assert
        assert!(
            app.services
                .db()
                .sessions()
                .preparation_prompt_operation_status(&id)
                .await
                .expect("operation")
                .is_none()
        );
        assert!(
            app.services
                .db()
                .sessions()
                .load_session_preparation(&id)
                .await
                .expect("load")
                .expect("preparation")
                .prompt
                .is_some()
        );
    }

    #[tokio::test]
    async fn delayed_creation_allows_input_and_rendering_without_stealing_navigation() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app(1).await;
        let request = regular_request(&app);
        let presentation = PresentationState::default();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");

        // Act
        tokio::time::timeout(
            Duration::from_secs(1),
            app.start_session_creation(request, None),
        )
        .await
        .expect("creation must return before Git finishes");
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        app.mode = AppMode::List;
        crate::runtime::mode::list::handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        )
        .await
        .expect("help input");
        terminal
            .draw(|frame| presentation.render(&app.view_snapshot(), frame))
            .expect("draw help");

        // Assert
        assert!(matches!(app.mode, AppMode::Help { .. }));
        assert_eq!(app.pending_session_creations.len(), 1);
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "?")
        );
        release.notify_one();
        finish_creation(&mut app).await;
        assert!(matches!(app.mode, AppMode::Help { .. }));
    }

    #[tokio::test]
    async fn delayed_api_creation_allows_other_commands_before_answering() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app(1).await;
        let (response_tx, mut response_rx) = oneshot::channel();
        let request = regular_request(&app);
        let (lookup_tx, lookup_rx) = oneshot::channel();

        // Act
        tokio::time::timeout(
            Duration::from_secs(1),
            app.apply_session_runtime_command(SessionRuntimeCommand::Create {
                request,
                response_tx,
            }),
        )
        .await
        .expect("command must return before Git finishes");
        app.apply_session_runtime_command(SessionRuntimeCommand::Get {
            response_tx: lookup_tx,
            session_id: "missing".into(),
        })
        .await;

        // Assert
        assert!(
            lookup_rx
                .await
                .expect("lookup response")
                .expect("lookup")
                .is_none()
        );
        assert!(matches!(
            response_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(app.mode, AppMode::List));
        release.notify_one();
        finish_creation(&mut app).await;
        let session_id = response_rx
            .await
            .expect("creation response")
            .expect("reserved session");
        let preparation = app
            .services
            .db()
            .sessions()
            .load_session_preparation(&session_id)
            .await
            .expect("load")
            .expect("row");
        assert_eq!(preparation.state, SessionPreparationState::Failed);
        assert!(
            preparation
                .error
                .expect("setup error")
                .contains("delayed creation failed")
        );
        assert!(app.sessions.session_for_id(&session_id).is_some());
    }

    #[tokio::test]
    async fn failed_api_setup_retries_its_reserved_session_through_the_first_message() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app_with_attempts(2, 2).await;
        let (response_tx, response_rx) = oneshot::channel();
        let request = regular_request(&app);

        // Act
        app.apply_session_runtime_command(SessionRuntimeCommand::Create {
            request,
            response_tx,
        })
        .await;
        release.notify_one();
        finish_creation(&mut app).await;
        let session_id = response_rx
            .await
            .expect("response")
            .expect("reserved session");
        let (response_tx, response_rx) = oneshot::channel();
        app.apply_session_runtime_command(SessionRuntimeCommand::SendMessage {
            access: crate::app::session_runtime::SessionRuntimeAccess::Coordinator,
            message: "retry this session".to_string(),
            response_tx,
            session_id: session_id.clone(),
        })
        .await;
        response_rx
            .await
            .expect("message response")
            .expect("saved message");
        release.notify_one();
        finish_creation(&mut app).await;
        let preparations = app
            .services
            .db()
            .sessions()
            .load_session_preparations(app.active_project_id())
            .await
            .expect("preparations");

        // Assert
        assert_eq!(preparations.len(), 1);
        assert_eq!(preparations[0].session_id, session_id.as_str());
        assert_eq!(preparations[0].state, SessionPreparationState::Failed);
        let prompt: TurnPrompt =
            serde_json::from_str(preparations[0].prompt.as_deref().expect("saved prompt"))
                .expect("prompt");
        assert_eq!(prompt.text, "retry this session");
        assert_eq!(app.sessions.sessions().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_cleans_a_workspace_that_finishes_at_the_cancel_transition() {
        // Arrange
        let (mut app, directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let session_id = app.create_session().await.expect("session");
        let folder = app
            .sessions
            .session_for_id(&session_id)
            .expect("session")
            .folder
            .clone();
        app.services
            .db()
            .sessions()
            .update_session_preparation(&session_id, SessionPreparationState::Preparing, None)
            .await
            .expect("preparing");
        app.refresh_workspace_preparation(&session_id).await;
        sqlx::query(
            "CREATE TRIGGER finish_before_cancel BEFORE UPDATE OF state ON session_preparation \
             WHEN OLD.state = 'preparing' AND NEW.state = 'canceled' BEGIN UPDATE \
             session_preparation SET state = 'ready' WHERE session_id = OLD.session_id; SELECT \
             RAISE(IGNORE); END",
        )
        .execute(&pool)
        .await
        .expect("completion race");

        // Act
        app.cancel_session(&session_id).await.expect("cancel");
        app.wait_for_background_cleanup_tasks().await;
        let preparation = app
            .services
            .db()
            .sessions()
            .load_session_preparation(&session_id)
            .await
            .expect("load")
            .expect("row");
        let branch = app
            .services
            .git_client()
            .ref_hash(
                directory.path().to_path_buf(),
                crate::app::session::session_branch(&session_id),
            )
            .await;

        // Assert
        assert_eq!(preparation.state, SessionPreparationState::Canceled);
        assert!(!folder.exists());
        assert!(branch.is_err());
    }

    #[tokio::test]
    async fn completion_preserves_composer_input_and_saved_prompt_on_failure() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app(1).await;
        let request = regular_request(&app);
        app.start_session_creation(request, None).await;
        let session_id = app.sessions.sessions()[0].id.clone();
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.reset_text("still typing".to_string());
        }
        let prompt = TurnPrompt::from_text("saved early".to_string());

        // Act
        assert!(
            app.queue_preparation_prompt(&session_id, &prompt)
                .await
                .expect("queue")
        );
        assert!(
            app.queue_preparation_prompt(&session_id, &prompt)
                .await
                .expect("duplicate")
        );
        let other_prompt = TurnPrompt::from_text("do not overwrite".to_string());
        assert!(
            app.queue_preparation_prompt(&session_id, &other_prompt)
                .await
                .is_err()
        );
        release.notify_one();
        finish_creation(&mut app).await;
        let preparation = app
            .services
            .db()
            .sessions()
            .load_session_preparation(&session_id)
            .await
            .expect("load")
            .expect("preparation");

        // Assert
        assert_eq!(preparation.state, SessionPreparationState::Failed);
        assert_eq!(
            serde_json::from_str::<TurnPrompt>(&preparation.prompt.expect("saved"))
                .expect("decode"),
            prompt
        );
        assert!(
            matches!(&app.mode, AppMode::Prompt { input, .. } if input.text() == "still typing")
        );
        assert!(app.sessions.session_for_id(&session_id).is_some());
    }

    #[tokio::test]
    async fn recovered_handoff_never_replays_an_already_accepted_turn() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let session_id = app.create_session().await.expect("session");
        let prompt = TurnPrompt::from_text("saved prompt".to_string());
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(
                &session_id,
                &serde_json::to_string(&prompt).expect("serialize"),
            )
            .await
            .expect("save");
        let operation_id = format!("workspace:{session_id}");
        app.services
            .db()
            .operations()
            .insert_session_operation(&operation_id, &session_id, "run")
            .await
            .expect("accepted turn");

        // Act
        app.run_prepared_prompt(&session_id)
            .await
            .expect("recover queued handoff");
        assert!(
            app.services
                .db()
                .sessions()
                .load_session_preparation(&session_id)
                .await
                .expect("load queued prompt")
                .expect("preparation")
                .prompt
                .is_some()
        );
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(
                &session_id,
                &serde_json::to_string(&prompt).expect("serialize"),
            )
            .await
            .expect("simulate interrupted acknowledgement");
        app.services
            .db()
            .operations()
            .mark_session_operation_done(&operation_id)
            .await
            .expect("complete turn");
        app.run_prepared_prompt(&session_id)
            .await
            .expect("acknowledge prior turn");
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(
                &session_id,
                &serde_json::to_string(&prompt).expect("serialize"),
            )
            .await
            .expect("simulate acknowledgement retry with an existing transcript");
        app.run_prepared_prompt(&session_id)
            .await
            .expect("recover without duplicating the existing transcript");
        let preparation = app
            .services
            .db()
            .sessions()
            .load_session_preparation(&session_id)
            .await
            .expect("load")
            .expect("row");
        let messages = app
            .services
            .db()
            .sessions()
            .load_session_messages(&session_id)
            .await
            .expect("messages");

        // Assert
        assert!(preparation.prompt.is_none());
        let user_prompts: Vec<_> = messages
            .iter()
            .filter(|message| message.kind == "user_prompt")
            .map(|message| message.content.as_str())
            .collect();
        assert_eq!(user_prompts, vec![prompt.transcript_text().as_str()]);
    }

    #[tokio::test]
    async fn invalid_saved_prompt_is_retained_with_retry_feedback() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let session_id = app.create_session().await.expect("session");
        app.services
            .db()
            .sessions()
            .save_preparation_prompt(&session_id, "invalid json")
            .await
            .expect("save corrupt payload");

        // Act
        app.resume_ready_workspace_prompts().await;
        let preparation = app
            .services
            .db()
            .sessions()
            .load_session_preparation(&session_id)
            .await
            .expect("load")
            .expect("row");

        // Assert
        assert_eq!(preparation.state, SessionPreparationState::Failed);
        assert_eq!(preparation.prompt.as_deref(), Some("invalid json"));
        assert!(preparation.error.is_some());
        assert!(
            app.sessions
                .session_for_id(&session_id)
                .expect("session")
                .transient_messages
                .get(crate::domain::transient_message::TransientMessageSlot::WorkspacePreparation)
                .is_some()
        );
    }

    #[tokio::test]
    async fn cancellation_during_checkout_prevents_saved_prompt_dispatch() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app(2).await;
        let request = regular_request(&app);
        app.start_session_creation(request, None).await;
        let session_id = app.sessions.sessions()[0].id.clone();
        let prompt = TurnPrompt::from_text("do not launch after cancel".to_string());
        app.queue_preparation_prompt(&session_id, &prompt)
            .await
            .expect("save prompt");

        // Act
        app.cancel_session(&session_id)
            .await
            .expect("cancel preparation");
        release.notify_one();
        finish_creation(&mut app).await;
        app.wait_for_background_cleanup_tasks().await;
        let preparation = app
            .services
            .db()
            .sessions()
            .load_session_preparation(&session_id)
            .await
            .expect("load")
            .expect("row");
        let operation = app
            .services
            .db()
            .sessions()
            .preparation_prompt_operation_status(&session_id)
            .await
            .expect("handoff lookup");

        // Assert
        assert_eq!(preparation.state, SessionPreparationState::Canceled);
        assert!(operation.is_none());
        assert!(app.pending_session_creations.is_empty());
    }

    #[tokio::test]
    async fn canceled_preparation_rejects_retry_and_late_submissions() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let session_id = app.create_session().await.expect("session");
        let prompt = TurnPrompt::from_text("late prompt".to_string());
        app.services
            .db()
            .sessions()
            .update_session_preparation(&session_id, SessionPreparationState::Canceled, None)
            .await
            .expect("cancel");

        // Act
        let submission = app.queue_preparation_prompt(&session_id, &prompt).await;
        let retry = app.retry_workspace_preparation(&session_id).await;
        app.refresh_workspace_preparation(&session_id).await;

        // Assert
        assert!(
            submission
                .expect_err("canceled submission")
                .to_string()
                .contains("canceled")
        );
        assert!(
            retry
                .expect_err("canceled retry")
                .to_string()
                .contains("canceled")
        );
        assert!(app.pending_session_creations.is_empty());
    }

    #[tokio::test]
    async fn creation_rejection_preserves_typed_api_error_even_when_caller_disconnects() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        let (response_tx, response_rx) = oneshot::channel();
        let request = CreateSessionRequest {
            mode: CreateSessionMode::Stacked {
                parent_session_id: "missing".into(),
            },
            ..regular_request(&app)
        };

        // Act
        app.start_session_creation(request, Some(response_tx)).await;
        let result = response_rx.await.expect("creation response");
        let (response_tx, response_rx) = oneshot::channel();
        drop(response_rx);
        app.pending_session_creations.insert(
            "disconnected".to_string(),
            PendingSessionCreation {
                project_id: app.active_project_id(),
                session_id: None,
                response_tx: Some(response_tx),
            },
        );
        app.complete_session_creation("disconnected", Err(SessionError::NotFound))
            .await;

        // Assert
        assert_eq!(result, Err(SessionError::NotFound));
        assert!(app.pending_session_creations.is_empty());
    }
}
