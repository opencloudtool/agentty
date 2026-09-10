use super::*;

#[tokio::test]
async fn parked_plan_reconciles_live_tasks_before_emitting_status() {
    // Arrange
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_orchestration_tasks()
        .once()
        .returning(|_| {
            Ok(vec![
                with_child_observation(
                    task(
                        1,
                        "running",
                        OrchestrationTaskStatus::Running,
                        Some("child-running"),
                    ),
                    SessionStatus::InProgress,
                    None,
                ),
                with_child_observation(
                    task(
                        2,
                        "waiting",
                        OrchestrationTaskStatus::WaitingForInput,
                        Some("child-waiting"),
                    ),
                    SessionStatus::Question,
                    None,
                ),
            ])
        });
    let backend = TestSessionBackend::default();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut campaign = orchestration(2);
    campaign.status = OrchestrationStatus::AwaitingApproval.to_string();

    // Act
    coordinator
        .reconcile_parked_plan(&campaign)
        .await
        .expect("parked plan should reconcile");

    // Assert
    assert!(matches!(
        event_rx.try_recv(),
        Ok(OrchestrationEvent::ProgressUpdated { .. })
    ));
}

#[tokio::test]
async fn coordinator_test_backend_covers_unneeded_session_ports() {
    // Arrange
    let backend = TestSessionBackend::default();
    let session_id = SessionId::from("unused");

    // Act / Assert
    assert!(
        backend
            .get_session(&session_id)
            .await
            .expect("session lookup should succeed")
            .is_none()
    );
    backend
        .answer_questions(
            &session_id,
            AnswerQuestionsRequest {
                answers: Vec::new(),
            },
        )
        .await
        .expect("question answer should succeed");
    backend
        .cancel_session(&session_id)
        .await
        .expect("cancellation should succeed");
    backend
        .merge_session(&session_id)
        .await
        .expect("merge should succeed");
    assert!(backend.create_review_request(&session_id).await.is_ok());
}

#[tokio::test]
async fn integration_task_covers_merge_failures_and_missing_children() {
    // Arrange
    let backend = TestSessionBackend::default();
    let (coordinator, updates) = coordinator_with_status_recorder(&backend);
    let mut merged = task(
        1,
        "merged",
        OrchestrationTaskStatus::AwaitingIntegration,
        Some("child-merge"),
    );
    let mut merge_failed = task(
        2,
        "merge-failed",
        OrchestrationTaskStatus::AwaitingIntegration,
        Some("child-merge-failed"),
    );
    let mut missing = task(
        3,
        "missing",
        OrchestrationTaskStatus::AwaitingIntegration,
        None,
    );
    let mut review_requested = task(
        4,
        "review-requested",
        OrchestrationTaskStatus::AwaitingIntegration,
        Some("child-review"),
    );
    let mut review_failed = task(
        5,
        "review-failed",
        OrchestrationTaskStatus::AwaitingIntegration,
        Some("child-review-failed"),
    );

    // Act
    coordinator
        .integrate_task(&mut merged, IntegrationApproach::LocalMerge)
        .await
        .expect("merge should start");
    backend.push_merge_error(SessionError::Operation("merge failed".to_string()));
    coordinator
        .integrate_task(&mut merge_failed, IntegrationApproach::LocalMerge)
        .await
        .expect("merge failure should settle");
    coordinator
        .integrate_task(&mut missing, IntegrationApproach::LocalMerge)
        .await
        .expect("missing child should settle");
    coordinator
        .integrate_task(&mut review_requested, IntegrationApproach::ReviewRequest)
        .await
        .expect("review request should publish");
    backend.push_review_error(SessionError::Operation("review publish failed".to_string()));
    coordinator
        .integrate_task(&mut review_failed, IntegrationApproach::ReviewRequest)
        .await
        .expect("review request failure should settle");

    // Assert
    let updates = updates
        .lock()
        .expect("status updates should remain available");
    assert!(updates.iter().any(|(_, status, _)| status == "Merging"));
    assert!(updates.iter().any(|(_, status, error)| {
        status == "IntegrationFailed" && error.as_deref() == Some("merge failed")
    }));
    assert!(updates.iter().any(|(_, status, error)| {
        status == "IntegrationFailed"
            && error.as_deref() == Some("Verified task has no child session")
    }));
    assert!(
        updates
            .iter()
            .any(|(_, status, _)| status == "ReviewRequested")
    );
    assert!(backend.calls().contains(&"review:child-review".to_string()));
    assert!(updates.iter().any(|(_, status, error)| {
        status == "IntegrationFailed" && error.as_deref() == Some("review publish failed")
    }));
}

