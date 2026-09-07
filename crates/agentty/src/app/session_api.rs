//! Agentty adapter for the frontend-neutral `ag-session` programmatic API.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use ag_agent::{ReasoningLevel, ResponseStyle, SpeedMode, parse_persisted_session_agent_model};
use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CoordinatorMessageVisibility,
    CreateSessionMode, CreateSessionRequest, QuestionAnswer, ReviewRequest, ReviewRequestState,
    SessionBackend, SessionError as ApiSessionError, SessionId, SessionMessage, SessionMessageKind,
    SessionRole, SessionService, SessionSettings, SessionStatus,
};
use async_trait::async_trait;
use tokio::sync::oneshot;

#[cfg(test)]
use crate::app::branch_publish::{BranchPublishTaskSuccess, review_request_from_publish_result};
use crate::app::branch_publish::{branch_publish_loading_label, review_request_queued_label};
use crate::app::orchestration::{OrchestrationApprovalOutcome, child_session_is_stopped};
use crate::app::session::{
    SessionCreationKind, SessionCreationSettings, migrate_session_off_retired_model,
};
use crate::app::session_creation::PreparedSessionCreation;
use crate::app::{
    App, AppError, AppEvent, SessionError, SessionRuntimeAccess, SessionRuntimeCommand,
    SessionRuntimeHandle,
};
use crate::domain::orchestration::{
    IntegrationApproach, OrchestrationStatus, OrchestrationTaskStatus,
};
use crate::domain::session::{PublishBranchAction, Session};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{SessionMessageRow, SessionReviewRequestRow, SessionRow};

#[async_trait]
impl SessionBackend for SessionRuntimeHandle {
    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, ApiSessionError> {
        SessionRuntimeHandle::create_session(self, request).await
    }

    async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        SessionRuntimeHandle::get_session(self, session_id).await
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::send_message(self, session_id, message).await
    }

    async fn submit_coordinator_message(
        &self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::submit_coordinator_message(self, session_id, request).await
    }

    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::answer_questions(self, session_id, request).await
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::cancel_session(self, session_id).await
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::merge_session(self, session_id).await
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, ApiSessionError> {
        SessionRuntimeHandle::create_review_request(self, session_id).await
    }
}

impl App {
    /// Returns a cloneable frontend-neutral session capability.
    pub fn session_service(&self) -> SessionService {
        SessionService::new(Arc::new(self.sessions.handle()))
    }

    /// Returns the capability reserved for orchestration coordinators.
    pub(crate) fn coordinator_session_service(&self) -> SessionService {
        SessionService::new(Arc::new(self.sessions.coordinator_handle()))
    }

    /// Approves the current plan or advances integration with a selected
    /// destination.
    pub(crate) async fn approve_orchestration(
        &self,
        controller_session_id: &str,
        integration_approach: Option<IntegrationApproach>,
    ) -> OrchestrationApprovalOutcome {
        let outcome = crate::app::orchestration::approve_orchestration(
            self.services.db(),
            controller_session_id,
            integration_approach,
        )
        .await
        .unwrap_or(OrchestrationApprovalOutcome::Unavailable);
        if outcome == OrchestrationApprovalOutcome::Approved {
            self.services.emit_app_event(AppEvent::RefreshSessions);
        }

        outcome
    }

    /// Detaches one managed child and schedules a session-list refresh.
    pub(crate) async fn detach_managed_child(&self, child_session_id: &str) -> bool {
        let detached =
            crate::app::orchestration::detach_managed_child(self.services.db(), child_session_id)
                .await
                .unwrap_or(false);
        if detached {
            self.services.emit_app_event(AppEvent::RefreshSessions);
        }

        detached
    }

    /// Drives one local API request while processing the actor commands ahead
    /// of it.
    ///
    /// Background callers rely on the terminal event loop to drive the same
    /// mailbox. Foreground callers use this helper so awaiting their own
    /// response never deadlocks the foreground executor. Pending creation also
    /// pumps reducer events so its background completion can acknowledge the
    /// request; other commands retain their normal snapshot ordering.
    pub(crate) async fn drive_session_request<RequestFuture>(
        &mut self,
        request: RequestFuture,
    ) -> RequestFuture::Output
    where
        RequestFuture: Future,
    {
        let _session_runtime_consumer = self.sessions.foreground_consumer();
        tokio::pin!(request);

        loop {
            tokio::select! {
                biased;
                result = &mut request => return result,
                event = async {
                    if self.pending_session_creations.is_empty() {
                        crate::app::AppRuntimeEvent::Session(self.sessions.next_command().await)
                    } else {
                        self.next_runtime_event().await
                    }
                } => {
                    match event {
                        crate::app::AppRuntimeEvent::App(event) => {
                            Box::pin(self.apply_app_events(*event)).await;
                        }
                        crate::app::AppRuntimeEvent::Session(command) => {
                            self.apply_session_runtime_command(command).await;
                        }
                    }
                }
            }
        }
    }

    /// Executes one accepted session command and answers its response channel.
    pub(crate) async fn apply_session_runtime_command(&mut self, command: SessionRuntimeCommand) {
        match command {
            SessionRuntimeCommand::Create {
                request,
                response_tx,
            } => {
                self.start_session_creation(request, Some(response_tx))
                    .await;
            }
            SessionRuntimeCommand::Get {
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.get_api_session(&session_id).await);
            }
            SessionRuntimeCommand::SendMessage {
                access,
                message,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.send_api_message(&session_id, message, access).await);
            }
            SessionRuntimeCommand::SubmitCoordinatorMessage {
                request,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(
                    self.submit_api_coordinator_message(&session_id, request)
                        .await,
                );
            }
            SessionRuntimeCommand::AnswerQuestions {
                access,
                request,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(
                    self.answer_api_questions(&session_id, request, access)
                        .await,
                );
            }
            SessionRuntimeCommand::Cancel {
                access,
                response_tx,
                session_id,
            } => {
                let result = self.cancel_api_session(&session_id, access).await;
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::Merge {
                access,
                response_tx,
                session_id,
            } => {
                let result = self.merge_api_session(&session_id, access).await;
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::CreateReviewRequest {
                access,
                response_tx,
                session_id,
            } => {
                self.start_api_review_request_publish(session_id, access, response_tx)
                    .await;
            }
        }
    }

    /// Queues review-request publishing on the session worker and leaves the
    /// foreground command loop available while the API caller awaits the
    /// worker result.
    async fn start_api_review_request_publish(
        &mut self,
        session_id: SessionId,
        access: SessionRuntimeAccess,
        response_tx: oneshot::Sender<Result<ReviewRequest, ApiSessionError>>,
    ) {
        let Some(session) = self.sessions.session_for_id(&session_id) else {
            let _ = response_tx.send(Err(ApiSessionError::NotFound));

            return;
        };
        if session.is_managed() && access != SessionRuntimeAccess::Coordinator {
            let _ = response_tx.send(Err(managed_session_error(&session_id, "publish")));

            return;
        }
        if !session.owns_branch_changes() {
            let _ = response_tx.send(Err(ApiSessionError::Operation(
                "Orchestrator sessions cannot publish review requests".to_string(),
            )));

            return;
        }
        let Some(branch_publish_context) = self.branch_publish_task_context(&session_id) else {
            let _ = response_tx.send(Err(ApiSessionError::NotFound));

            return;
        };
        let branch_operation_lock = Arc::clone(&branch_publish_context.branch_operation_lock);
        // Reserve an idle branch before persistence. An existing owner
        // already serializes worker execution, so the runtime actor never waits
        // here.
        let _branch_operation_guard = branch_operation_lock.try_lock_owned().ok();
        let enqueue_result = self
            .sessions
            .enqueue_review_request_creation(
                &self.services,
                branch_publish_context.session,
                None,
                Some(response_tx),
            )
            .await;
        match enqueue_result {
            Err(error) => {
                let _ = self.sessions.finish_branch_publish(
                    &session_id,
                    crate::domain::transient_message::TransientMessageBody::Markdown(format!(
                        "**Review request publish failed**\n\n{error}"
                    )),
                );
            }
            Ok(Some(queued_order)) => self.sessions.queue_branch_publish(
                &session_id,
                queued_order,
                review_request_queued_label(),
            ),
            Ok(None) => self.sessions.start_branch_publish(
                &session_id,
                branch_publish_loading_label(PublishBranchAction::PublishPullRequest),
            ),
        }
    }

    /// Validates one creation request and captures its launch settings. Drafts
    /// persist immediately; materialized worktrees return an owned effect plan.
    pub(super) async fn prepare_api_session_creation(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<PreparedSessionCreation, ApiSessionError> {
        self.validate_api_session_request(&request).await?;
        self.ensure_project_checkout_available(request.project_id)
            .map_err(api_error_from_app)?;

        let inherited_settings = self
            .inherited_creation_settings(
                request.inherit_from_session_id.as_ref(),
                request.project_id,
            )
            .await?;
        let (base_branch_override, creation_settings) = inherited_settings
            .map_or((None, None), |inherited| {
                (Some(inherited.base_branch), Some(inherited.settings))
            });
        let creation_kind = match request.mode {
            CreateSessionMode::Draft => {
                let project = self.api_project_creation_context(base_branch_override)?;
                let session_id = self
                    .sessions
                    .create_draft_session_for_project_with_settings(
                        &self.services,
                        request.project_id,
                        &project.base_branch,
                        creation_settings,
                    )
                    .await
                    .map_err(api_error_from_session)?;

                return Ok(PreparedSessionCreation::Persisted(session_id));
            }
            CreateSessionMode::Stacked { parent_session_id } => {
                let session_id = if let Some(settings) = creation_settings {
                    self.sessions
                        .create_stacked_draft_session_with_settings(
                            &self.services,
                            &parent_session_id,
                            settings,
                        )
                        .await
                } else {
                    self.sessions
                        .create_stacked_draft_session(&self.services, &parent_session_id)
                        .await
                }
                .map_err(api_error_from_session)?;

                return Ok(PreparedSessionCreation::Persisted(session_id));
            }
            CreateSessionMode::Regular => SessionCreationKind::Worker,
            CreateSessionMode::Orchestrator => SessionCreationKind::Orchestrator,
            CreateSessionMode::OrchestrationChild { task_id } => {
                SessionCreationKind::OrchestrationChild { task_id }
            }
            CreateSessionMode::OrchestrationResearch { task_id } => {
                SessionCreationKind::OrchestrationResearch { task_id }
            }
        };
        let project = self.api_project_creation_context(base_branch_override)?;
        let settings = self
            .sessions
            .resolve_session_creation_settings(
                &self.services,
                request.project_id,
                creation_settings,
            )
            .await
            .map_err(api_error_from_session)?;

        Ok(PreparedSessionCreation::Materialized {
            base_branch: project.base_branch,
            creation_kind,
            project_id: request.project_id,
            settings,
            working_dir: project.working_dir,
        })
    }

    async fn validate_api_session_request(
        &self,
        request: &CreateSessionRequest,
    ) -> Result<(), ApiSessionError> {
        if request.project_id != self.active_project_id() {
            return Err(ApiSessionError::Operation(format!(
                "Project `{}` is not active",
                request.project_id
            )));
        }
        if let CreateSessionMode::Stacked { parent_session_id } = &request.mode {
            let parent = self
                .get_api_session(parent_session_id)
                .await?
                .ok_or(ApiSessionError::NotFound)?;
            if parent.settings.project_id != request.project_id {
                return Err(ApiSessionError::Operation(format!(
                    "Parent session `{parent_session_id}` belongs to project `{}`, not `{}`",
                    parent.settings.project_id, request.project_id
                )));
            }
        }

        Ok(())
    }

    /// Loads one complete session aggregate from persistence plus live queue
    /// state.
    async fn get_api_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        let Some(row) = self
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(None);
        };
        let session_status = row
            .status
            .parse::<SessionStatus>()
            .unwrap_or(SessionStatus::Done);
        migrate_session_off_retired_model(
            self.services.db(),
            &row.id,
            &row.agent,
            &row.model,
            session_status,
        )
        .await;
        let message_rows = self
            .services
            .db()
            .sessions()
            .load_session_messages(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let queued_messages = self
            .sessions
            .session_for_id(session_id)
            .map(|session| {
                session
                    .queued_messages
                    .iter()
                    .map(|message| message.transcript_text().to_string())
                    .collect()
            })
            .unwrap_or_default();

        build_api_session(row, message_rows, queued_messages).map(Some)
    }

