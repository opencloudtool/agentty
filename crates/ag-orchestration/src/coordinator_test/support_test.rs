use super::*;

#[derive(Clone, Default)]
pub(super) struct TestSessionBackend {
    pub(super) state: Arc<Mutex<TestSessionBackendState>>,
}

#[derive(Default)]
pub(super) struct TestSessionBackendState {
    pub(super) accepted_coordinator_operations: HashSet<String>,
    pub(super) calls: Vec<String>,
    pub(super) cancel_errors: VecDeque<SessionError>,
    pub(super) create_results: VecDeque<SessionId>,
    pub(super) merge_errors: VecDeque<SessionError>,
    pub(super) review_errors: VecDeque<SessionError>,
    pub(super) send_errors: VecDeque<SessionError>,
}

impl TestSessionBackend {
    pub(super) fn push_create_result(&self, session_id: impl Into<SessionId>) {
        self.state
            .lock()
            .expect("test backend state should remain available")
            .create_results
            .push_back(session_id.into());
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("test backend state should remain available")
            .calls
            .clone()
    }

    pub(super) fn push_cancel_error(&self, error: SessionError) {
        self.state
            .lock()
            .expect("test backend state should remain available")
            .cancel_errors
            .push_back(error);
    }

    pub(super) fn push_send_error(&self, error: SessionError) {
        self.state
            .lock()
            .expect("test backend state should remain available")
            .send_errors
            .push_back(error);
    }

    pub(super) fn push_merge_error(&self, error: SessionError) {
        self.state
            .lock()
            .expect("test backend state should remain available")
            .merge_errors
            .push_back(error);
    }

    pub(super) fn push_review_error(&self, error: SessionError) {
        self.state
            .lock()
            .expect("test backend state should remain available")
            .review_errors
            .push_back(error);
    }

    pub(super) fn service(&self) -> SessionService {
        SessionService::new(Arc::new(self.clone()))
    }
}

#[async_trait]
impl SessionBackend for TestSessionBackend {
    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        let mut state = self
            .state
            .lock()
            .expect("test backend state should remain available");
        state.calls.push(format!("create:{:?}", request.mode));

        state
            .create_results
            .pop_front()
            .ok_or_else(|| SessionError::Operation("missing create result".to_string()))
    }

    async fn get_session(&self, _session_id: &SessionId) -> Result<Option<Session>, SessionError> {
        Ok(None)
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        let mut state = self
            .state
            .lock()
            .expect("test backend state should remain available");
        state.calls.push(format!("send:{session_id}:{message}"));

        state.send_errors.pop_front().map_or(Ok(()), Err)
    }

    async fn submit_coordinator_message(
        &self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), SessionError> {
        let mut state = self
            .state
            .lock()
            .expect("test backend state should remain available");
        state.calls.push(format!(
            "rollup-attempt:{session_id}:{}",
            request.operation_id
        ));
        if state
            .accepted_coordinator_operations
            .insert(request.operation_id)
        {
            state
                .calls
                .push(format!("rollup:{session_id}:{}", request.message));
        }

        Ok(())
    }

    async fn answer_questions(
        &self,
        _session_id: &SessionId,
        _request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        Ok(())
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let mut state = self
            .state
            .lock()
            .expect("test backend state should remain available");
        state.calls.push(format!("cancel:{session_id}"));

        state.cancel_errors.pop_front().map_or(Ok(()), Err)
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let mut state = self
            .state
            .lock()
            .expect("test backend state should remain available");
        state.calls.push(format!("merge:{session_id}"));

        state.merge_errors.pop_front().map_or(Ok(()), Err)
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        let mut state = self
            .state
            .lock()
            .expect("test backend state should remain available");
        state.calls.push(format!("review:{session_id}"));
        if let Some(error) = state.review_errors.pop_front() {
            return Err(error);
        }

        Ok(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#1".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "child".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Campaign task".to_string(),
                web_url: "https://example.test/review/1".to_string(),
            },
        })
    }
}

