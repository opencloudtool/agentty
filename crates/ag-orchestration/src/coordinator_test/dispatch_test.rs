use super::*;

#[tokio::test]
async fn continuation_reconciliation_recovers_every_operation_and_child_state() {
    // Arrange
    let mut repository = MockOrchestrationRepository::new();
    let operation_states = Arc::new(Mutex::new(VecDeque::from([
        Some("queued".to_string()),
        Some("running".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("done".to_string()),
        Some("unexpected".to_string()),
        Some("failed".to_string()),
    ])));
    repository
        .expect_load_rollup_operation_status()
        .times(9)
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
        .times(5)
        .returning(|_, _, _| Ok(()));
    repository
        .expect_update_orchestration_task_result_summary()
        .once()
        .returning(|_, _| Ok(()));
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut base = task(
        1,
        "continue",
        OrchestrationTaskStatus::ContinuationPending,
        Some("child"),
    );
    base.continuation_generation = 1;
    base.continuation_prompt = Some("Address feedback".to_string());
    let mut lost = base.clone();
    lost.child_session_id = None;
    let observed = [
        SessionStatus::Question,
        SessionStatus::Canceled,
        SessionStatus::Review,
        SessionStatus::Merged,
        SessionStatus::InProgress,
    ];

    // Act
    coordinator
        .reconcile_continuation(&mut lost)
        .await
        .expect("lost continuation should fail");
    for _ in 0..2 {
        coordinator
            .reconcile_continuation(&mut base.clone())
            .await
            .expect("pending continuation operation should wait");
    }
    let mut reconciled = Vec::new();
    for status in observed {
        let mut task = with_child_observation(base.clone(), status, Some("Follow-up complete"));
        coordinator
            .reconcile_continuation(&mut task)
            .await
            .expect("completed continuation should reconcile");
        reconciled.push(task.status);
    }
    let unknown = coordinator.reconcile_continuation(&mut base.clone()).await;
    coordinator
        .reconcile_continuation(&mut base)
        .await
        .expect("failed operation should resubmit");

    // Assert
    assert_eq!(
        unknown,
        Err("Unknown continuation operation status `unexpected` for task 1".to_string())
    );
    assert_eq!(
        reconciled,
        [
            OrchestrationTaskStatus::WaitingForInput.to_string(),
            OrchestrationTaskStatus::Failed.to_string(),
            OrchestrationTaskStatus::Reviewing.to_string(),
            OrchestrationTaskStatus::Ready.to_string(),
            OrchestrationTaskStatus::ContinuationPending.to_string(),
        ]
    );
    assert!(backend.calls().iter().any(|call| {
        call.starts_with("rollup:child:Continue task `continue` on the same branch")
    }));
}

#[tokio::test]
async fn coordinator_run_survives_one_reconciliation_error() {
    // Arrange
    let reconciliation_attempted = Arc::new(tokio::sync::Notify::new());
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_active_orchestrations()
        .once()
        .returning({
            let reconciliation_attempted = Arc::clone(&reconciliation_attempted);

            move || {
                reconciliation_attempted.notify_one();

                Err(DbError::Io(std::io::Error::other("injected failure")))
            }
        });
    let backend = TestSessionBackend::default();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    let coordinator_task = tokio::spawn(coordinator.run(OneShotSchedule::default()));
    reconciliation_attempted.notified().await;
    tokio::task::yield_now().await;
    coordinator_task.abort();
    let join_result = coordinator_task.await;

    // Assert
    assert!(join_result.is_err());
}

#[tokio::test]
async fn canceling_orchestration_recovers_every_task_shape_and_settles() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let mut canceling = orchestration(1);
    canceling.status = OrchestrationStatus::Canceling.to_string();
    repository
        .expect_load_active_orchestrations()
        .once()
        .return_once(move || Ok(vec![canceling]));
    repository
        .expect_load_orchestration_tasks()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| {
            Ok(vec![
                task(1, "protocol", OrchestrationTaskStatus::Creating, None),
                with_child_observation(
                    task(
                        2,
                        "terminal",
                        OrchestrationTaskStatus::Running,
                        Some("child-2"),
                    ),
                    SessionStatus::Done,
                    None,
                ),
                task(3, "unstarted", OrchestrationTaskStatus::Planned, None),
                task(
                    4,
                    "settled",
                    OrchestrationTaskStatus::Ready,
                    Some("child-4"),
                ),
            ])
        });
    repository
        .expect_load_child_session_id_for_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(Some("child-1".to_string())));
    repository
        .expect_load_child_session_id_for_task()
        .withf(|id| *id == 3)
        .once()
        .returning(|_| Ok(None));
    repository
        .expect_update_orchestration_task_status()
        .withf(|id, status, error| {
            [1, 2, 3].contains(id)
                && status == OrchestrationTaskStatus::Canceled.to_string()
                && error.is_none()
        })
        .times(3)
        .returning(|_, _, _| Ok(()));
    repository
        .expect_update_orchestration_status()
        .withf(|id, status| *id == 1 && status == OrchestrationStatus::Canceled.to_string())
        .once()
        .returning(|_, _| Ok(()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    let result = coordinator.reconcile_once().await;

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(backend.calls(), vec!["cancel:child-1".to_string()]);
}

#[tokio::test]
async fn canceling_orchestration_retries_after_child_cancellation_error() {
    // Arrange
    let backend = TestSessionBackend::default();
    backend.push_cancel_error(SessionError::Operation("cancel failed".to_string()));
    let mut repository = MockOrchestrationRepository::new();
    let mut canceling = orchestration(1);
    canceling.status = OrchestrationStatus::Canceling.to_string();
    repository
        .expect_load_active_orchestrations()
        .times(2)
        .returning(move || Ok(vec![canceling.clone()]));
    repository
        .expect_load_orchestration_tasks()
        .withf(|id| *id == 1)
        .times(2)
        .returning(|_| {
            Ok(vec![task(
                1,
                "protocol",
                OrchestrationTaskStatus::Running,
                Some("child-1"),
            )])
        });
    repository
        .expect_update_orchestration_task_status()
        .withf(|id, status, error| {
            *id == 1 && status == OrchestrationTaskStatus::Canceled.to_string() && error.is_none()
        })
        .once()
        .returning(|_, _, _| Ok(()));
    repository
        .expect_update_orchestration_status()
        .withf(|id, status| *id == 1 && status == OrchestrationStatus::Canceled.to_string())
        .once()
        .returning(|_, _| Ok(()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    let first_result = coordinator.reconcile_once().await;
    let retry_result = coordinator.reconcile_once().await;

    // Assert
    assert_eq!(first_result, Err("cancel failed".to_string()));
    assert_eq!(retry_result, Ok(()));
    assert_eq!(
        backend.calls(),
        vec!["cancel:child-1".to_string(), "cancel:child-1".to_string()]
    );
}

#[tokio::test]
async fn reconciliation_spawns_only_up_to_the_parallelism_cap() {
    // Arrange
    let backend = TestSessionBackend::default();
    backend.push_create_result("child-1");
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_active_orchestrations()
        .once()
        .returning(|| Ok(vec![orchestration(1)]));
    mock_task_snapshots(
        &mut repository,
        vec![
            vec![
                task(1, "protocol", OrchestrationTaskStatus::Planned, None),
                task(2, "ui", OrchestrationTaskStatus::Planned, None),
            ],
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
        ],
    );
    repository
        .expect_claim_orchestration_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(true));
    repository
        .expect_link_orchestration_task_child()
        .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
        .once()
        .returning(|_, _| Ok(true));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    coordinator
        .reconcile_once()
        .await
        .expect("reconciliation should succeed");

    // Assert
    let calls = backend.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("create:"))
            .count(),
        1
    );
    assert!(calls.iter().any(|call| {
        call.starts_with("send:child-1:")
            && call.contains("You are one worker in an orchestration.")
            && call.contains("Task key: protocol")
            && call.contains("keep `answer` concise")
    }));
}

#[tokio::test]
async fn failed_child_creation_queues_an_infrastructure_retry() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_claim_orchestration_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(true));
    repository
        .expect_record_orchestration_spawn_failure()
        .withf(|id, error, retry_limit| {
            *id == 1 && error == "missing create result" && *retry_limit == 2
        })
        .once()
        .returning(|_, _, _| Ok(OrchestrationTaskStatus::Planned.to_string()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

    // Act
    coordinator
        .spawn_task(&orchestration(2), &mut planned_task)
        .await
        .expect("failed creation should settle the task");

    // Assert
    assert_eq!(
        planned_task.status,
        OrchestrationTaskStatus::Planned.to_string()
    );
    assert_eq!(planned_task.infrastructure_retry_count, 1);
    assert_eq!(
        planned_task.last_error.as_deref(),
        Some("missing create result")
    );
}
