use super::*;

#[tokio::test]
async fn failed_child_prompt_cancels_the_child_and_queues_a_retry() {
    // Arrange
    let backend = TestSessionBackend::default();
    backend.push_create_result("child-1");
    backend.push_send_error(SessionError::Operation("send failed".to_string()));
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_claim_orchestration_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(true));
    repository
        .expect_record_orchestration_spawn_failure()
        .withf(|id, error, retry_limit| *id == 1 && error == "send failed" && *retry_limit == 2)
        .once()
        .returning(|_, _, _| Ok(OrchestrationTaskStatus::Planned.to_string()));
    repository
        .expect_link_orchestration_task_child()
        .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
        .once()
        .returning(|_, _| Ok(true));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

    // Act
    coordinator
        .spawn_task(&orchestration(2), &mut planned_task)
        .await
        .expect("failed prompt delivery should settle the task");

    // Assert
    assert_eq!(
        planned_task.status,
        OrchestrationTaskStatus::Planned.to_string()
    );
    assert_eq!(planned_task.infrastructure_retry_count, 1);
    assert_eq!(planned_task.last_error.as_deref(), Some("send failed"));
    assert!(backend.calls().iter().any(|call| call == "cancel:child-1"));
}

#[tokio::test]
async fn cancellation_barrier_prevents_a_stale_planned_task_from_spawning() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_claim_orchestration_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(false));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

    // Act
    coordinator
        .spawn_task(&orchestration(2), &mut planned_task)
        .await
        .expect("a lost fan-out claim should be harmless");

    // Assert
    assert_eq!(
        planned_task.status,
        OrchestrationTaskStatus::Planned.to_string()
    );
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}

#[tokio::test]
async fn cancellation_after_child_creation_stops_the_unclaimed_child() {
    // Arrange
    let backend = TestSessionBackend::default();
    backend.push_create_result("child-1");
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_claim_orchestration_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(true));
    repository
        .expect_link_orchestration_task_child()
        .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
        .once()
        .returning(|_, _| Ok(false));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

    // Act
    coordinator
        .spawn_task(&orchestration(2), &mut planned_task)
        .await
        .expect("the unclaimed child should be canceled");

    // Assert
    assert_eq!(
        backend.calls(),
        vec![
            "create:OrchestrationChild { task_id: 1 }".to_string(),
            "cancel:child-1".to_string(),
        ]
    );
}

#[tokio::test]
async fn interrupted_creation_without_child_is_retried() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_child_session_id_for_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(None));
    repository
        .expect_record_orchestration_spawn_failure()
        .withf(|id, error, retry_limit| {
            *id == 1 && error == "Child creation did not complete" && *retry_limit == 2
        })
        .once()
        .returning(|_, _, _| Ok(OrchestrationTaskStatus::Planned.to_string()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut creating_task = task(1, "protocol", OrchestrationTaskStatus::Creating, None);

    // Act
    coordinator
        .reconcile_task(&mut creating_task)
        .await
        .expect("interrupted creation should settle the task");

    // Assert
    assert_eq!(
        creating_task.status,
        OrchestrationTaskStatus::Planned.to_string()
    );
    assert_eq!(creating_task.infrastructure_retry_count, 1);
    assert_eq!(
        creating_task.last_error.as_deref(),
        Some("Child creation did not complete")
    );
}

#[tokio::test]
async fn continuation_reuses_the_existing_child_with_a_stable_operation() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_rollup_operation_status()
        .withf(|operation_id| operation_id == "orchestration-continuation-1-1")
        .once()
        .returning(|_| Ok(None));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut continued = task(
        1,
        "protocol",
        OrchestrationTaskStatus::ContinuationPending,
        Some("child-protocol"),
    );
    continued.continuation_generation = 1;
    continued.continuation_prompt = Some("Add the missing validation".to_string());

    // Act
    coordinator
        .reconcile_task(&mut continued)
        .await
        .expect("continuation should be delivered");

    // Assert
    let calls = backend.calls();
    assert!(
        calls
            .iter()
            .any(|call| { call == "rollup-attempt:child-protocol:orchestration-continuation-1-1" })
    );
    assert!(calls.iter().any(|call| {
        call.starts_with("rollup:child-protocol:")
            && call.contains("Continue task `protocol` on the same branch")
            && call.contains("Add the missing validation")
            && call.contains("Expected touched areas (planning references): [\"protocol/\"]")
    }));
}

#[tokio::test]
async fn restart_relink_cancels_a_child_after_losing_the_link_claim() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_child_session_id_for_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(Some("child-1".to_string())));
    repository
        .expect_link_orchestration_task_child()
        .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
        .once()
        .returning(|_, _| Ok(false));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut creating_task = task(1, "protocol", OrchestrationTaskStatus::Creating, None);

    // Act
    coordinator
        .reconcile_task(&mut creating_task)
        .await
        .expect("a child that lost its link claim should be canceled");

    // Assert
    assert_eq!(creating_task.child_session_id.as_deref(), Some("child-1"));
    assert_eq!(backend.calls(), vec!["cancel:child-1".to_string()]);
}

#[tokio::test]
async fn waiting_children_hold_parallelism_slots() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_active_orchestrations()
        .once()
        .returning(|| Ok(vec![orchestration(1)]));
    mock_task_snapshots(
        &mut repository,
        vec![
            vec![
                with_child_observation(
                    task(
                        1,
                        "protocol",
                        OrchestrationTaskStatus::Running,
                        Some("child-1"),
                    ),
                    SessionStatus::Question,
                    None,
                ),
                task(2, "ui", OrchestrationTaskStatus::Planned, None),
            ],
            vec![
                task(
                    1,
                    "protocol",
                    OrchestrationTaskStatus::WaitingForInput,
                    Some("child-1"),
                ),
                task(2, "ui", OrchestrationTaskStatus::Planned, None),
            ],
        ],
    );
    repository
        .expect_update_orchestration_task_status()
        .withf(|id, status, error| {
            *id == 1
                && status == OrchestrationTaskStatus::WaitingForInput.to_string()
                && error.is_none()
        })
        .once()
        .returning(|_, _, _| Ok(()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    coordinator
        .reconcile_once()
        .await
        .expect("reconciliation should succeed");

    // Assert
    assert!(
        !backend
            .calls()
            .iter()
            .any(|call| call.starts_with("create:"))
    );
}