#[tokio::test]
async fn integrating_campaign_recovers_children_and_completes_settled_work() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let done = with_child_observation(
        task(
            1,
            "done",
            OrchestrationTaskStatus::Merging,
            Some("child-done"),
        ),
        SessionStatus::Done,
        None,
    );
    let canceled = with_child_observation(
        task(
            2,
            "canceled",
            OrchestrationTaskStatus::Merging,
            Some("child-canceled"),
        ),
        SessionStatus::Canceled,
        None,
    );
    let pending = task(
        3,
        "pending",
        OrchestrationTaskStatus::Merging,
        Some("child-pending"),
    );
    let awaiting = task(
        4,
        "awaiting",
        OrchestrationTaskStatus::AwaitingIntegration,
        Some("child-awaiting"),
    );
    let failed = task(5, "failed", OrchestrationTaskStatus::Failed, None);
    mock_task_snapshots(
        &mut repository,
        vec![
            vec![done],
            vec![canceled],
            vec![pending],
            vec![awaiting],
            vec![failed],
        ],
    );
    repository
        .expect_update_orchestration_task_status()
        .times(3)
        .returning(|_, _, _| Ok(()));
    repository
        .expect_load_orchestration_integration_approach()
        .times(5)
        .returning(|_| Ok(IntegrationApproach::LocalMerge.to_string()));
    repository
        .expect_complete_orchestration_campaign()
        .times(2)
        .returning(|_| Ok(true));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut campaign = orchestration(2);
    campaign.status = OrchestrationStatus::Integrating.to_string();

    // Act
    for _ in 0..5 {
        coordinator
            .reconcile_integration(&campaign)
            .await
            .expect("integration snapshot should reconcile");
    }

    // Assert
    let calls = backend.calls();
    assert!(calls.iter().any(|call| call == "merge:child-awaiting"));
    assert!(calls.iter().all(|call| !call.starts_with("rollup-attempt")));
}

