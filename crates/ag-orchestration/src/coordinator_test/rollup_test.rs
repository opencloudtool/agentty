use super::*;

#[test]
fn live_status_loader_is_deduplicated_and_clearable() {
    // Arrange
    let repository = MockOrchestrationRepository::new();
    let backend = TestSessionBackend::default();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let orchestration = orchestration(2);
    let tasks = vec![
        task(
            1,
            "protocol",
            OrchestrationTaskStatus::Running,
            Some("child-1"),
        ),
        task(2, "ui", OrchestrationTaskStatus::Planned, None),
    ];

    // Act
    coordinator.emit_live_status(&orchestration, &tasks);
    coordinator.emit_live_status(&orchestration, &tasks);

    // Assert
    assert_eq!(
        event_rx.try_recv(),
        Ok(OrchestrationEvent::ProgressUpdated {
            progress: Some(
                "Phase: Running\nParallel workers: 2 (global setting)\n- protocol [protocol]: \
                 running\n- ui [ui]: waiting"
                    .to_string()
            ),
            session_id: SessionId::from("controller"),
        })
    );
    assert!(event_rx.try_recv().is_err());

    // Act
    coordinator.clear_live_status(&orchestration);

    // Assert
    assert_eq!(
        event_rx.try_recv(),
        Ok(OrchestrationEvent::ProgressUpdated {
            progress: None,
            session_id: SessionId::from("controller"),
        })
    );
}

#[test]
fn live_status_loader_formats_every_task_state() {
    // Arrange
    let states = [
        (OrchestrationTaskStatus::Proposed, "awaiting approval"),
        (OrchestrationTaskStatus::Planned, "waiting"),
        (OrchestrationTaskStatus::Creating, "starting"),
        (OrchestrationTaskStatus::Running, "running"),
        (OrchestrationTaskStatus::WaitingForInput, "waiting on you"),
        (OrchestrationTaskStatus::Ready, "ready"),
        (OrchestrationTaskStatus::ContinuationPending, "continuing"),
        (
            OrchestrationTaskStatus::AwaitingIntegration,
            "awaiting integration",
        ),
        (OrchestrationTaskStatus::Merging, "integrating"),
        (OrchestrationTaskStatus::Integrated, "integrated"),
        (OrchestrationTaskStatus::ReviewRequested, "review requested"),
        (
            OrchestrationTaskStatus::IntegrationFailed,
            "integration failed",
        ),
        (OrchestrationTaskStatus::Detached, "detached"),
        (OrchestrationTaskStatus::Failed, "failed"),
        (OrchestrationTaskStatus::Canceled, "canceled"),
    ];
    let mut tasks = (0_i64..)
        .zip(states)
        .map(|(index, (status, _))| task(index, &status.to_string(), status, None))
        .collect::<Vec<_>>();
    let mut invalid_task = task(8, "invalid", OrchestrationTaskStatus::Running, None);
    invalid_task.status = "invalid".to_string();
    tasks.push(invalid_task);
    tasks[0].areas_compliant = Some(true);
    tasks[0].verification_verdict = Some("Pass".to_string());
    tasks[1].areas_compliant = Some(false);
    tasks[1].area_violations = r#"["README.md"]"#.to_string();
    tasks[1].verification_verdict = Some("Flag".to_string());
    tasks[1].verification_reason = Some("Wrong file".to_string());
    tasks[2].verification_verdict = Some("Flag".to_string());

    // Act
    let message = campaign_status_message(&orchestration(2), &tasks);

    // Assert
    assert!(message.starts_with("Phase: Running\nParallel workers: 2 (global setting)\n"));
    for (status, label) in states {
        assert!(message.contains(&format!("- {status} [{status}]: {label}")));
    }
    assert!(message.contains("- invalid [invalid]: unknown"));
    assert!(message.contains("within expected areas; verified"));
    assert!(message.contains(r#"additional paths: ["README.md"]; flagged: Wrong file"#));
    assert!(message.contains("Creating [Creating]: starting; flagged"));
}

#[tokio::test]
async fn restart_relink_and_out_of_band_settlement_submit_rollup() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_active_orchestrations()
        .times(2)
        .returning(|| Ok(vec![orchestration(2)]));
    let observed_merged = with_child_observation(
        task(
            1,
            "protocol",
            OrchestrationTaskStatus::Running,
            Some("child-merged"),
        ),
        SessionStatus::Merged,
        Some("Merged result"),
    );
    let mut refreshed_merged = observed_merged.clone();
    refreshed_merged.status = OrchestrationTaskStatus::Ready.to_string();
    refreshed_merged.result_summary = Some("Merged result".to_string());
    let observed_canceled = with_child_observation(
        task(
            2,
            "ui",
            OrchestrationTaskStatus::Running,
            Some("child-canceled"),
        ),
        SessionStatus::Canceled,
        None,
    );
    let settled_canceled = task(
        2,
        "ui",
        OrchestrationTaskStatus::Failed,
        Some("child-canceled"),
    );
    mock_task_snapshots(
        &mut repository,
        vec![
            vec![
                task(1, "protocol", OrchestrationTaskStatus::Creating, None),
                observed_canceled,
            ],
            vec![observed_merged.clone(), settled_canceled.clone()],
            vec![observed_merged, settled_canceled.clone()],
            vec![refreshed_merged, settled_canceled],
        ],
    );
    repository
        .expect_load_child_session_id_for_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(Some("child-merged".to_string())));
    repository
        .expect_link_orchestration_task_child()
        .withf(|id, child_session_id| *id == 1 && child_session_id == "child-merged")
        .once()
        .returning(|_, _| Ok(true));
    let status_updates = Arc::new(Mutex::new(Vec::new()));
    repository
        .expect_update_orchestration_task_status()
        .times(2)
        .returning({
            let status_updates = Arc::clone(&status_updates);

            move |id, status, _| {
                status_updates
                    .lock()
                    .expect("status updates should remain available")
                    .push((id, status.to_string()));

                Ok(())
            }
        });
    repository
        .expect_update_orchestration_task_result_summary()
        .withf(|id, summary| *id == 1 && summary == "Merged result")
        .once()
        .returning(|_, _| Ok(()));
    repository
        .expect_claim_orchestration_rollup()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(true));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    coordinator
        .reconcile_once()
        .await
        .expect("restart re-link should succeed");
    coordinator
        .reconcile_once()
        .await
        .expect("settlement should succeed on the next snapshot");

    // Assert
    assert_reconciled_rollup(&backend, &status_updates);
}

