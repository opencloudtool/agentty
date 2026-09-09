use super::*;

#[tokio::test]
async fn focused_review_reconciliation_waits_and_settles_terminal_results() {
    // Arrange
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_update_orchestration_task_status()
        .times(0..)
        .returning(|_, _, _| Ok(()));
    repository
        .expect_update_orchestration_task_result_summary()
        .times(3)
        .returning(|_, _| Ok(()));
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut pending = focused_review_task(
        1,
        "pending",
        "child-pending",
        FocusedReviewStatus::Pending,
        None,
    );
    let mut failed = focused_review_task(
        2,
        "failed",
        "child-failed",
        FocusedReviewStatus::Failed,
        None,
    );
    failed.child_answer = Some("Review failed, task complete".to_string());
    let mut empty = focused_review_task(
        3,
        "empty",
        "child-empty",
        FocusedReviewStatus::Ready,
        Some("### Suggestions\n\n- None"),
    );
    let mut no_diff = task(
        4,
        "no-diff",
        OrchestrationTaskStatus::Reviewing,
        Some("child-no-diff"),
    );
    no_diff.child_has_diff = Some(false);
    no_diff.child_answer = Some("No changes needed".to_string());
    let mut invalid = task(
        5,
        "invalid",
        OrchestrationTaskStatus::Reviewing,
        Some("child-invalid"),
    );
    invalid.child_focused_review_status = Some("Unknown".to_string());

    // Act
    coordinator
        .reconcile_focused_review(&mut pending)
        .await
        .expect("pending review should wait");
    coordinator
        .reconcile_focused_review(&mut failed)
        .await
        .expect("failed review should settle for controller verification");
    coordinator
        .reconcile_focused_review(&mut empty)
        .await
        .expect("empty review should settle");
    coordinator
        .reconcile_focused_review(&mut no_diff)
        .await
        .expect("a child without a diff should settle immediately");
    let invalid_result = coordinator.reconcile_focused_review(&mut invalid).await;

    // Assert
    assert_eq!(
        pending.status,
        OrchestrationTaskStatus::Reviewing.to_string()
    );
    assert_eq!(failed.status, OrchestrationTaskStatus::Ready.to_string());
    assert_eq!(empty.status, OrchestrationTaskStatus::Ready.to_string());
    assert_eq!(no_diff.status, OrchestrationTaskStatus::Ready.to_string());
    assert_eq!(
        invalid_result,
        Err("Unknown focused review status: Unknown".to_string())
    );
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}

#[tokio::test]
async fn focused_review_reconciliation_applies_suggestions_and_stops_at_limit() {
    // Arrange
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_update_orchestration_task_status()
        .times(0..)
        .returning(|_, _, _| Ok(()));
    repository
        .expect_update_orchestration_task_result_summary()
        .once()
        .returning(|_, _| Ok(()));
    let claims = Arc::new(Mutex::new(VecDeque::from([false, true])));
    repository
        .expect_claim_orchestration_review_application()
        .times(2)
        .returning({
            let claims = Arc::clone(&claims);

            move |_, prompt, limit| {
                assert!(prompt.starts_with("Verify the focused-review suggestions"));
                assert_eq!(limit, MAX_AUTOMATED_REVIEW_ITERATIONS);

                Ok(claims
                    .lock()
                    .expect("review claims should remain available")
                    .pop_front()
                    .expect("review claim should exist"))
            }
        });
    repository
        .expect_load_rollup_operation_status()
        .once()
        .returning(|operation_id| {
            assert_eq!(operation_id, "orchestration-continuation-5-1");

            Ok(None)
        });
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut unclaimed = focused_review_task(
        4,
        "unclaimed",
        "child-unclaimed",
        FocusedReviewStatus::Ready,
        Some("### Suggestions\n\n- Fix one"),
    );
    let mut actionable = focused_review_task(
        5,
        "actionable",
        "child-actionable",
        FocusedReviewStatus::Ready,
        Some("### Suggestions\n\n- Fix two"),
    );
    let mut capped = focused_review_task(
        6,
        "capped",
        "child-capped",
        FocusedReviewStatus::Ready,
        Some("### Suggestions\n\n- Still present"),
    );
    capped.review_iteration = MAX_AUTOMATED_REVIEW_ITERATIONS;

    // Act
    coordinator
        .reconcile_focused_review(&mut unclaimed)
        .await
        .expect("lost claim should retry from a fresh snapshot");
    coordinator
        .reconcile_focused_review(&mut actionable)
        .await
        .expect("actionable review should start remediation");
    coordinator
        .reconcile_focused_review(&mut capped)
        .await
        .expect("iteration cap should settle for controller verification");

    // Assert
    assert_eq!(
        unclaimed.status,
        OrchestrationTaskStatus::Reviewing.to_string()
    );
    assert_eq!(
        actionable.status,
        OrchestrationTaskStatus::ReviewApplying.to_string()
    );
    assert_eq!(actionable.review_iteration, 1);
    assert_eq!(capped.status, OrchestrationTaskStatus::Ready.to_string());
    assert!(backend.calls().iter().any(|call| {
        call.starts_with("rollup:child-actionable:Verify the focused-review suggestions")
    }));
}