#[tokio::test]
async fn review_request_integration_retries_interrupted_tasks() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let interrupted = task(
        1,
        "interrupted",
        OrchestrationTaskStatus::Merging,
        Some("child-interrupted"),
    );
    let missing = task(2, "missing", OrchestrationTaskStatus::Merging, None);
    let settled = task(
        3,
        "settled",
        OrchestrationTaskStatus::ReviewRequested,
        Some("child-settled"),
    );
    mock_task_snapshots(
        &mut repository,
        vec![vec![interrupted], vec![missing], vec![settled]],
    );
    repository
        .expect_update_orchestration_task_status()
        .times(2)
        .returning(|_, _, _| Ok(()));
    repository
        .expect_load_orchestration_integration_approach()
        .times(3)
        .returning(|_| Ok(IntegrationApproach::ReviewRequest.to_string()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut campaign = orchestration(2);
    campaign.status = OrchestrationStatus::Integrating.to_string();

    // Act
    for _ in 0..3 {
        coordinator
            .reconcile_integration(&campaign)
            .await
            .expect("review-request integration should reconcile");
    }

    // Assert
    assert!(
        backend
            .calls()
            .contains(&"review:child-interrupted".to_string())
    );
}

#[tokio::test]
async fn review_request_campaign_waits_for_open_children_and_completes_after_merge() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let open = task(
        1,
        "open-review",
        OrchestrationTaskStatus::ReviewRequested,
        Some("child-review"),
    );
    let merged = with_child_observation(
        task(
            1,
            "merged-review",
            OrchestrationTaskStatus::ReviewRequested,
            Some("child-review"),
        ),
        SessionStatus::Merged,
        None,
    );
    mock_task_snapshots(&mut repository, vec![vec![open], vec![merged]]);
    repository
        .expect_update_orchestration_task_status()
        .once()
        .withf(|_, status, error| status == "Integrated" && error.as_ref().is_none())
        .returning(|_, _, _| Ok(()));
    repository
        .expect_complete_orchestration_campaign()
        .once()
        .returning(|_| Ok(true));
    repository
        .expect_load_orchestration_integration_approach()
        .times(2)
        .returning(|_| Ok(IntegrationApproach::ReviewRequest.to_string()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut campaign = orchestration(2);
    campaign.status = OrchestrationStatus::Integrating.to_string();

    // Act
    coordinator
        .reconcile_integration(&campaign)
        .await
        .expect("open review request should remain active");
    coordinator
        .reconcile_integration(&campaign)
        .await
        .expect("merged review request should complete");

    // Assert
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}

#[tokio::test]
async fn closed_review_request_records_integration_failure() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let canceled = with_child_observation(
        task(
            1,
            "closed-review",
            OrchestrationTaskStatus::ReviewRequested,
            Some("child-review"),
        ),
        SessionStatus::Canceled,
        None,
    );
    mock_task_snapshots(&mut repository, vec![vec![canceled]]);
    repository
        .expect_update_orchestration_task_status()
        .once()
        .withf(|_, status, error| {
            status == "IntegrationFailed"
                && error.as_deref() == Some("Review request closed without merge")
        })
        .returning(|_, _, _| Ok(()));
    repository
        .expect_load_orchestration_integration_approach()
        .once()
        .returning(|_| Ok(IntegrationApproach::ReviewRequest.to_string()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut campaign = orchestration(2);
    campaign.status = OrchestrationStatus::Integrating.to_string();

    // Act
    coordinator
        .reconcile_integration(&campaign)
        .await
        .expect("closed review request should record a failure");

    // Assert
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}

#[tokio::test]
async fn awaiting_integration_campaign_completes_only_when_every_task_is_settled() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let mut reported_research = task(
        2,
        "research",
        OrchestrationTaskStatus::Reported,
        Some("research-child"),
    );
    reported_research.kind = OrchestrationTaskKind::Research.to_string();
    let unverified_research = reported_research.clone();
    reported_research.verification_verdict = Some("Pass".to_string());
    mock_task_snapshots(
        &mut repository,
        vec![vec![unverified_research], vec![reported_research]],
    );
    repository
        .expect_complete_orchestration_campaign()
        .once()
        .returning(|_| Ok(true));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut campaign = orchestration(2);
    campaign.status = OrchestrationStatus::AwaitingIntegration.to_string();

    // Act
    coordinator
        .reconcile_awaiting_integration(&campaign)
        .await
        .expect("unsettled integration should remain parked");
    coordinator
        .reconcile_awaiting_integration(&campaign)
        .await
        .expect("settled integration should complete");

    // Assert
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}

#[tokio::test]
async fn reconciliation_dispatches_every_parked_campaign_phase() {
    // Arrange
    let mut repository = MockOrchestrationRepository::new();
    let phases = [
        OrchestrationStatus::AwaitingApproval,
        OrchestrationStatus::AwaitingIntegration,
        OrchestrationStatus::Integrating,
    ]
    .into_iter()
    .map(|status| {
        let mut campaign = orchestration(2);
        campaign.status = status.to_string();

        campaign
    })
    .collect::<Vec<_>>();
    repository
        .expect_load_active_orchestrations()
        .once()
        .return_once(move || Ok(phases));
    let parked = with_child_observation(
        task(1, "parked", OrchestrationTaskStatus::Running, Some("child")),
        SessionStatus::Review,
        None,
    );
    mock_task_snapshots(
        &mut repository,
        vec![
            vec![parked],
            vec![task(
                2,
                "approval",
                OrchestrationTaskStatus::AwaitingIntegration,
                Some("child"),
            )],
            vec![task(
                3,
                "merging",
                OrchestrationTaskStatus::Merging,
                Some("child"),
            )],
        ],
    );
    repository
        .expect_update_orchestration_task_status()
        .once()
        .returning(|_, _, _| Ok(()));
    repository
        .expect_load_orchestration_integration_approach()
        .once()
        .returning(|_| Ok(IntegrationApproach::LocalMerge.to_string()));
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    let result = coordinator.reconcile_once().await;

    // Assert
    assert_eq!(result, Ok(()));
}

#[tokio::test]
async fn child_questions_surface_once_without_controller_chat_turns() {
    // Arrange
    let mut repository = MockOrchestrationRepository::new();
    let surfaced = Arc::new(Mutex::new(VecDeque::from([true, false])));
    repository
        .expect_surface_orchestration_questions()
        .times(2)
        .returning({
            let surfaced = Arc::clone(&surfaced);

            move |_, _, _| {
                Ok(surfaced
                    .lock()
                    .expect("question results should remain available")
                    .pop_front()
                    .expect("question result should exist"))
            }
        });
    let backend = TestSessionBackend::default();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut waiting = task(
        1,
        "waiting",
        OrchestrationTaskStatus::WaitingForInput,
        Some("child"),
    );
    waiting.child_questions = Some(r#"[{"text":"Choose one"}]"#.to_string());
    let campaign = orchestration(2);

    // Act
    coordinator
        .surface_child_questions(&campaign, std::slice::from_ref(&waiting))
        .await
        .expect("first question should surface");
    coordinator
        .surface_child_questions(&campaign, std::slice::from_ref(&waiting))
        .await
        .expect("duplicate question should be ignored");
    coordinator
        .surface_child_questions(&campaign, &[])
        .await
        .expect("empty questions should be ignored");

    // Assert
    assert!(matches!(
        event_rx.try_recv(),
        Ok(OrchestrationEvent::RefreshSessions)
    ));
    assert!(event_rx.try_recv().is_err());
}