#[tokio::test]
async fn settled_rollup_claimed_elsewhere_is_not_submitted_twice() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_load_active_orchestrations()
        .once()
        .returning(|| Ok(vec![orchestration(2)]));
    let settled_tasks = vec![task(1, "protocol", OrchestrationTaskStatus::Failed, None)];
    mock_task_snapshots(&mut repository, vec![settled_tasks.clone(), settled_tasks]);
    repository
        .expect_claim_orchestration_rollup()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(false));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    coordinator
        .reconcile_once()
        .await
        .expect("an existing roll-up claim should be accepted");

    // Assert
    assert!(
        backend
            .calls()
            .iter()
            .all(|call| !call.starts_with("rollup"))
    );
}

#[tokio::test]
async fn completed_rollup_retries_status_persistence_without_resubmitting() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let mut submitting = orchestration(2);
    submitting.status = OrchestrationStatus::Verifying.to_string();
    submitting.verification_generation = 1;
    let active_snapshots = Arc::new(Mutex::new(VecDeque::from([
        vec![orchestration(2)],
        vec![submitting.clone()],
        vec![submitting],
    ])));
    repository
        .expect_load_active_orchestrations()
        .times(3)
        .returning({
            let active_snapshots = Arc::clone(&active_snapshots);

            move || {
                Ok(active_snapshots
                    .lock()
                    .expect("active snapshots should remain available")
                    .pop_front()
                    .expect("expected another active snapshot"))
            }
        });
    let mut ready_task = task(
        1,
        "protocol",
        OrchestrationTaskStatus::Ready,
        Some("child-ready"),
    );
    ready_task.child_answer = Some("Completed".to_string());
    ready_task.result_summary = Some("Completed".to_string());
    let task_snapshots = Arc::new(Mutex::new(VecDeque::from([
        vec![ready_task.clone()],
        vec![ready_task.clone()],
        vec![ready_task.clone()],
        vec![ready_task],
    ])));
    repository
        .expect_load_orchestration_tasks()
        .times(4)
        .returning({
            let task_snapshots = Arc::clone(&task_snapshots);

            move |_| {
                Ok(task_snapshots
                    .lock()
                    .expect("task snapshots should remain available")
                    .pop_front()
                    .expect("expected another task snapshot"))
            }
        });
    repository
        .expect_claim_orchestration_rollup()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(true));
    repository
        .expect_load_rollup_operation_status()
        .withf(|operation_id| operation_id == "orchestration-rollup-1-1")
        .times(2)
        .returning(|_| Ok(Some("done".to_string())));
    expect_rollup_completion_failure_then_success(&mut repository);
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    let first_result = coordinator.reconcile_once().await;
    let second_result = coordinator.reconcile_once().await;
    let third_result = coordinator.reconcile_once().await;

    // Assert
    assert_eq!(first_result, Ok(()));
    assert_eq!(
        second_result,
        Err("injected post-submit failure".to_string())
    );
    assert_eq!(third_result, Ok(()));
    let calls = backend.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| {
                call.as_str() == "rollup-attempt:controller:orchestration-rollup-1-1"
            })
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("rollup:controller:"))
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_rollup_operation_is_retried_with_the_same_identifier() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let mut submitting = orchestration(2);
    submitting.status = OrchestrationStatus::Verifying.to_string();
    submitting.verification_generation = 1;
    repository
        .expect_load_active_orchestrations()
        .once()
        .return_once(move || Ok(vec![submitting]));
    let ready_task = task(
        1,
        "protocol",
        OrchestrationTaskStatus::Ready,
        Some("child-ready"),
    );
    mock_task_snapshots(&mut repository, vec![vec![ready_task]]);
    repository
        .expect_load_rollup_operation_status()
        .withf(|operation_id| operation_id == "orchestration-rollup-1-1")
        .once()
        .returning(|_| Ok(Some("failed".to_string())));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    coordinator
        .reconcile_once()
        .await
        .expect("failed roll-up delivery should be retried");

    // Assert
    assert!(
        backend
            .calls()
            .iter()
            .any(|call| { call == "rollup-attempt:controller:orchestration-rollup-1-1" })
    );
}