    /// Sends one validated API message through the existing session workflow.
    async fn send_api_message(
        &mut self,
        session_id: &SessionId,
        message: String,
        access: SessionRuntimeAccess,
    ) -> Result<(), ApiSessionError> {
        if message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot send an empty session message".to_string(),
            ));
        }

        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        if session.is_managed() && access != SessionRuntimeAccess::Coordinator {
            return Err(managed_session_error(session_id, "send messages"));
        }
        let is_draft = session.is_draft_session();
        let status = session.status;
        let prompt = TurnPrompt::from_text(message);

        if status == SessionStatus::Draft {
            if is_draft {
                self.stage_draft_message(session_id, prompt)
                    .await
                    .map_err(api_error_from_app)?;
                self.start_staged_session(session_id)
                    .await
                    .map_err(api_error_from_app)?;
            } else {
                self.start_session(session_id, prompt)
                    .await
                    .map_err(api_error_from_app)?;
            }

            return Ok(());
        }

        if matches!(status, SessionStatus::InProgress | SessionStatus::Rebasing) {
            return self
                .enqueue_message(session_id, prompt)
                .map_err(api_error_from_session);
        }

        if App::reply(self, session_id, prompt).await {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot accept a message in status `{status}`"
        )))
    }

    /// Submits a coordinator-owned turn only when it can bypass the lossy
    /// in-memory chat queue and enter the serialized session worker directly.
    async fn submit_api_coordinator_message(
        &mut self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), ApiSessionError> {
        if request.message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit an empty coordinator message".to_string(),
            ));
        }
        if request.operation_id.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit a coordinator message without an operation id".to_string(),
            ));
        }

        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        let status = session.status;
        if !matches!(
            status,
            SessionStatus::Review | SessionStatus::AgentReview | SessionStatus::Question
        ) {
            return Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept a coordinator message in status `{status}`"
            )));
        }

        if self
            .sessions
            .reply_to_coordinator_message(
                &self.services,
                session_id,
                request.operation_id,
                request.visibility == CoordinatorMessageVisibility::Visible,
                TurnPrompt::from_agent_data(request.message),
            )
            .await
        {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` could not enqueue the coordinator message"
        )))
    }

    /// Cascades orchestrator cancellation to every active child before
    /// canceling the controller itself.
    ///
    /// A child cancellation failure aborts the cascade and leaves the
    /// orchestration active so the controller never reports a false terminal
    /// cancellation while a worker may still be running.
    async fn cancel_api_session(
        &mut self,
        session_id: &SessionId,
        access: SessionRuntimeAccess,
    ) -> Result<(), ApiSessionError> {
        if self
            .sessions
            .session_for_id(session_id)
            .is_some_and(Session::is_managed)
            && access != SessionRuntimeAccess::Coordinator
        {
            return Err(managed_session_error(session_id, "cancel"));
        }
        let is_orchestrator = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| session.role == SessionRole::Orchestrator);
        if is_orchestrator {
            self.cancel_api_orchestration(session_id).await?;
        }

        if access == SessionRuntimeAccess::Coordinator
            && self
                .sessions
                .session_for_id(session_id)
                .is_some_and(Session::is_managed)
        {
            return self
                .sessions
                .cancel_managed_session(&self.services, session_id)
                .await
                .map_err(api_error_from_session);
        }

        self.cancel_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    async fn cancel_api_orchestration(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        let Some(orchestration) = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_for_controller(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(());
        };
        let cancellation_started = self
            .services
            .db()
            .orchestrations()
            .begin_orchestration_cancellation(orchestration.id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        if !cancellation_started {
            return Ok(());
        }
        let tasks = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(orchestration.id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        for task in tasks.into_iter().filter(|task| {
            task.status
                .parse::<OrchestrationTaskStatus>()
                .is_ok_and(|status| !status.is_settled())
        }) {
            let child_session_id = if task.child_session_id.is_some() {
                task.child_session_id
            } else {
                self.services
                    .db()
                    .orchestrations()
                    .load_child_session_id_for_task(task.id)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            };
            if let Some(child_session_id) = child_session_id.as_deref()
                && !child_session_is_stopped(task.child_status.as_deref())
            {
                self.sessions
                    .cancel_managed_session(&self.services, child_session_id)
                    .await
                    .map_err(api_error_from_session)?;
            }
            self.services
                .db()
                .orchestrations()
                .update_orchestration_task_status(
                    task.id,
                    &OrchestrationTaskStatus::Canceled.to_string(),
                    None,
                )
                .await
                .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        }
        self.services
            .db()
            .orchestrations()
            .update_orchestration_status(
                orchestration.id,
                &OrchestrationStatus::Canceled.to_string(),
            )
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        self.sessions
            .update_orchestration_progress(session_id, None);

        Ok(())
    }

    /// Returns the active child count displayed in orchestration cancellation
    /// confirmation.
    pub(crate) async fn orchestration_running_child_count(&self, session_id: &str) -> usize {
        crate::app::orchestration::running_child_count(self.services.db(), session_id).await
    }

    /// Claims structured question answers against the current persisted
    /// question set before resuming the session.
    async fn answer_api_questions(
        &mut self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
        access: SessionRuntimeAccess,
    ) -> Result<(), ApiSessionError> {
        if self
            .sessions
            .session_for_id(session_id)
            .is_some_and(Session::is_managed)
            && access != SessionRuntimeAccess::Coordinator
        {
            return Err(managed_session_error(session_id, "answer questions"));
        }
        let session_role = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?
            .role;
        let question_relay = if session_role == SessionRole::Orchestrator {
            self.orchestration_question_target(session_id).await?
        } else {
            None
        };
        let target_session_id = question_relay
            .as_ref()
            .map_or(session_id, |(_, target_session_id)| target_session_id);
        let row = self
            .services
            .db()
            .sessions()
            .load_session(target_session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            .ok_or(ApiSessionError::NotFound)?;
        let persisted_questions = row.questions.unwrap_or_default();
        let questions = api_questions_from_json(Some(&persisted_questions), target_session_id)?;
        validate_question_answers(&questions, &request.answers)?;
        let message = question_answer_message(&request.answers);
        let status = self
            .sessions
            .session_for_id(target_session_id)
            .ok_or(ApiSessionError::NotFound)?
            .status;

        self.services
            .db()
            .sessions()
            .update_session_questions(target_session_id, "")
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let send_result = if self
            .sessions
            .reply_to_question_answers(&self.services, target_session_id, message)
            .await
        {
            Ok(())
        } else {
            Err(ApiSessionError::Operation(format!(
                "Session `{target_session_id}` cannot accept question answers in status `{status}`"
            )))
        };
        if let Err(send_error) = send_result {
            self.services
                .db()
                .sessions()
                .update_session_questions(target_session_id, &persisted_questions)
                .await
                .map_err(|restore_error| question_restore_error(&send_error, &restore_error))?;

            return Err(send_error);
        }
        self.clear_diff_comment_progress(target_session_id);
        if let Some((session_orchestration_id, _)) = question_relay {
            self.services
                .db()
                .orchestrations()
                .clear_orchestration_questions(session_orchestration_id)
                .await
                .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        }
        self.services.emit_app_event(AppEvent::RefreshSessions);

        Ok(())
    }

    /// Resolves the exact managed task claimed by the controller's question
    /// inbox.
    async fn orchestration_question_target(
        &self,
        controller_session_id: &SessionId,
    ) -> Result<Option<(i64, SessionId)>, ApiSessionError> {
        let Some(orchestration) = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_for_controller(controller_session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(None);
        };
        let Some(relayed_question_task_id) = orchestration.relayed_question_task_id else {
            return Ok(None);
        };
        let tasks = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(orchestration.id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;

        let target_session_id = tasks
            .into_iter()
            .find(|task| task.id == relayed_question_task_id)
            .and_then(|task| task.child_session_id)
            .map(SessionId::from)
            .ok_or_else(|| {
                ApiSessionError::Operation(format!(
                    "Orchestration question relay references unavailable task \
                     `{relayed_question_task_id}`"
                ))
            })?;

        Ok(Some((orchestration.id, target_session_id)))
    }

    /// Returns whether the controller's visible questions are a child relay.
    pub(crate) async fn has_orchestration_question_proxy(
        &self,
        controller_session_id: &str,
    ) -> bool {
        self.orchestration_question_target(&SessionId::from(controller_session_id))
            .await
            .is_ok_and(|relay| relay.is_some())
    }

    async fn merge_api_session(
        &mut self,
        session_id: &SessionId,
        access: SessionRuntimeAccess,
    ) -> Result<(), ApiSessionError> {
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        if session.is_managed() && access != SessionRuntimeAccess::Coordinator {
            return Err(managed_session_error(session_id, "merge"));
        }

        self.merge_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    /// Loads inherited launch settings and verifies that the source belongs
    /// to the requested project.
    async fn inherited_creation_settings(
        &self,
        source_session_id: Option<&SessionId>,
        project_id: i64,
    ) -> Result<Option<InheritedCreationSettings>, ApiSessionError> {
        let Some(source_session_id) = source_session_id else {
            return Ok(None);
        };
        let source = self
            .get_api_session(source_session_id)
            .await?
            .ok_or(ApiSessionError::NotFound)?;
        if source.settings.project_id != project_id {
            return Err(ApiSessionError::Operation(format!(
                "Session `{source_session_id}` belongs to project `{}`, not `{project_id}`",
                source.settings.project_id
            )));
        }

        Ok(Some(InheritedCreationSettings {
            base_branch: source.settings.base_branch,
            settings: SessionCreationSettings {
                agent: source.settings.agent,
                permission_mode: source.settings.permission_mode,
                personality_id: source.settings.personality_id,
                reasoning_level: source.settings.reasoning_level,
                response_style: source.settings.response_style,
                role: SessionRole::Worker,
                speed_mode: source.settings.speed_mode,
            },
        }))
    }

    /// Resolves the active project into worktree creation inputs.
    fn api_project_creation_context(
        &self,
        base_branch_override: Option<String>,
    ) -> Result<ApiProjectCreationContext, ApiSessionError> {
        let base_branch = base_branch_override
            .or_else(|| self.projects.git_branch().map(str::to_string))
            .ok_or_else(|| {
                ApiSessionError::Operation("Git branch is required to create a session".to_string())
            })?;

        Ok(ApiProjectCreationContext {
            base_branch,
            working_dir: self.projects.working_dir().to_path_buf(),
        })
    }

    /// Attempts to register a newly persisted active-project session before
    /// acknowledging creation, scheduling a refresh retry when loading is
    /// temporarily unavailable.
    pub(super) async fn finish_api_session_creation(&mut self, session_id: &str) {
        if self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.refresh_sessions_now().await;
        if self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.services.emit_app_event(AppEvent::RefreshSessions);
    }
}

/// Worktree inputs resolved for one API-requested project.
struct ApiProjectCreationContext {
    base_branch: String,
    working_dir: PathBuf,
}

/// Launch settings loaded from one existing session.
struct InheritedCreationSettings {
    base_branch: String,
    settings: SessionCreationSettings,
}

/// Combines a rejected question answer with a subsequent persistence failure.
fn question_restore_error(
    send_error: &ApiSessionError,
    restore_error: &impl std::fmt::Display,
) -> ApiSessionError {
    ApiSessionError::Operation(format!(
        "{send_error}; failed to restore session questions: {restore_error}"
    ))
}

/// Validates that a structured answer set exactly matches the current
/// persisted questions.
fn validate_question_answers(
    questions: &[QuestionItem],
    answers: &[QuestionAnswer],
) -> Result<(), ApiSessionError> {
    if questions.is_empty() {
        return Err(ApiSessionError::Operation(
            "Session has no questions to answer".to_string(),
        ));
    }

    if questions.len() != answers.len() {
        return Err(ApiSessionError::Operation(format!(
            "Expected {} question answers, received {}",
            questions.len(),
            answers.len()
        )));
    }

    for (question_index, (question, answer)) in questions.iter().zip(answers).enumerate() {
        if question.text != answer.question {
            return Err(ApiSessionError::Operation(format!(
                "Question answer {} is stale",
                question_index + 1
            )));
        }
        if answer.answer.trim().is_empty() {
            return Err(ApiSessionError::Operation(format!(
                "Question answer {} is empty",
                question_index + 1
            )));
        }
    }

    Ok(())
}

/// Formats validated structured answers into the existing clarification
/// follow-up prompt.
fn question_answer_message(answers: &[QuestionAnswer]) -> String {
    let mut lines = vec!["Clarifications:".to_string()];

    for (question_index, answer) in answers.iter().enumerate() {
        lines.push(format!("{}. Q: {}", question_index + 1, answer.question));
        lines.push(format!("   A: {}", answer.answer));
    }

    lines.join("\n")
}

/// Converts complete persistence rows into the public session aggregate.
fn build_api_session(
    row: SessionRow,
    message_rows: Vec<SessionMessageRow>,
    queued_messages: Vec<String>,
) -> Result<ag_session::Session, ApiSessionError> {
    let project_id = row.project_id.ok_or_else(|| {
        ApiSessionError::InvalidData(format!("session `{}` has no project", row.id))
    })?;
    let status = row
        .status
        .parse::<SessionStatus>()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?;
    let reasoning_level = row
        .reasoning_level_override
        .as_deref()
        .and_then(|value| value.parse::<ReasoningLevel>().ok())
        .unwrap_or_default();
    let permission_mode = row
        .permission_mode
        .parse::<ag_agent::PermissionMode>()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?;
    let role = row
        .role
        .as_deref()
        .map(str::parse::<SessionRole>)
        .transpose()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?
        .unwrap_or_default();
    let speed_mode = row.speed_mode.parse::<SpeedMode>().unwrap_or_default();
    let response_style = row
        .response_style
        .parse::<ResponseStyle>()
        .unwrap_or_default();
    let messages = message_rows
        .into_iter()
        .map(api_message_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let questions = api_questions_from_json(row.questions.as_deref(), &row.id)?;
    let review_request = row
        .review_request
        .map(api_review_request_from_row)
        .transpose()?;
    let draft_prompt = (row.is_draft && !row.prompt.is_empty()).then_some(row.prompt);

    Ok(ag_session::Session {
        created_at: row.created_at,
        draft_prompt,
        id: SessionId::from(row.id),
        messages,
        published_upstream_ref: row.published_upstream_ref,
        questions,
        queued_messages,
        review_request,
        settings: SessionSettings {
            agent: parse_persisted_session_agent_model(Some(&row.agent), &row.model),
            base_branch: row.base_branch,
            is_draft: row.is_draft,
            parent_session_id: row.parent_session_id.map(SessionId::from),
            permission_mode,
            personality_id: row.personality_id,
            project_id,
            reasoning_level,
            response_style,
            role,
            speed_mode,
        },
        status,
        title: row.title,
        updated_at: row.updated_at,
    })
}

/// Converts one persisted transcript row into its shared typed model.
fn api_message_from_row(row: SessionMessageRow) -> Result<SessionMessage, ApiSessionError> {
    let kind = row.kind.parse::<SessionMessageKind>().map_err(|error| {
        ApiSessionError::InvalidData(format!(
            "session message at position {}: {error}",
            row.position
        ))
    })?;

    Ok(SessionMessage::new(row.position, kind, row.content))
}

/// Parses current and legacy persisted clarification-question payloads.
fn api_questions_from_json(
    raw_json: Option<&str>,
    session_id: &str,
) -> Result<Vec<QuestionItem>, ApiSessionError> {
    let Some(raw_json) = raw_json.filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    if let Ok(questions) = serde_json::from_str::<Vec<QuestionItem>>(raw_json) {
        return Ok(questions);
    }

    serde_json::from_str::<Vec<String>>(raw_json)
        .map(|questions| questions.into_iter().map(QuestionItem::new).collect())
        .map_err(|error| {
            ApiSessionError::InvalidData(format!(
                "session `{session_id}` has invalid questions: {error}"
            ))
        })
}

/// Converts persisted joined forge metadata into its shared typed model.
fn api_review_request_from_row(
    row: SessionReviewRequestRow,
) -> Result<ReviewRequest, ApiSessionError> {
    let forge_kind = row
        .forge_kind
        .parse()
        .map_err(ApiSessionError::InvalidData)?;
    let state = row
        .state
        .parse::<ReviewRequestState>()
        .map_err(ApiSessionError::InvalidData)?;

    Ok(ReviewRequest {
        last_refreshed_at: row.last_refreshed_at,
        summary: ag_session::ReviewRequestSummary {
            display_id: row.display_id,
            forge_kind,
            source_branch: row.source_branch,
            state,
            status_summary: row.status_summary,
            target_branch: row.target_branch,
            title: row.title,
            web_url: row.web_url,
        },
    })
}

/// Preserves stable not-found semantics while translating host app errors.
fn api_error_from_app(error: AppError) -> ApiSessionError {
    match error {
        AppError::Session(error) => api_error_from_session(error),
        other => ApiSessionError::Operation(other.to_string()),
    }
}

/// Preserves stable not-found semantics while translating session errors.
fn api_error_from_session(error: SessionError) -> ApiSessionError {
    match error {
        SessionError::NotFound => ApiSessionError::NotFound,
        other => ApiSessionError::Operation(other.to_string()),
    }
}

/// Builds the stable capability error returned for direct managed-worker
/// mutations.
fn managed_session_error(session_id: &SessionId, action: &str) -> ApiSessionError {
    ApiSessionError::Operation(format!(
        "Session `{session_id}` is managed by an orchestration campaign and cannot {action} \
         directly"
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ag_agent::{
        AgentKind, AgentModel, AgentRequestKind, AgentSelection, AppServerTurnResponse,
        MockAppServerClient, PermissionMode,
    };
    use ag_forge::ForgeKind;

    use super::*;
    use crate::domain::orchestration::OrchestrationTaskKind;
    use crate::domain::session::Status;
    use crate::domain::transient_message::{TransientMessageBody, TransientMessageSlot};
    use crate::infra::db::PersistedOrchestrationTask;
    use crate::presentation::app_mode::{DiffCommentTarget, DiffLineComments};

    async fn request_session_creation(
        app: &mut App,
        request: CreateSessionRequest,
    ) -> Result<SessionId, ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.create_session(request).await })
            .await
    }

    async fn request_session(
        app: &mut App,
        session_id: SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.get_session(&session_id).await })
            .await
    }

    async fn request_message(
        app: &mut App,
        session_id: SessionId,
        message: &str,
    ) -> Result<(), ApiSessionError> {
        let service = app.session_service();
        let message = message.to_string();

        app.drive_session_request(async move { service.send_message(&session_id, message).await })
            .await
    }

    async fn request_coordinator_message(
        app: &mut App,
        session_id: SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move {
            service
                .submit_coordinator_message(&session_id, request)
                .await
        })
        .await
    }

    async fn request_question_answers(
        app: &mut App,
        session_id: SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(
            async move { service.answer_questions(&session_id, request).await },
        )
        .await
    }

    async fn request_cancellation(
        app: &mut App,
        session_id: SessionId,
    ) -> Result<(), ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.cancel_session(&session_id).await })
            .await
    }

    async fn request_merge(app: &mut App, session_id: SessionId) -> Result<(), ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.merge_session(&session_id).await })
            .await
    }

    async fn request_review_request(
        app: &mut App,
        session_id: SessionId,
    ) -> Result<ReviewRequest, ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.create_review_request(&session_id).await })
            .await
    }

    struct ActiveOrchestrationFixture {
        child: SessionId,
        controller: SessionId,
        orchestration: i64,
        task: i64,
    }

    async fn set_orchestration_fixture_review_statuses(
        app: &mut App,
        session_ids: [&SessionId; 2],
    ) {
        for session_id in session_ids {
            crate::test_support::set_session_status_for_test(
                app,
                session_id,
                SessionStatus::Review,
            );
            app.services
                .db()
                .sessions()
                .update_session_status_with_timing_at(session_id, "Review", 0)
                .await
                .expect("orchestration fixture status should persist");
        }
    }

    async fn seed_active_orchestration_child(
        app: &mut App,
        link_child: bool,
    ) -> ActiveOrchestrationFixture {
        seed_active_orchestration_session(app, link_child, OrchestrationTaskKind::Implementation)
            .await
    }

    async fn seed_active_orchestration_session(
        app: &mut App,
        link_child: bool,
        task_kind: OrchestrationTaskKind,
    ) -> ActiveOrchestrationFixture {
        let project_id = app.active_project_id();
        let controller_session_id = request_session_creation(
            app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Orchestrator,
                project_id,
            },
        )
        .await
        .expect("orchestrator should be created");
        let orchestration_id = app
            .services
            .db()
            .orchestrations()
            .insert_orchestration(
                &controller_session_id,
                &OrchestrationStatus::Running.to_string(),
                2,
            )
            .await
            .expect("orchestration should persist");
        let task_id = app
            .services
            .db()
            .orchestrations()
            .upsert_orchestration_task(PersistedOrchestrationTask {
                acceptance_criteria: r#"["Worker task is implemented"]"#.to_string(),
                kind: task_kind.to_string(),
                merge_position: 0,
                prompt: "Implement the worker task".to_string(),
                session_orchestration_id: orchestration_id,
                task_key: "worker-task".to_string(),
                title: "Worker task".to_string(),
                touched_areas: r#"["crates/worker/"]"#.to_string(),
            })
            .await
            .expect("orchestration task should persist");
        app.services
            .db()
            .orchestrations()
            .upsert_orchestration_task(PersistedOrchestrationTask {
                acceptance_criteria: r#"["Unlinked task is implemented"]"#.to_string(),
                kind: "Implementation".to_string(),
                merge_position: 1,
                prompt: "Implement the unlinked task".to_string(),
                session_orchestration_id: orchestration_id,
                task_key: "unlinked-task".to_string(),
                title: "Unlinked task".to_string(),
                touched_areas: r#"["crates/unlinked/"]"#.to_string(),
            })
            .await
            .expect("unlinked orchestration task should persist");
        let claimed = app
            .services
            .db()
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("orchestration task should be claimed");
        assert!(claimed);
        let child_session_id = request_session_creation(
            app,
            CreateSessionRequest {
                inherit_from_session_id: Some(controller_session_id.clone()),
                mode: match task_kind {
                    OrchestrationTaskKind::Implementation => {
                        CreateSessionMode::OrchestrationChild { task_id }
                    }
                    OrchestrationTaskKind::Research => {
                        CreateSessionMode::OrchestrationResearch { task_id }
                    }
                },
                project_id,
            },
        )
        .await
        .expect("orchestration child should be created");
        if link_child {
            let linked = app
                .services
                .db()
                .orchestrations()
                .link_orchestration_task_child(task_id, &child_session_id)
                .await
                .expect("orchestration child should link");
            assert!(linked);
        }
        set_orchestration_fixture_review_statuses(app, [&controller_session_id, &child_session_id])
            .await;

        ActiveOrchestrationFixture {
            child: child_session_id,
            controller: controller_session_id,
            orchestration: orchestration_id,
            task: task_id,
        }
    }

    #[tokio::test]
    async fn orchestration_research_mode_creates_a_managed_researcher() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;

        // Act
        let fixture =
            seed_active_orchestration_session(&mut app, true, OrchestrationTaskKind::Research)
                .await;
        let child = app
            .sessions
            .session_for_id(&fixture.child)
            .expect("research child should be loaded");
        let persisted_task = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(fixture.orchestration)
            .await
            .expect("research task should load from persistence")
            .into_iter()
            .find(|task| task.id == fixture.task)
            .expect("research task should exist in persistence");

        // Assert
        assert_eq!(child.role, SessionRole::OrchestrationResearcher);
        assert_eq!(
            persisted_task.child_session_id.as_deref(),
            Some(fixture.child.as_str())
        );
    }

    fn question_transition_app_server(
        first_turn_release: Arc<tokio::sync::Notify>,
        turn_started_tx: tokio::sync::mpsc::UnboundedSender<AgentRequestKind>,
    ) -> MockAppServerClient {
        let turn_index = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app_server = MockAppServerClient::new();
        app_server.expect_run_turn().times(3..).returning({
            move |request, _| {
                let first_turn_release = Arc::clone(&first_turn_release);
                let request_kind = request.request_kind;
                if request_kind == AgentRequestKind::UtilityPrompt {
                    return Box::pin(async {
                        Ok(app_server_response(
                            r#"{"answer":"Initial prompt","questions":[]}"#,
                            None,
                        ))
                    });
                }

                let turn_index =
                    turn_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = turn_started_tx.send(request_kind);

                Box::pin(async move {
                    if turn_index == 0 {
                        first_turn_release.notified().await;

                        return Ok(app_server_response(
                            r#"{"answer":"Need detail","questions":[{"text":"Current question?","options":[]}]}"#,
                            Some("conversation-1"),
                        ));
                    }

                    Ok(app_server_response(
                        r#"{"answer":"ready","questions":[]}"#,
                        Some("conversation-1"),
                    ))
                })
            }
        });
        app_server
            .expect_shutdown_session()
            .times(0..)
            .returning(|_| Box::pin(async {}));

        app_server
    }

    fn app_server_response(
        assistant_message: &str,
        provider_conversation_id: Option<&str>,
    ) -> AppServerTurnResponse {
        AppServerTurnResponse {
            assistant_message: assistant_message.to_string(),
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            pid: None,
            provider_conversation_id: provider_conversation_id.map(str::to_string),
        }
    }

    fn current_question_answer(answer: &str) -> AnswerQuestionsRequest {
        AnswerQuestionsRequest {
            answers: vec![QuestionAnswer {
                answer: answer.to_string(),
                question: "Current question?".to_string(),
            }],
        }
    }

    fn clarification_answer_count(session: &ag_session::Session) -> usize {
        session
            .messages
            .iter()
            .filter(|message| {
                message.kind == SessionMessageKind::UserPrompt
                    && message.content.starts_with("Clarifications:")
            })
            .count()
    }

    fn session_row() -> SessionRow {
        SessionRow {
            added_lines: 12,
            agent: "codex".to_string(),
            base_branch: "main".to_string(),
            created_at: 10,
            deleted_lines: 4,
            has_diff: Some(true),
            id: "session-1".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 40,
            input_tokens: 50,
            is_draft: true,
            model: "gpt-5.6-sol".to_string(),
            output_tokens: 60,
            parent_session_id: Some("parent-1".to_string()),
            permission_mode: "read_only".to_string(),
            personality_id: Some("reviewer".to_string()),
            project_id: Some(7),
            prompt: "staged prompt".to_string(),
            published_upstream_ref: Some("origin/wt/session-1".to_string()),
            questions: Some(
                r#"[{"text":"Which target?","options":["main","develop"]}]"#.to_string(),
            ),
            reasoning_level_override: Some("xhigh".to_string()),
            response_style: "detailed".to_string(),
            review_request: Some(SessionReviewRequestRow {
                display_id: "#42".to_string(),
                forge_kind: "GitHub".to_string(),
                last_refreshed_at: 15,
                source_branch: "wt/session-1".to_string(),
                state: "Open".to_string(),
                status_summary: Some("checks passing".to_string()),
                target_branch: "main".to_string(),
                title: "Build feature".to_string(),
                web_url: "https://example.test/pull/42".to_string(),
            }),
            role: None,
            size: "S".to_string(),
            speed_mode: "normal".to_string(),
            status: "Draft".to_string(),
            title: Some("Build feature".to_string()),
            updated_at: 20,
        }
    }

    #[test]
    fn api_review_request_result_requires_a_published_review_request() {
        // Arrange
        let review_request = api_review_request_from_row(
            session_row()
                .review_request
                .expect("review-request fixture should exist"),
        )
        .expect("review-request fixture should parse");
        let published_result = Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request: review_request.clone(),
            upstream_reference: "origin/wt/session-1".to_string(),
        });
        let pushed_result = Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: "wt/session-1".to_string(),
            review_request_creation: None,
            upstream_reference: "origin/wt/session-1".to_string(),
        });

        // Act
        let published_review_request = review_request_from_publish_result(&published_result);
        let pushed_error = review_request_from_publish_result(&pushed_result)
            .expect_err("a plain branch-push result should not satisfy the API request");

        // Assert
        assert_eq!(published_review_request, Ok(review_request));
        assert_eq!(
            pushed_error,
            "Review-request publishing completed without a review request".to_string()
        );
    }

    #[test]
    fn build_api_session_returns_complete_settings_and_messages() {
        // Arrange
        let row = session_row();
        let message_rows = vec![
            SessionMessageRow {
                content: "first".to_string(),
                kind: "user_prompt".to_string(),
                position: 0,
            },
            SessionMessageRow {
                content: "done".to_string(),
                kind: "assistant_answer".to_string(),
                position: 1,
            },
        ];

        // Act
        let session = build_api_session(row, message_rows, vec!["queued message".to_string()])
            .expect("row should convert");

        // Assert
        assert_eq!(session.id, "session-1");
        assert_eq!(session.draft_prompt.as_deref(), Some("staged prompt"));
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.questions[0].text, "Which target?");
        assert_eq!(session.queued_messages, ["queued message"]);
        assert_eq!(session.settings.project_id, 7);
        assert_eq!(session.settings.parent_session_id, Some("parent-1".into()));
        assert_eq!(session.settings.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(session.settings.personality_id.as_deref(), Some("reviewer"));
        assert_eq!(
            session.settings.agent,
            ag_agent::AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
        );
        assert_eq!(session.settings.reasoning_level, ReasoningLevel::XHigh);
        assert_eq!(session.settings.speed_mode, SpeedMode::Normal);
        assert_eq!(
            session
                .review_request
                .expect("review request should convert")
                .summary
                .forge_kind,
            ForgeKind::GitHub
        );
    }

    #[test]
    fn build_api_session_rejects_invalid_persisted_data() {
        // Arrange
        let mut missing_project = session_row();
        missing_project.project_id = None;
        let mut invalid_status = session_row();
        invalid_status.status = "Unknown".to_string();
        let mut invalid_permission_mode = session_row();
        invalid_permission_mode.permission_mode = "invalid".to_string();
        let invalid_message = SessionMessageRow {
            content: "content".to_string(),
            kind: "unknown".to_string(),
            position: 4,
        };
        let mut invalid_review = session_row();
        invalid_review
            .review_request
            .as_mut()
            .expect("fixture should have review metadata")
            .state = "Unknown".to_string();
        let mut invalid_questions = session_row();
        invalid_questions.questions = Some("{invalid".to_string());

        // Act
        let missing_project_error = build_api_session(missing_project, Vec::new(), Vec::new())
            .expect_err("project is required");
        let invalid_status_error = build_api_session(invalid_status, Vec::new(), Vec::new())
            .expect_err("status should be validated");
        let invalid_permission_mode_error =
            build_api_session(invalid_permission_mode, Vec::new(), Vec::new())
                .expect_err("permission mode should be validated");
        let invalid_message_error =
            build_api_session(session_row(), vec![invalid_message], Vec::new())
                .expect_err("message kind should be validated");
        let invalid_review_error = build_api_session(invalid_review, Vec::new(), Vec::new())
            .expect_err("review should be validated");
        let invalid_questions_error = build_api_session(invalid_questions, Vec::new(), Vec::new())
            .expect_err("questions should be validated");
        let legacy_questions = api_questions_from_json(Some(r#"["Legacy question"]"#), "session-1")
            .expect("legacy questions should convert");
        let empty_questions =
            api_questions_from_json(Some(""), "session-1").expect("empty questions should convert");

        // Assert
        assert!(matches!(
            missing_project_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_status_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_permission_mode_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_message_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_review_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_questions_error,
            ApiSessionError::InvalidData(_)
        ));
        assert_eq!(legacy_questions[0].text, "Legacy question");
        assert_eq!(empty_questions, Vec::new());
    }

    #[test]
    fn api_error_translation_preserves_not_found() {
        // Arrange / Act
        let app_error = api_error_from_app(AppError::Session(SessionError::NotFound));
        let session_error = api_error_from_session(SessionError::NotFound);
        let workflow_error = api_error_from_app(AppError::Workflow("workflow failed".to_string()));

        // Assert
        assert_eq!(app_error, ApiSessionError::NotFound);
        assert_eq!(session_error, ApiSessionError::NotFound);
        assert_eq!(
            workflow_error,
            ApiSessionError::Operation("workflow failed".to_string())
        );
    }

    #[tokio::test]
    async fn runtime_backend_creates_and_loads_complete_sessions() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();

        // Act
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("regular session should be created");
        app.services
            .db()
            .sessions()
            .append_session_message(&session_id, SessionMessageKind::UserPrompt, "build it")
            .await
            .expect("message should persist");
        let loaded_session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");
        let missing_session = request_session(&mut app, SessionId::from("missing"))
            .await
            .expect("missing lookup should succeed");
        let stacked_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Stacked {
                    parent_session_id: session_id.clone(),
                },
                project_id,
            },
        )
        .await
        .expect("stacked session should be created");
        let stacked_session = request_session(&mut app, stacked_session_id)
            .await
            .expect("stacked session should load")
            .expect("stacked session should exist");
        let inherited_stacked_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(session_id.clone()),
                mode: CreateSessionMode::Stacked {
                    parent_session_id: session_id.clone(),
                },
                project_id,
            },
        )
        .await
        .expect("inherited stacked session should be created");
        let inherited_stacked_session = request_session(&mut app, inherited_stacked_session_id)
            .await
            .expect("inherited stacked session should load")
            .expect("inherited stacked session should exist");

        // Assert
        assert_eq!(loaded_session.id, session_id);
        assert_eq!(loaded_session.status, SessionStatus::Draft);
        assert_eq!(loaded_session.messages.len(), 1);
        assert_eq!(loaded_session.messages[0].content, "build it");
        assert_eq!(
            loaded_session.settings.project_id,
            app.projects.active_project_id()
        );
        assert_eq!(missing_session, None);
        assert_eq!(stacked_session.settings.parent_session_id, Some(session_id));
        assert_eq!(
            inherited_stacked_session.settings.parent_session_id,
            Some(loaded_session.id)
        );
    }

    #[tokio::test]
    async fn runtime_backend_rejects_session_creation_during_project_sync() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        app.project_sync_status = Some(crate::app::sync::ProjectSyncStatus {
            context: crate::app::sync::ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id,
                project_name: "agentty".to_string(),
            },
            phase: crate::app::sync::ProjectSyncPhase::Running,
        });

        // Act
        let result = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Orchestrator,
                project_id,
            },
        )
        .await;

        // Assert
        assert!(matches!(
            result,
            Err(ApiSessionError::Operation(message))
                if message.contains("is synchronizing `main`")
        ));
    }

    #[tokio::test]
    async fn runtime_backend_coordinator_turn_clears_diff_comments() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Orchestrator,
                project_id,
            },
        )
        .await
        .expect("orchestrator should be created");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Review,
        );
        let mut line_comments = DiffLineComments::default();
        line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
        line_comments
            .editing_input_mut()
            .expect("controller diff comment should be editable")
            .insert_text("Review the controller change");
        line_comments.finish_editing();
        app.save_diff_comment_progress(session_id.clone(), line_comments);

        // Act
        let review_request_error = request_review_request(&mut app, session_id.clone())
            .await
            .expect_err("orchestrator review request should fail");
        request_coordinator_message(
            &mut app,
            session_id.clone(),
            CoordinatorMessageRequest {
                message: "Summarize the worker results".to_string(),
                operation_id: "orchestration-rollup-42".to_string(),
                visibility: CoordinatorMessageVisibility::Hidden,
            },
        )
        .await
        .expect("coordinator turn should be accepted");
        tokio::time::timeout(Duration::from_secs(1), async {
            while app.diff_comment_progress.contains_key(&session_id) {
                app.process_pending_app_events().await;
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("coordinator turn should clear saved diff comments when it starts");
        let messages = app
            .services
            .db()
            .sessions()
            .load_session_messages(&session_id)
            .await
            .expect("coordinator transcript should load");

        // Assert
        assert_eq!(
            review_request_error,
            ApiSessionError::Operation(
                "Orchestrator sessions cannot publish review requests".to_string()
            )
        );
        assert!(!app.diff_comment_progress.contains_key(&session_id));
        assert!(messages.iter().all(|message| {
            message.kind != SessionMessageKind::UserPrompt.to_string()
                || message.content != "Summarize the worker results"
        }));
    }

    #[tokio::test]
    async fn visible_coordinator_turn_persists_continuation_prompt() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Orchestrator,
                project_id,
            },
        )
        .await
        .expect("orchestrator should be created");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Review,
        );

        // Act
        request_coordinator_message(
            &mut app,
            session_id.clone(),
            CoordinatorMessageRequest {
                message: "Apply the requested correction".to_string(),
                operation_id: "orchestration-continuation-1-1".to_string(),
                visibility: CoordinatorMessageVisibility::Visible,
            },
        )
        .await
        .expect("visible coordinator turn should be accepted");
        let messages = app
            .services
            .db()
            .sessions()
            .load_session_messages(&session_id)
            .await
            .expect("continuation transcript should load");

        // Assert
        assert!(messages.iter().any(|message| {
            message.kind == SessionMessageKind::UserPrompt.to_string()
                && message.content == "Apply the requested correction"
        }));
    }

    #[tokio::test]
    async fn user_capability_rejects_every_managed_child_mutation() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let session_id = SessionId::from("session-id");
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .role(SessionRole::OrchestrationWorker)
                .status(Status::Review)
                .build(),
        );
        let (publish_tx, publish_rx) = oneshot::channel();

        // Act
        let send_error = app
            .send_api_message(
                &session_id,
                "Direct edit".to_string(),
                SessionRuntimeAccess::User,
            )
            .await
            .expect_err("managed child message should fail");
        let answer_error = app
            .answer_api_questions(
                &session_id,
                AnswerQuestionsRequest {
                    answers: Vec::new(),
                },
                SessionRuntimeAccess::User,
            )
            .await
            .expect_err("managed child answers should fail");
        let cancel_error = app
            .cancel_api_session(&session_id, SessionRuntimeAccess::User)
            .await
            .expect_err("managed child cancellation should fail");
        let merge_error = app
            .merge_api_session(&session_id, SessionRuntimeAccess::User)
            .await
            .expect_err("managed child merge should fail");
        app.start_api_review_request_publish(
            session_id.clone(),
            SessionRuntimeAccess::User,
            publish_tx,
        )
        .await;
        let publish_error = publish_rx
            .await
            .expect("publish result should be returned")
            .expect_err("managed child publish should fail");

        // Assert
        for error in [
            send_error,
            answer_error,
            cancel_error,
            merge_error,
            publish_error,
        ] {
            assert!(matches!(
                error,
                ApiSessionError::Operation(message)
                    if message.contains("managed by an orchestration campaign")
            ));
        }
    }

    #[tokio::test]
    async fn review_request_queue_rejects_stale_in_progress_session_without_worker() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("regular session should be created");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::InProgress,
        );

        // Act
        let result = request_review_request(&mut app, session_id).await;

        // Assert
        assert!(matches!(
            result,
            Err(ApiSessionError::Operation(message))
                if message.contains("active session worker is unavailable")
        ));
    }

    #[tokio::test]
    async fn review_request_runtime_handler_does_not_wait_for_branch_operation() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let session_id = SessionId::from(
            app.create_session()
                .await
                .expect("regular session should be created"),
        );
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Done,
        );
        let branch_operation_lock = Arc::clone(
            &app.sessions
                .session_handles_or_err(&session_id)
                .expect("expected session handles")
                .branch_operation_lock,
        );
        let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
        let (response_tx, mut response_rx) = oneshot::channel();

        // Act
        let start_result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            app.start_api_review_request_publish(
                session_id.clone(),
                SessionRuntimeAccess::User,
                response_tx,
            ),
        )
        .await;
        let response_before_unlock = response_rx.try_recv();
        drop(existing_operation_guard);
        let response_after_unlock = response_rx
            .await
            .expect("review-request result should be delivered after the lock is released");

        // Assert
        assert!(
            start_result.is_ok(),
            "runtime command handling should not wait for the branch operation"
        );
        assert!(matches!(
            response_before_unlock,
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            matches!(
                &response_after_unlock,
                Err(ApiSessionError::Operation(message))
                    if message == "Session must be in review to publish the review request."
            ),
            "unexpected review-request response: {response_after_unlock:?}"
        );
    }

    #[tokio::test]
    async fn review_request_runtime_handler_queues_on_existing_worker() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let session_id = SessionId::from(
            app.create_session()
                .await
                .expect("regular session should be created"),
        );
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Done,
        );
        let branch_operation_lock = Arc::clone(
            &app.sessions
                .session_handles_or_err(&session_id)
                .expect("expected session handles")
                .branch_operation_lock,
        );
        let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
        let (first_response_tx, _first_response_rx) = oneshot::channel();
        app.start_api_review_request_publish(
            session_id.clone(),
            SessionRuntimeAccess::User,
            first_response_tx,
        )
        .await;
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::InProgress,
        );
        let (queued_response_tx, _queued_response_rx) = oneshot::channel();

        // Act
        app.start_api_review_request_publish(
            session_id.clone(),
            SessionRuntimeAccess::User,
            queued_response_tx,
        )
        .await;
        let publish_body = app.sessions.state().sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .map(|message| &message.body);

        // Assert
        assert!(matches!(
            publish_body,
            Some(TransientMessageBody::Queued(action))
                if action.order == 0 && action.text == "review request — publish after this turn"
        ));
        drop(existing_operation_guard);
    }

    #[tokio::test]
    async fn coordinator_capability_cancels_managed_workers_and_regular_sessions_still_cancel() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let fixture = seed_active_orchestration_child(&mut app, true).await;
        let coordinator_service = app.coordinator_session_service();
        let managed_child = fixture.child.clone();
        let project_id = app.active_project_id();
        let regular_session = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("regular session should be created");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &regular_session,
            SessionStatus::Review,
        );

        // Act
        let managed_result = app
            .drive_session_request(async move {
                coordinator_service.cancel_session(&managed_child).await
            })
            .await;
        let regular_result = request_cancellation(&mut app, regular_session.clone()).await;
        let managed = request_session(&mut app, fixture.child)
            .await
            .expect("managed worker should load")
            .expect("managed worker should exist");
        let regular = request_session(&mut app, regular_session)
            .await
            .expect("regular session should load")
            .expect("regular session should exist");

        // Assert
        assert_eq!(managed_result, Ok(()));
        assert_eq!(regular_result, Ok(()));
        assert_eq!(managed.status, SessionStatus::Canceled);
        assert_eq!(regular.status, SessionStatus::Canceled);
    }

    #[tokio::test]
    async fn orchestration_approvals_and_detach_update_campaign() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let fixture = seed_active_orchestration_child(&mut app, true).await;
        app.services
            .db()
            .orchestrations()
            .update_orchestration_status(
                fixture.orchestration,
                &OrchestrationStatus::AwaitingApproval.to_string(),
            )
            .await
            .expect("failed to park campaign");

        // Act / Assert
        assert_eq!(
            app.approve_orchestration(&fixture.controller, None).await,
            OrchestrationApprovalOutcome::Approved
        );
        assert_eq!(
            app.approve_orchestration(&fixture.controller, None).await,
            OrchestrationApprovalOutcome::Unavailable
        );
        app.services
            .db()
            .orchestrations()
            .update_orchestration_status(
                fixture.orchestration,
                &OrchestrationStatus::AwaitingIntegration.to_string(),
            )
            .await
            .expect("failed to park integration");
        assert_eq!(
            app.approve_orchestration(&fixture.controller, None).await,
            OrchestrationApprovalOutcome::IntegrationApproachRequired
        );
        assert_eq!(
            app.approve_orchestration(&fixture.controller, Some(IntegrationApproach::LocalMerge),)
                .await,
            OrchestrationApprovalOutcome::Approved
        );
        assert!(app.detach_managed_child(&fixture.child).await);
        assert!(!app.detach_managed_child(&fixture.child).await);
        assert_eq!(
            app.approve_orchestration("missing", None).await,
            OrchestrationApprovalOutcome::Unavailable
        );
    }

    #[tokio::test]
    async fn runtime_backend_migrates_retired_model_for_inactive_project_lookup() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("main".to_string()))
            .await
            .expect("inactive project should persist");
        let session_id = SessionId::from("inactive-retired-session");
        app.services
            .db()
            .sessions()
            .insert_session(
                &session_id,
                "gemini-3.5-flash",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("retired-model session should persist");

        // Act
        let loaded_session = request_session(&mut app, session_id.clone())
            .await
            .expect("session lookup should succeed")
            .expect("session should exist");
        let persisted_row = app
            .services
            .db()
            .sessions()
            .load_session(&session_id)
            .await
            .expect("migrated session should load")
            .expect("migrated session should exist");

        // Assert
        assert_eq!(loaded_session.settings.project_id, inactive_project_id);
        assert_eq!(
            loaded_session.settings.agent,
            ag_agent::AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini35FlashLite)
        );
        assert_eq!(persisted_row.agent, "antigravity");
        assert_eq!(persisted_row.model, "gemini-3.5-flash-lite");
    }

    #[tokio::test]
    async fn runtime_backend_rejects_creation_for_inactive_projects() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("develop".to_string()))
            .await
            .expect("inactive project should persist");

        // Act
        let creation_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id: inactive_project_id,
            },
        )
        .await
        .expect_err("inactive project creation should fail");

        // Assert
        assert_eq!(
            creation_error,
            ApiSessionError::Operation(format!("Project `{inactive_project_id}` is not active"))
        );
    }

    #[tokio::test]
    async fn runtime_backend_cascade_cancels_orchestrator_children_and_tasks() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let fixture = seed_active_orchestration_child(&mut app, true).await;
        app.sessions.update_orchestration_progress(
            &fixture.controller,
            Some("Working... child: running".to_string()),
        );

        // Act
        request_cancellation(&mut app, fixture.controller.clone())
            .await
            .expect("orchestrator cancellation should cascade");
        let controller = request_session(&mut app, fixture.controller.clone())
            .await
            .expect("controller should load")
            .expect("controller should exist");
        let child = request_session(&mut app, fixture.child)
            .await
            .expect("child should load")
            .expect("child should exist");
        let orchestration = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_for_controller(&fixture.controller)
            .await
            .expect("orchestration should load")
            .expect("orchestration should exist");
        let tasks = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(fixture.orchestration)
            .await
            .expect("orchestration tasks should load");

        // Assert
        assert_eq!(controller.status, SessionStatus::Canceled);
        assert_eq!(child.status, SessionStatus::Canceled);
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::Canceled.to_string()
        );
        assert_eq!(
            tasks[0].status,
            OrchestrationTaskStatus::Canceled.to_string()
        );
        assert!(
            app.sessions
                .sessions()
                .iter()
                .find(|session| session.id == fixture.controller)
                .and_then(|session| {
                    session
                        .transient_messages
                        .get(crate::domain::transient_message::TransientMessageSlot::Orchestration)
                })
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancel_api_orchestration_ignores_missing_orchestration() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let session_id = SessionId::from("missing-orchestration");

        // Act
        let result = app.cancel_api_orchestration(&session_id).await;

        // Assert
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn cancel_api_orchestration_ignores_settled_orchestration() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let fixture = seed_active_orchestration_child(&mut app, true).await;
        app.services
            .db()
            .orchestrations()
            .update_orchestration_status(
                fixture.orchestration,
                &OrchestrationStatus::Done.to_string(),
            )
            .await
            .expect("orchestration should settle");

        // Act
        let result = app.cancel_api_orchestration(&fixture.controller).await;
        let orchestration = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_for_controller(&fixture.controller)
            .await
            .expect("orchestration should load")
            .expect("orchestration should exist");

        // Assert
        assert_eq!(result, Ok(()));
        assert_eq!(orchestration.status, OrchestrationStatus::Done.to_string());
    }

    #[tokio::test]
    async fn runtime_backend_preserves_orchestration_when_child_cancellation_fails() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let fixture = seed_active_orchestration_child(&mut app, true).await;
        crate::test_support::set_session_status_for_test(
            &mut app,
            &fixture.child,
            SessionStatus::Done,
        );
        let controller_before = request_session(&mut app, fixture.controller.clone())
            .await
            .expect("controller should load before cancellation")
            .expect("controller should exist before cancellation");

        // Act
        let cancel_error = request_cancellation(&mut app, fixture.controller.clone())
            .await
            .expect_err("terminal child should prevent false cascade success");
        let controller = request_session(&mut app, fixture.controller.clone())
            .await
            .expect("controller should load")
            .expect("controller should exist");
        let orchestration = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_for_controller(&fixture.controller)
            .await
            .expect("orchestration should load")
            .expect("orchestration should exist");
        let tasks = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(fixture.orchestration)
            .await
            .expect("orchestration tasks should load");

        // Assert
        assert!(
            cancel_error
                .to_string()
                .contains("not cancelable in its current state")
        );
        assert_eq!(controller.status, controller_before.status);
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::Canceling.to_string()
        );
        assert_eq!(
            tasks[0].status,
            OrchestrationTaskStatus::Running.to_string()
        );
    }

    #[tokio::test]
    async fn runtime_backend_cancels_reverse_linked_orchestration_child() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let fixture = seed_active_orchestration_child(&mut app, false).await;

        // Act
        request_cancellation(&mut app, fixture.controller.clone())
            .await
            .expect("reverse-linked child cancellation should cascade");
        let child = request_session(&mut app, fixture.child)
            .await
            .expect("child should load")
            .expect("child should exist");
        let tasks = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(fixture.orchestration)
            .await
            .expect("orchestration tasks should load");

        // Assert
        assert_eq!(child.status, SessionStatus::Canceled);
        assert_eq!(
            tasks[0].status,
            OrchestrationTaskStatus::Canceled.to_string()
        );
        assert!(tasks[0].child_session_id.is_none());
    }

    #[tokio::test]
    async fn runtime_backend_inherits_launch_settings_for_regular_and_draft_sessions() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let default_session_model = app.sessions.default_session_model();
        let source_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("source session should be created");
        persist_inherited_launch_settings(&app, &source_session_id).await;

        // Act
        let inherited_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id.clone()),
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("inherited session should be created");
        let inherited_session = request_session(&mut app, inherited_session_id)
            .await
            .expect("inherited session should load")
            .expect("inherited session should exist");
        let inherited_regular_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id.clone()),
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("inherited regular session should be created");
        let inherited_regular_session = request_session(&mut app, inherited_regular_session_id)
            .await
            .expect("inherited regular session should load")
            .expect("inherited regular session should exist");
        let ordinary_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("ordinary session should be created");
        let ordinary_session = request_session(&mut app, ordinary_session_id)
            .await
            .expect("ordinary session should load")
            .expect("ordinary session should exist");

        // Assert
        assert_eq!(
            inherited_session.settings.agent,
            ag_agent::AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5)
        );
        assert_eq!(
            inherited_session.settings.reasoning_level,
            ReasoningLevel::High
        );
        assert_eq!(inherited_session.settings.speed_mode, SpeedMode::Fast);
        assert_eq!(
            inherited_session.settings.permission_mode,
            PermissionMode::ReadOnly
        );
        assert_eq!(
            inherited_session.settings.personality_id.as_deref(),
            Some("inherited-personality")
        );
        assert_eq!(
            inherited_regular_session.settings.agent,
            inherited_session.settings.agent
        );
        assert_eq!(
            inherited_regular_session.settings.reasoning_level,
            inherited_session.settings.reasoning_level
        );
        assert_eq!(
            inherited_regular_session.settings.speed_mode,
            inherited_session.settings.speed_mode
        );
        assert_eq!(
            inherited_regular_session.settings.personality_id.as_deref(),
            Some("inherited-personality")
        );
        assert_eq!(
            ordinary_session.settings.agent.model(),
            default_session_model
        );
        assert_eq!(ordinary_session.settings.speed_mode, SpeedMode::Normal);
        assert_eq!(
            ordinary_session.settings.permission_mode,
            PermissionMode::AutoEdit
        );
        assert_eq!(app.sessions.default_session_model(), default_session_model);
    }

    #[tokio::test]
    async fn runtime_backend_rejects_invalid_permission_mode_retrieval_and_inheritance() {
        // Arrange
        let (mut app, _temp_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let project_id = app.active_project_id();
        let source_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("source session should be created");
        sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = ?")
            .bind(source_session_id.as_str())
            .execute(&pool)
            .await
            .expect("source permission mode should be corrupted");

        // Act
        let retrieval_error = request_session(&mut app, source_session_id.clone())
            .await
            .expect_err("invalid permission mode retrieval should fail");
        let inheritance_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id),
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect_err("invalid permission mode inheritance should fail");

        // Assert
        for error in [retrieval_error, inheritance_error] {
            assert!(matches!(
                error,
                ApiSessionError::InvalidData(message)
                    if message.contains("Unknown permission mode: invalid")
            ));
        }
    }

    #[tokio::test]
    async fn runtime_backend_rejects_cross_project_inheritance() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("develop".to_string()))
            .await
            .expect("inactive project should persist");
        let source_session_id = SessionId::from("inactive-source");
        app.services
            .db()
            .sessions()
            .insert_session(
                &source_session_id,
                "gpt-5.6-sol",
                "develop",
                "Draft",
                inactive_project_id,
            )
            .await
            .expect("inactive source session should persist");

        // Act
        let project_mismatch_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id),
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect_err("cross-project inheritance should fail");

        // Assert
        assert_eq!(
            project_mismatch_error,
            ApiSessionError::Operation(format!(
                "Session `inactive-source` belongs to project `{inactive_project_id}`, not \
                 `{project_id}`"
            ))
        );
    }

    #[tokio::test]
    async fn runtime_backend_rejects_stacked_parent_from_another_project() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("develop".to_string()))
            .await
            .expect("inactive project should persist");
        let parent_session_id = SessionId::from("inactive-parent");
        app.services
            .db()
            .sessions()
            .insert_session(
                &parent_session_id,
                "gpt-5.6-sol",
                "develop",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("inactive parent session should persist");

        // Act
        let project_mismatch_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Stacked {
                    parent_session_id: parent_session_id.clone(),
                },
                project_id,
            },
        )
        .await
        .expect_err("cross-project stacked creation should fail");

        // Assert
        assert_eq!(
            project_mismatch_error,
            ApiSessionError::Operation(format!(
                "Parent session `{parent_session_id}` belongs to project `{inactive_project_id}`, \
                 not `{project_id}`"
            ))
        );
    }

    async fn persist_inherited_launch_settings(app: &App, session_id: &SessionId) {
        app.services
            .db()
            .sessions()
            .update_session_agent_model(session_id, "claude", "claude-sonnet-5")
            .await
            .expect("source agent should update");
        app.services
            .db()
            .sessions()
            .update_session_reasoning_level(session_id, ReasoningLevel::High)
            .await
            .expect("source reasoning should update");
        app.services
            .db()
            .sessions()
            .update_session_speed_mode(session_id, SpeedMode::Fast)
            .await
            .expect("source speed mode should update");
        app.services
            .db()
            .sessions()
            .update_session_permission_mode(session_id, PermissionMode::ReadOnly)
            .await
            .expect("source permission mode should update");
        app.services
            .db()
            .sessions()
            .update_session_personality_id(session_id, Some("inherited-personality".to_string()))
            .await
            .expect("source personality should update");
    }

    #[tokio::test]
    async fn runtime_backend_preserves_workflow_validation_errors() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();

        // Act
        let project_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id: project_id.saturating_add(1),
            },
        )
        .await
        .expect_err("missing project should fail");
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");

        let empty_message_error = request_message(&mut app, session_id.clone(), "  ")
            .await
            .expect_err("empty message should fail");
        let missing_message_error =
            request_message(&mut app, SessionId::from("missing"), "continue")
                .await
                .expect_err("missing session should fail");
        let stale_answers_error = request_question_answers(
            &mut app,
            session_id.clone(),
            AnswerQuestionsRequest {
                answers: vec![QuestionAnswer {
                    answer: "main".to_string(),
                    question: "Which target?".to_string(),
                }],
            },
        )
        .await
        .expect_err("unexpected answer set should fail");
        let cancel_error = request_cancellation(&mut app, SessionId::from("missing"))
            .await
            .expect_err("missing cancellation should fail");
        app.sessions
            .session_at_mut(0)
            .expect("draft session should be loaded")
            .status = SessionStatus::InProgress;
        if let Some(handles) = app.sessions.session_handles().get(&session_id)
            && let Ok(mut status) = handles.status.lock()
        {
            *status = SessionStatus::InProgress;
        }
        request_message(&mut app, session_id.clone(), "queued follow-up")
            .await
            .expect("running session should queue the message");
        let queued_session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Done,
        );
        let terminal_message_error = request_message(&mut app, session_id.clone(), "too late")
            .await
            .expect_err("terminal session should reject messages");
        let merge_error = request_merge(&mut app, session_id.clone())
            .await
            .expect_err("draft session should not merge");
        let missing_review_error =
            request_review_request(&mut app, SessionId::from("missing-review"))
                .await
                .expect_err("missing session should not publish");
        let review_error = request_review_request(&mut app, session_id)
            .await
            .expect_err("draft session should not publish");

        // Assert
        assert!(matches!(project_error, ApiSessionError::Operation(_)));
        assert!(matches!(empty_message_error, ApiSessionError::Operation(_)));
        assert_eq!(missing_message_error, ApiSessionError::NotFound);
        assert!(matches!(stale_answers_error, ApiSessionError::Operation(_)));
        assert_eq!(cancel_error, ApiSessionError::NotFound);
        assert_eq!(queued_session.queued_messages, ["queued follow-up"]);
        assert!(matches!(
            terminal_message_error,
            ApiSessionError::Operation(_)
        ));
        assert!(matches!(merge_error, ApiSessionError::Operation(_)));
        assert_eq!(missing_review_error, ApiSessionError::NotFound);
        assert!(matches!(review_error, ApiSessionError::Operation(_)));
    }

    #[tokio::test]
    async fn runtime_backend_starts_regular_and_staged_draft_messages() {
        // Arrange
        let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app_server = MockAppServerClient::new();
        app_server
            .expect_run_turn()
            .times(2)
            .returning(move |_, _| {
                let turn_started_tx = turn_started_tx.clone();

                Box::pin(async move {
                    let _ = turn_started_tx.send(());

                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        pid: None,
                        provider_conversation_id: None,
                    })
                })
            });
        app_server
            .expect_shutdown_session()
            .times(0..)
            .returning(|_| Box::pin(async {}));
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(app_server));
        let (mut app, _temp_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let project_id = app.active_project_id();
        let regular_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("regular session should be created");
        let draft_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        for session_id in [&regular_session_id, &draft_session_id] {
            app.set_session_model(
                session_id,
                AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            )
            .await
            .expect("session model should update");
        }

        // Act
        request_message(&mut app, regular_session_id.clone(), "regular prompt")
            .await
            .expect("regular session should start");
        request_message(&mut app, draft_session_id.clone(), "draft prompt")
            .await
            .expect("staged draft should start");
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
                .await
                .expect("agent turn should start")
                .expect("agent turn signal should be available");
        }

        // Assert
        assert_eq!(
            app.sessions
                .session_for_id(&regular_session_id)
                .map(|session| session.prompt.as_str()),
            Some("regular prompt")
        );
        assert_eq!(
            app.sessions
                .session_for_id(&draft_session_id)
                .map(|session| session.prompt.as_str()),
            Some("draft prompt")
        );
    }

    #[tokio::test]
    async fn runtime_backend_loads_inherited_creation_before_acknowledging_event_backlog() {
        // Arrange
        let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app_server = MockAppServerClient::new();
        app_server.expect_run_turn().once().returning(move |_, _| {
            let turn_started_tx = turn_started_tx.clone();

            Box::pin(async move {
                let _ = turn_started_tx.send(());

                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
        app_server
            .expect_shutdown_session()
            .times(0..)
            .returning(|_| Box::pin(async {}));
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(app_server));
        let (mut app, _temp_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let project_id = app.active_project_id();
        let source_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("source session should be created");
        for _ in 0..crate::app::reducer::APP_EVENT_DRAIN_BUDGET {
            app.services
                .emit_app_event(crate::app::AppEvent::RefreshProjects);
        }

        // Act
        let inherited_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id),
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("inherited session should be created");
        let send_result =
            request_message(&mut app, inherited_session_id.clone(), "inherited prompt").await;
        tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
            .await
            .expect("agent turn should start")
            .expect("agent turn signal should be available");

        // Assert
        assert_eq!(send_result, Ok(()));
        assert_eq!(
            app.sessions
                .session_for_id(&inherited_session_id)
                .map(|session| session.prompt.as_str()),
            Some("inherited prompt")
        );
    }

    #[tokio::test]
    async fn runtime_backend_queues_one_question_resume_behind_turn_entering_question_state() {
        // Arrange
        let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let first_turn_release = Arc::new(tokio::sync::Notify::new());
        let app_server =
            question_transition_app_server(Arc::clone(&first_turn_release), turn_started_tx);
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(app_server));
        let (mut app, _temp_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        app.set_session_model(
            &session_id,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        )
        .await
        .expect("session model should update");
        request_message(&mut app, session_id.clone(), "initial prompt")
            .await
            .expect("initial turn should start");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
                .await
                .expect("initial turn should start")
                .expect("initial turn kind should be available"),
            AgentRequestKind::SessionStart
        );
        app.services
            .db()
            .sessions()
            .update_session_questions(
                &session_id,
                r#"[{"text":"Current question?","options":[]}]"#,
            )
            .await
            .expect("current questions should persist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::InProgress,
        );
        let cached_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("session should be loaded");
        cached_session.questions = vec![QuestionItem::new("Stale question?")];
        assert_eq!(cached_session.status, SessionStatus::InProgress);

        // Act
        let answer_result = request_question_answers(
            &mut app,
            session_id.clone(),
            current_question_answer("Current answer"),
        )
        .await;
        let duplicate_answer_error = request_question_answers(
            &mut app,
            session_id.clone(),
            current_question_answer("Duplicate answer"),
        )
        .await
        .expect_err("the persisted question set should be consumed once");
        let queued_messages_before_transition = app
            .sessions
            .session_for_id(&session_id)
            .expect("session should stay loaded")
            .queued_messages
            .clone();
        first_turn_release.notify_one();
        let resumed_turn_kind =
            tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
                .await
                .expect("question answer should resume")
                .expect("resumed turn kind should be available");
        let session = request_session(&mut app, session_id)
            .await
            .expect("session should load")
            .expect("session should exist");

        // Assert
        assert_eq!(answer_result, Ok(()));
        assert_eq!(
            duplicate_answer_error,
            ApiSessionError::Operation("Session has no questions to answer".to_string())
        );
        assert_eq!(resumed_turn_kind, AgentRequestKind::SessionResume);
        assert_eq!(queued_messages_before_transition, []);
        assert_eq!(session.questions, [] as [ag_protocol::QuestionItem; 0]);
        assert_eq!(session.queued_messages, [] as [std::string::String; 0]);
        assert_eq!(clarification_answer_count(&session), 1);
    }

    #[tokio::test]
    async fn controller_question_answers_proxy_to_the_managed_worker() {
        // Arrange
        let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let first_turn_release = Arc::new(tokio::sync::Notify::new());
        let app_server =
            question_transition_app_server(Arc::clone(&first_turn_release), turn_started_tx);
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(app_server));
        let (mut app, _temp_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let fixture = seed_active_orchestration_child(&mut app, true).await;
        app.set_session_model(
            &fixture.child,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        )
        .await
        .expect("managed worker model should update");
        let coordinator_service = app.coordinator_session_service();
        let child_session_id = fixture.child.clone();
        app.drive_session_request(async move {
            coordinator_service
                .send_message(&child_session_id, "initial prompt".to_string())
                .await
        })
        .await
        .expect("coordinator should start the managed worker");
        tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
            .await
            .expect("managed worker turn should start")
            .expect("managed worker turn signal should be available");
        let questions_json = r#"[{"text":"Current question?","options":[]}]"#;
        app.services
            .db()
            .sessions()
            .update_session_questions(&fixture.child, questions_json)
            .await
            .expect("managed worker questions should persist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &fixture.child,
            SessionStatus::InProgress,
        );
        let tasks = app
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(fixture.orchestration)
            .await
            .expect("campaign tasks should load");
        app.services
            .db()
            .orchestrations()
            .update_orchestration_task_status(
                tasks[0].id,
                &OrchestrationTaskStatus::WaitingForInput.to_string(),
                None,
            )
            .await
            .expect("managed worker task should wait for input");
        app.services
            .db()
            .orchestrations()
            .surface_orchestration_questions(fixture.orchestration, tasks[0].id, questions_json)
            .await
            .expect("managed worker questions should surface");

        // Act
        let answer_result = request_question_answers(
            &mut app,
            fixture.controller.clone(),
            current_question_answer("Current answer"),
        )
        .await;
        assert_eq!(answer_result, Ok(()));
        first_turn_release.notify_one();
        let resumed_turn_kind =
            tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
                .await
                .expect("proxied answer should resume the worker")
                .expect("resumed worker turn should be available");
        let controller = request_session(&mut app, fixture.controller)
            .await
            .expect("controller should load")
            .expect("controller should exist");

        // Assert
        assert_eq!(resumed_turn_kind, AgentRequestKind::SessionResume);
        assert_eq!(controller.questions, [] as [ag_protocol::QuestionItem; 0]);
    }

    #[tokio::test]
    async fn orchestration_question_target_requires_an_available_relayed_child() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let fixture = seed_active_orchestration_child(&mut app, true).await;

        // Act
        let missing_relay = app
            .orchestration_question_target(&fixture.controller)
            .await
            .expect("an orchestration without a relay should remain available");
        app.services
            .db()
            .orchestrations()
            .update_orchestration_task_status(
                fixture.task,
                &OrchestrationTaskStatus::WaitingForInput.to_string(),
                None,
            )
            .await
            .expect("managed worker task should wait for input");
        let surfaced = app
            .services
            .db()
            .orchestrations()
            .surface_orchestration_questions(
                fixture.orchestration,
                fixture.task,
                r#"[{"text":"Current question?","options":[]}]"#,
            )
            .await
            .expect("managed worker questions should surface");
        let detached = app
            .services
            .db()
            .orchestrations()
            .detach_orchestration_child(&fixture.child)
            .await
            .expect("managed worker should detach");
        let unavailable_relay_error = app
            .orchestration_question_target(&fixture.controller)
            .await
            .expect_err("a relay without its child should fail explicitly");

        // Assert
        assert_eq!(missing_relay, None);
        assert!(surfaced);
        assert!(detached);
        assert_eq!(
            unavailable_relay_error,
            ApiSessionError::Operation(format!(
                "Orchestration question relay references unavailable task `{}`",
                fixture.task
            ))
        );
    }

    #[tokio::test]
    async fn runtime_backend_does_not_persist_question_answer_when_worker_enqueue_fails() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        app.services
            .db()
            .sessions()
            .update_session_questions(
                &session_id,
                r#"[{"text":"Current question?","options":[]}]"#,
            )
            .await
            .expect("current questions should persist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::InProgress,
        );

        // Act
        let answer_error = request_question_answers(
            &mut app,
            session_id.clone(),
            current_question_answer("Current answer"),
        )
        .await
        .expect_err("missing active worker should reject question answers");
        let session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");

        // Assert
        assert_eq!(
            answer_error,
            ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept question answers in status `InProgress`"
            ))
        );
        assert_eq!(session.questions, [QuestionItem::new("Current question?")]);
        assert_eq!(session.messages, [] as [ag_session::SessionMessage; 0]);
    }

    #[tokio::test]
    async fn runtime_backend_restores_claimed_questions_when_resume_is_rejected() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        app.services
            .db()
            .sessions()
            .update_session_questions(
                &session_id,
                r#"[{"text":"Current question?","options":[]}]"#,
            )
            .await
            .expect("current questions should persist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Merged,
        );

        // Act
        let answer_error = request_question_answers(
            &mut app,
            session_id.clone(),
            AnswerQuestionsRequest {
                answers: vec![QuestionAnswer {
                    answer: "Current answer".to_string(),
                    question: "Current question?".to_string(),
                }],
            },
        )
        .await
        .expect_err("a read-only session should reject question answers");
        let session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");

        // Assert
        assert_eq!(
            answer_error,
            ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept question answers in status `Merged`"
            ))
        );
        assert_eq!(session.questions, [QuestionItem::new("Current question?")]);
    }

    #[tokio::test]
    async fn project_creation_context_requires_a_base_branch() {
        // Arrange
        let (app, _temp_dir) = crate::test_support::new_test_app().await;

        // Act
        let active_error = app.api_project_creation_context(None).err();

        // Assert
        let expected =
            ApiSessionError::Operation("Git branch is required to create a session".to_string());
        assert_eq!(active_error.as_ref(), Some(&expected));
    }

    #[tokio::test]
    async fn finishing_api_creation_schedules_registration_retry_after_load_failure() {
        // Arrange
        let (mut app, _temp_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let project_id = app.active_project_id();
        app.services
            .db()
            .sessions()
            .insert_session(
                "persisted-session",
                "gpt-5.6-sol",
                "main",
                "Draft",
                project_id,
            )
            .await
            .expect("session should persist before registration");
        sqlx::query("DROP TABLE session")
            .execute(&pool)
            .await
            .expect("session reads should fail");

        // Act
        app.finish_api_session_creation("persisted-session").await;
        let retry_event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = app
                    .next_app_event()
                    .await
                    .expect("app event channel should remain open");
                if event == AppEvent::RefreshSessions {
                    break event;
                }
            }
        })
        .await
        .expect("registration retry should be scheduled");

        // Assert
        assert_eq!(retry_event, AppEvent::RefreshSessions);
    }

    #[tokio::test]
    async fn runtime_backend_reports_session_read_failures() {
        // Arrange
        let (mut session_query_app, _session_temp_dir, session_pool) =
            crate::test_support::new_git_test_app_with_pool().await;
        let (mut message_query_app, _message_temp_dir, message_pool) =
            crate::test_support::new_git_test_app_with_pool().await;
        let message_project_id = message_query_app.active_project_id();
        let message_session_id = request_session_creation(
            &mut message_query_app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id: message_project_id,
            },
        )
        .await
        .expect("session should be created");
        sqlx::query("DROP TABLE session")
            .execute(&session_pool)
            .await
            .expect("session table should be dropped");
        sqlx::query("DROP TABLE session_message")
            .execute(&message_pool)
            .await
            .expect("message table should be dropped");

        // Act
        let session_query_error =
            request_session(&mut session_query_app, SessionId::from("missing"))
                .await
                .expect_err("session query should fail");
        let message_query_error = request_session(&mut message_query_app, message_session_id)
            .await
            .expect_err("message query should fail");

        // Assert
        assert!(matches!(session_query_error, ApiSessionError::Operation(_)));
        assert!(matches!(message_query_error, ApiSessionError::Operation(_)));
    }

    #[test]
    fn structured_question_answers_require_current_non_empty_pairs() {
        // Arrange
        let questions = vec![
            QuestionItem::new("Which target?"),
            QuestionItem::new("Run tests?"),
        ];
        let valid_answers = vec![
            QuestionAnswer {
                answer: "main".to_string(),
                question: "Which target?".to_string(),
            },
            QuestionAnswer {
                answer: "yes".to_string(),
                question: "Run tests?".to_string(),
            },
        ];
        let mut stale_answers = valid_answers.clone();
        stale_answers[1].question = "Different question".to_string();
        let mut empty_answers = valid_answers.clone();
        empty_answers[0].answer = " ".to_string();

        // Act
        let valid_result = validate_question_answers(&questions, &valid_answers);
        let no_questions_error =
            validate_question_answers(&[], &[]).expect_err("empty question set should fail");
        let missing_error = validate_question_answers(&questions, &valid_answers[..1])
            .expect_err("missing answer should fail");
        let stale_error = validate_question_answers(&questions, &stale_answers)
            .expect_err("stale answer should fail");
        let empty_error = validate_question_answers(&questions, &empty_answers)
            .expect_err("empty answer should fail");
        let message = question_answer_message(&valid_answers);

        // Assert
        assert_eq!(valid_result, Ok(()));
        assert_eq!(
            no_questions_error,
            ApiSessionError::Operation("Session has no questions to answer".to_string())
        );
        assert_eq!(
            missing_error,
            ApiSessionError::Operation("Expected 2 question answers, received 1".to_string())
        );
        assert_eq!(
            stale_error,
            ApiSessionError::Operation("Question answer 2 is stale".to_string())
        );
        assert_eq!(
            empty_error,
            ApiSessionError::Operation("Question answer 1 is empty".to_string())
        );
        assert_eq!(
            message,
            "Clarifications:\n1. Q: Which target?\n   A: main\n2. Q: Run tests?\n   A: yes"
        );
    }

    #[tokio::test]
    async fn coordinator_messages_require_content_and_operation_id() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
        let session_id = SessionId::from("missing");
        let (mut busy_app, _busy_temp_dir) = crate::test_support::new_test_app().await;
        busy_app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .status(Status::InProgress)
                .build(),
        );
        let (mut unbound_app, _unbound_temp_dir) = crate::test_support::new_test_app().await;
        unbound_app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .status(Status::Review)
                .build(),
        );
        let existing_session_id = SessionId::from("session-id");

        // Act
        let empty_message_error = app
            .submit_api_coordinator_message(
                &session_id,
                CoordinatorMessageRequest {
                    message: " ".to_string(),
                    operation_id: "rollup-1".to_string(),
                    visibility: CoordinatorMessageVisibility::Hidden,
                },
            )
            .await
            .expect_err("empty coordinator message should fail");
        let empty_operation_error = app
            .submit_api_coordinator_message(
                &session_id,
                CoordinatorMessageRequest {
                    message: "Roll up".to_string(),
                    operation_id: " ".to_string(),
                    visibility: CoordinatorMessageVisibility::Hidden,
                },
            )
            .await
            .expect_err("empty coordinator operation id should fail");
        let busy_error = busy_app
            .submit_api_coordinator_message(
                &existing_session_id,
                CoordinatorMessageRequest {
                    message: "Roll up".to_string(),
                    operation_id: "rollup-busy".to_string(),
                    visibility: CoordinatorMessageVisibility::Hidden,
                },
            )
            .await
            .expect_err("busy coordinator should reject a roll-up");
        let enqueue_error = unbound_app
            .submit_api_coordinator_message(
                &existing_session_id,
                CoordinatorMessageRequest {
                    message: "Roll up".to_string(),
                    operation_id: "rollup-unbound".to_string(),
                    visibility: CoordinatorMessageVisibility::Hidden,
                },
            )
            .await
            .expect_err("coordinator without a worker should reject a roll-up");

        // Assert
        assert_eq!(
            empty_message_error,
            ApiSessionError::Operation("Cannot submit an empty coordinator message".to_string())
        );
        assert_eq!(
            empty_operation_error,
            ApiSessionError::Operation(
                "Cannot submit a coordinator message without an operation id".to_string()
            )
        );
        assert_eq!(
            busy_error,
            ApiSessionError::Operation(
                "Session `session-id` cannot accept a coordinator message in status `InProgress`"
                    .to_string()
            )
        );
        assert_eq!(
            enqueue_error,
            ApiSessionError::Operation(
                "Session `session-id` could not enqueue the coordinator message".to_string()
            )
        );
    }

    #[test]
    fn question_restore_error_preserves_both_failures() {
        // Arrange
        let send_error =
            ApiSessionError::Operation("Session cannot accept question answers".to_string());

        // Act
        let error = question_restore_error(&send_error, &"database unavailable");

        // Assert
        assert_eq!(
            error,
            ApiSessionError::Operation(
                "Session cannot accept question answers; failed to restore session questions: \
                 database unavailable"
                    .to_string()
            )
        );
    }
}