pub(super) fn orchestration(max_parallelism: i64) -> SessionOrchestrationRow {
    SessionOrchestrationRow {
        controller_project_id: 1,
        controller_session_id: "controller".to_string(),
        goal_statement: "Complete the campaign".to_string(),
        id: 1,
        max_parallelism,
        relayed_question_task_id: None,
        status: OrchestrationStatus::Running.to_string(),
        verification_generation: 0,
    }
}

pub(super) fn task(
    id: i64,
    task_key: &str,
    status: OrchestrationTaskStatus,
    child_session_id: Option<&str>,
) -> SessionOrchestrationTaskRow {
    let child_status = child_session_id.map(|_| match status {
        OrchestrationTaskStatus::WaitingForInput => SessionStatus::Question,
        OrchestrationTaskStatus::Ready
        | OrchestrationTaskStatus::Reported
        | OrchestrationTaskStatus::ContinuationPending
        | OrchestrationTaskStatus::AwaitingIntegration
        | OrchestrationTaskStatus::Merging
        | OrchestrationTaskStatus::ReviewRequested
        | OrchestrationTaskStatus::IntegrationFailed
        | OrchestrationTaskStatus::Reviewing => SessionStatus::Review,
        OrchestrationTaskStatus::Integrated | OrchestrationTaskStatus::Detached => {
            SessionStatus::Done
        }
        OrchestrationTaskStatus::Failed | OrchestrationTaskStatus::Canceled => {
            SessionStatus::Canceled
        }
        OrchestrationTaskStatus::Proposed
        | OrchestrationTaskStatus::Planned
        | OrchestrationTaskStatus::Creating
        | OrchestrationTaskStatus::Running
        | OrchestrationTaskStatus::ReviewApplying => SessionStatus::InProgress,
    });
    let has_completed_review = status == OrchestrationTaskStatus::Ready;

    SessionOrchestrationTaskRow {
        acceptance_criteria: format!(r#"["{task_key} is complete"]"#),
        area_violations: "[]".to_string(),
        areas_compliant: None,
        attempt_count: i64::from(child_session_id.is_some()),
        child_added_lines: 3,
        child_answer: None,
        child_deleted_lines: 1,
        child_focused_review_status: has_completed_review
            .then(|| FocusedReviewStatus::Ready.to_string()),
        child_focused_review_text: has_completed_review
            .then(|| "## Review\n\n### Suggestions\n\n- None".to_string()),
        child_has_diff: child_session_id.map(|_| true),
        child_input_tokens: i64::from(child_session_id.is_some()) * 10,
        child_output_tokens: i64::from(child_session_id.is_some()) * 5,
        child_questions: None,
        child_session_id: child_session_id.map(str::to_string),
        child_status: child_status.map(|status| status.to_string()),
        continuation_generation: 0,
        continuation_prompt: None,
        id,
        infrastructure_retry_count: 0,
        kind: OrchestrationTaskKind::Implementation.to_string(),
        last_error: None,
        merge_position: id,
        prompt: format!("Implement {task_key}"),
        research_report: None,
        result_summary: None,
        review_iteration: 0,
        status: status.to_string(),
        task_key: task_key.to_string(),
        title: task_key.to_string(),
        touched_areas: format!("[\"{task_key}/\"]"),
        verification_reason: None,
        verification_verdict: None,
    }
}

pub(super) fn with_child_observation(
    mut task: SessionOrchestrationTaskRow,
    status: SessionStatus,
    answer: Option<&str>,
) -> SessionOrchestrationTaskRow {
    task.child_status = Some(status.to_string());
    task.child_answer = answer.map(str::to_string);

    task
}

pub(super) fn focused_review_task(
    id: i64,
    task_key: &str,
    child_session_id: &str,
    status: FocusedReviewStatus,
    text: Option<&str>,
) -> SessionOrchestrationTaskRow {
    let mut task = task(
        id,
        task_key,
        OrchestrationTaskStatus::Reviewing,
        Some(child_session_id),
    );
    task.child_focused_review_status = Some(status.to_string());
    task.child_focused_review_text = text.map(str::to_string);

    task
}

pub(super) fn review_applying_task() -> SessionOrchestrationTaskRow {
    let mut task = task(
        7,
        "review",
        OrchestrationTaskStatus::ReviewApplying,
        Some("child-review"),
    );
    task.continuation_generation = 2;
    task.continuation_prompt = Some("Verify then apply".to_string());

    task
}

pub(super) fn mock_task_snapshots(
    mock: &mut MockOrchestrationRepository,
    snapshots: Vec<Vec<SessionOrchestrationTaskRow>>,
) {
    let snapshot_count = snapshots.len();
    let snapshots = Arc::new(Mutex::new(VecDeque::from(snapshots)));
    mock.expect_load_orchestration_tasks()
        .times(snapshot_count)
        .returning(move |_| {
            Ok(snapshots
                .lock()
                .expect("task snapshots should remain available")
                .pop_front()
                .expect("expected another task snapshot"))
        });
}

pub(super) type TaskStatusUpdates = Arc<Mutex<Vec<(i64, String, Option<String>)>>>;

pub(super) fn coordinator_with_status_recorder(
    backend: &TestSessionBackend,
) -> (OrchestrationCoordinator, TaskStatusUpdates) {
    let updates = Arc::new(Mutex::new(Vec::new()));
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_update_orchestration_task_status()
        .times(0..)
        .returning({
            let updates = Arc::clone(&updates);

            move |id, status, error| {
                updates
                    .lock()
                    .expect("status updates should remain available")
                    .push((id, status.to_string(), error));

                Ok(())
            }
        });
    let (event_tx, _event_rx) = mpsc::unbounded_channel();

    (
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service()),
        updates,
    )
}