#[tokio::test]
async fn unfinished_rollups_wait_and_unknown_operation_states_fail() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    let mut submitting = orchestration(2);
    submitting.status = OrchestrationStatus::Verifying.to_string();
    repository
        .expect_load_active_orchestrations()
        .times(3)
        .returning(move || Ok(vec![submitting.clone()]));
    let ready_task = task(
        1,
        "protocol",
        OrchestrationTaskStatus::Ready,
        Some("child-ready"),
    );
    mock_task_snapshots(
        &mut repository,
        vec![
            vec![ready_task.clone()],
            vec![ready_task.clone()],
            vec![ready_task],
        ],
    );
    let statuses = Arc::new(Mutex::new(VecDeque::from([
        "queued".to_string(),
        "running".to_string(),
        "unexpected".to_string(),
    ])));
    repository
        .expect_load_rollup_operation_status()
        .times(3)
        .returning(move |_| {
            Ok(statuses
                .lock()
                .expect("operation statuses should remain available")
                .pop_front())
        });
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());

    // Act
    let queued_result = coordinator.reconcile_once().await;
    let running_result = coordinator.reconcile_once().await;
    let unknown_result = coordinator.reconcile_once().await;

    // Assert
    assert_eq!(queued_result, Ok(()));
    assert_eq!(running_result, Ok(()));
    assert_eq!(
        unknown_result,
        Err("Unknown roll-up operation status `unexpected` for orchestration 1".to_string())
    );
    assert_eq!(backend.calls(), [] as [std::string::String; 0]);
}