#[tokio::test]
async fn review_application_reconciliation_reports_missing_data_and_recovers_delivery() {
    // Arrange
    let operation_states = Arc::new(Mutex::new(VecDeque::from([
        None,
        None,
        Some("failed".to_string()),
    ])));
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_rollup_operation_status()
        .times(3)
        .returning({
            let operation_states = Arc::clone(&operation_states);

            move |_| {
                Ok(operation_states
                    .lock()
                    .expect("operation states should remain available")
                    .pop_front()
                    .expect("operation state should exist"))
            }
        });
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut base = review_applying_task();
    let mut lost_child = base.clone();
    lost_child.child_session_id = None;
    let mut lost_prompt = base.clone();
    lost_prompt.continuation_prompt = None;

    // Act
    let lost_child_result = coordinator
        .reconcile_review_application(&mut lost_child)
        .await;
    let lost_prompt_result = coordinator
        .reconcile_review_application(&mut lost_prompt)
        .await;
    coordinator
        .reconcile_review_application(&mut base)
        .await
        .expect("failed operation should resubmit");

    // Assert
    assert_eq!(
        lost_child_result,
        Err("Review remediation lost its managed child".to_string())
    );
    assert_eq!(
        lost_prompt_result,
        Err("Review remediation lost its verification prompt".to_string())
    );
    assert!(
        backend
            .calls()
            .contains(&"rollup:child-review:Verify then apply".to_string())
    );
}

#[tokio::test]
async fn review_application_reconciliation_maps_operation_and_child_states() {
    // Arrange
    let operation_states = Arc::new(Mutex::new(VecDeque::from([
        Some("queued".to_string()),
        Some("running".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("unexpected".to_string()),
    ])));
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_rollup_operation_status()
        .times(8)
        .returning({
            let operation_states = Arc::clone(&operation_states);

            move |_| {
                Ok(operation_states
                    .lock()
                    .expect("operation states should remain available")
                    .pop_front()
                    .expect("operation state should exist"))
            }
        });
    repository
        .expect_update_orchestration_task_status()
        .times(4)
        .returning(|_, _, _| Ok(()));
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let base = review_applying_task();
    let observed = [
        SessionStatus::Question,
        SessionStatus::Canceled,
        SessionStatus::Review,
        SessionStatus::Merged,
        SessionStatus::InProgress,
    ];

    // Act
    for _ in 0..2 {
        coordinator
            .reconcile_review_application(&mut base.clone())
            .await
            .expect("pending operation should wait");
    }
    let mut reconciled = Vec::new();
    for status in observed {
        let mut observed_task = with_child_observation(base.clone(), status, None);
        coordinator
            .reconcile_review_application(&mut observed_task)
            .await
            .expect("completed review application should reconcile");
        reconciled.push(observed_task.status);
    }
    let unknown = coordinator
        .reconcile_review_application(&mut base.clone())
        .await;

    // Assert
    assert_eq!(
        reconciled,
        [
            OrchestrationTaskStatus::WaitingForInput.to_string(),
            OrchestrationTaskStatus::Failed.to_string(),
            OrchestrationTaskStatus::Reviewing.to_string(),
            OrchestrationTaskStatus::Ready.to_string(),
            OrchestrationTaskStatus::ReviewApplying.to_string(),
        ]
    );
    assert_eq!(
        unknown,
        Err("Unknown review remediation operation status `unexpected` for task 7".to_string())
    );
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}

#[tokio::test]
async fn review_application_restart_reconciles_through_task_dispatch() {
    // Arrange
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_rollup_operation_status()
        .once()
        .returning(|_| Ok(Some("queued".to_string())));
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut applying = review_applying_task();

    // Act
    coordinator
        .reconcile_task(&mut applying)
        .await
        .expect("restart should dispatch review remediation reconciliation");

    // Assert
    assert_eq!(
        applying.status,
        OrchestrationTaskStatus::ReviewApplying.to_string()
    );
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}