#[derive(Default)]
pub(super) struct OneShotSchedule {
    pub(super) has_fired: bool,
}

#[async_trait]
impl OrchestrationSchedule for OneShotSchedule {
    async fn wait_for_reconciliation(&mut self) {
        if self.has_fired {
            std::future::pending::<()>().await;
        }
        self.has_fired = true;
    }
}

pub(super) fn expect_rollup_completion_failure_then_success(
    mock: &mut MockOrchestrationRepository,
) {
    let update_attempt = Arc::new(Mutex::new(0_u8));
    mock.expect_complete_orchestration_rollup()
        .withf(|id| *id == 1)
        .times(2)
        .returning({
            let update_attempt = Arc::clone(&update_attempt);

            move |_| {
                let mut update_attempt = update_attempt
                    .lock()
                    .expect("update attempt should remain available");
                *update_attempt += 1;
                if *update_attempt == 1 {
                    return Err(DbError::Io(std::io::Error::other(
                        "injected post-submit failure",
                    )));
                }

                Ok(true)
            }
        });
}

pub(super) async fn controller_database() -> (AppRepositories, i64) {
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/orchestration-project", Some("main".to_string()))
        .await
        .expect("failed to create orchestration test project");
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "controller",
            is_draft: false,
            model: AgentKind::Codex.default_model().as_str(),
            orchestration_task_id: None,
            parent_session_id: None,
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("Orchestrator"),
            speed_mode: SpeedMode::Normal,
            status: "Review",
        })
        .await
        .expect("failed to insert controller session");

    (database, project_id)
}

pub(super) fn subtask(task_key: &str, touched_areas: &[&str]) -> SubtaskItem {
    SubtaskItem {
        acceptance_criteria: vec![format!("{task_key} is complete")],
        kind: SubtaskKind::Implementation,
        prompt: format!("Implement {task_key}"),
        task_key: task_key.to_string(),
        title: task_key.to_string(),
        touched_areas: touched_areas
            .iter()
            .map(|area| (*area).to_string())
            .collect(),
    }
}

pub(super) fn research_subtask(task_key: &str) -> SubtaskItem {
    SubtaskItem {
        acceptance_criteria: vec![format!("{task_key} questions are answered")],
        kind: SubtaskKind::Research,
        prompt: format!("Inspect {task_key}"),
        task_key: task_key.to_string(),
        title: format!("{task_key} research"),
        touched_areas: vec!["**".to_string()],
    }
}

