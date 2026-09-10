use super::*;

#[tokio::test]
async fn approval_reports_unavailable_when_another_actor_advances_the_campaign() {
    // Arrange
    for phase in [
        OrchestrationStatus::AwaitingApproval,
        OrchestrationStatus::AwaitingIntegration,
    ] {
        let mut repository = MockOrchestrationRepository::new();
        let mut snapshot = orchestration(2);
        snapshot.status = phase.to_string();
        repository
            .expect_load_orchestration_for_controller()
            .withf(|id| id == "controller")
            .once()
            .return_once(move |_| Ok(Some(snapshot)));
        if phase == OrchestrationStatus::AwaitingApproval {
            repository
                .expect_approve_orchestration_plan()
                .withf(|id| *id == 1)
                .once()
                .returning(|_| Ok(false));
        } else {
            repository
                .expect_load_orchestration_tasks()
                .withf(|id| *id == 1)
                .once()
                .returning(|_| Ok(Vec::new()));
            repository
                .expect_approve_orchestration_integration()
                .withf(|id, approach| *id == 1 && *approach == IntegrationApproach::LocalMerge)
                .once()
                .returning(|_, _| Ok(false));
        }

        // Act
        let outcome = approve_orchestration(
            &repository,
            "controller",
            Some(IntegrationApproach::LocalMerge),
        )
        .await
        .expect("stale approval is not a repository failure");

        // Assert
        assert_eq!(outcome, OrchestrationApprovalOutcome::Unavailable);
    }
}

#[tokio::test]
async fn invalid_active_follow_up_preserves_the_persisted_plan_and_requests_revision() {
    // Arrange
    let (database, _) = controller_database().await;
    let (campaign, _, _, _) = persist_approved_plan(
        &database,
        vec![subtask("protocol", &[]), subtask("ui", &[])],
    )
    .await;
    let before = database
        .orchestrations()
        .load_orchestration_tasks(campaign.id)
        .await
        .expect("tasks should load");
    let mut invalid = subtask("protocol", &[]);
    invalid.prompt.clear();
    let mut response = AgentResponse::plain("Follow up");
    response.subtasks = vec![invalid];

    // Act
    persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect("invalid follow-up should ask for revision");

    // Assert
    assert_eq!(response.subtasks, [] as [SubtaskItem; 0]);
    assert_eq!(response.questions.len(), 1);
    assert_eq!(
        database
            .orchestrations()
            .load_orchestration_tasks(campaign.id)
            .await
            .expect("tasks should still load"),
        before,
    );
}

#[tokio::test]
async fn unrecognized_campaign_phase_does_not_issue_session_mutations() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let mut snapshot = orchestration(2);
    snapshot.status = "Unrecognized".to_string();
    repository
        .expect_load_active_orchestrations()
        .once()
        .return_once(move || Ok(vec![snapshot]));
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    let result = coordinator.reconcile_once().await;

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(backend.calls(), [] as [String; 0]);
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn repeated_terminal_result_does_not_rewrite_the_task_summary() {
    // Arrange
    let backend = TestSessionBackend::default();
    let repository = MockOrchestrationRepository::new();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut completed = task(1, "protocol", OrchestrationTaskStatus::Ready, Some("child"));
    completed.child_status = Some(SessionStatus::Done.to_string());
    completed.child_answer = Some("Completed".to_string());
    completed.result_summary = Some("Completed".to_string());
    let before = completed.clone();

    // Act
    coordinator
        .reconcile_task(&mut completed)
        .await
        .expect("first observation");
    coordinator
        .reconcile_task(&mut completed)
        .await
        .expect("repeated observation");

    // Assert
    assert_eq!(completed, before);
    assert_eq!(backend.calls(), [] as [String; 0]);
}