pub(super) async fn persist_approved_two_task_plan(
    database: &AppRepositories,
) -> (
    SessionOrchestrationRow,
    Vec<SessionOrchestrationTaskRow>,
    AgentResponse,
    OrchestrationSessionMetadata,
) {
    persist_approved_plan(
        database,
        vec![
            subtask("protocol", &["crates/ag-protocol/"]),
            subtask("ui", &["crates/agentty/src/ui/"]),
        ],
    )
    .await
}

pub(super) async fn persist_approved_plan(
    database: &AppRepositories,
    subtasks: Vec<SubtaskItem>,
) -> (
    SessionOrchestrationRow,
    Vec<SessionOrchestrationTaskRow>,
    AgentResponse,
    OrchestrationSessionMetadata,
) {
    let mut response = AgentResponse::plain("Plan");
    response.subtasks = subtasks;
    persist_controller_plan(database, "controller", &mut response)
        .await
        .expect("plan should persist");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load plan")
        .expect("plan should exist");
    let tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("failed to load tasks");
    approve_orchestration(database.orchestrations(), "controller", None)
        .await
        .expect("approval should start orchestration");
    let project_id = database
        .sessions()
        .load_session("controller")
        .await
        .expect("controller should load")
        .and_then(|controller| controller.project_id)
        .expect("controller should belong to a project");
    let metadata = session_metadata_for_project(database, project_id)
        .await
        .remove("controller")
        .expect("controller metadata should load");

    (orchestration, tasks, response, metadata)
}

pub(super) async fn insert_managed_child(
    database: &AppRepositories,
    project_id: i64,
    task_id: i64,
    child_session_id: &str,
) {
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: child_session_id,
            is_draft: false,
            model: AgentKind::Codex.default_model().as_str(),
            orchestration_task_id: Some(task_id),
            parent_session_id: None,
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("OrchestrationWorker"),
            speed_mode: SpeedMode::Normal,
            status: "Review",
        })
        .await
        .expect("failed to insert managed child");
    assert!(
        database
            .orchestrations()
            .link_orchestration_task_child(task_id, child_session_id)
            .await
            .expect("failed to link managed child")
    );
}

pub(super) async fn seed_verifying_tasks(
    database: &AppRepositories,
    orchestration: &SessionOrchestrationRow,
    tasks: &[SessionOrchestrationTaskRow],
) {
    database
        .orchestrations()
        .update_orchestration_status(
            orchestration.id,
            &OrchestrationStatus::AwaitingApproval.to_string(),
        )
        .await
        .expect("failed to reopen configuration gate");
    for task in tasks {
        database
            .orchestrations()
            .update_orchestration_task_status(
                task.id,
                &OrchestrationTaskStatus::Ready.to_string(),
                None,
            )
            .await
            .expect("failed to settle task");
    }
    database
        .orchestrations()
        .update_orchestration_status(
            orchestration.id,
            &OrchestrationStatus::Verifying.to_string(),
        )
        .await
        .expect("failed to start verification");
}

pub(super) fn assert_reconciled_rollup(
    backend: &TestSessionBackend,
    status_updates: &Arc<Mutex<Vec<(i64, String)>>>,
) {
    assert_eq!(
        *status_updates
            .lock()
            .expect("status updates should remain available"),
        vec![
            (2, OrchestrationTaskStatus::Failed.to_string()),
            (1, OrchestrationTaskStatus::Ready.to_string()),
        ]
    );
    let rollup = backend
        .calls()
        .into_iter()
        .find(|call| call.starts_with("rollup:controller:"))
        .expect("settled tasks should submit a rollup");
    assert!(rollup.contains("Task `protocol`"));
    assert!(rollup.contains("Task `ui`"));
    assert!(rollup.contains("20 input, 10 output"));
    assert!(rollup.contains("Integration order"));
}
