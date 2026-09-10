use super::*;

#[tokio::test]
async fn research_task_spawns_with_read_only_mode_and_prompt() {
    // Arrange
    let backend = TestSessionBackend::default();
    backend.push_create_result("research-child");
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_claim_orchestration_task()
        .withf(|id| *id == 1)
        .once()
        .returning(|_| Ok(true));
    repository
        .expect_link_orchestration_task_child()
        .withf(|id, child_session_id| *id == 1 && child_session_id == "research-child")
        .once()
        .returning(|_, _| Ok(true));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut planned_task = task(1, "architecture", OrchestrationTaskStatus::Planned, None);
    planned_task.kind = OrchestrationTaskKind::Research.to_string();
    planned_task.prompt = "Map the runtime boundaries".to_string();

    // Act
    coordinator
        .spawn_task(&orchestration(2), &mut planned_task)
        .await
        .expect("research child should spawn");

    // Assert
    assert_eq!(
        planned_task.status,
        OrchestrationTaskStatus::Running.to_string()
    );
    assert_eq!(
        backend.calls()[0],
        "create:OrchestrationResearch { task_id: 1 }"
    );
    assert!(backend.calls()[1].contains("send:research-child:"));
    assert!(backend.calls()[1].contains("Treat the repository as read-only"));
    assert!(backend.calls()[1].contains("do not run mutating Git commands"));
    assert!(backend.calls()[1].contains("Map the runtime boundaries"));
}

#[tokio::test]
async fn completed_research_captures_full_answer_cancels_child_and_discards_edits() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_update_orchestration_task_research_report()
        .withf(|id, report| *id == 1 && report == "Deep architecture report")
        .once()
        .returning(|_, _| Ok(()));
    repository
        .expect_update_orchestration_task_status()
        .withf(|id, status, error| {
            *id == 1 && status == "Reported" && error.as_deref() == Some(RESEARCH_EDIT_WARNING)
        })
        .once()
        .returning(|_, _, _| Ok(()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut research = task(
        1,
        "architecture",
        OrchestrationTaskStatus::Running,
        Some("research-child"),
    );
    research.kind = OrchestrationTaskKind::Research.to_string();
    research.child_status = Some(SessionStatus::Review.to_string());
    research.child_answer = Some("Deep architecture report".to_string());
    research.child_has_diff = Some(true);

    // Act
    coordinator
        .reconcile_task(&mut research)
        .await
        .expect("research report should settle");

    // Assert
    assert_eq!(
        research.status,
        OrchestrationTaskStatus::Reported.to_string()
    );
    assert_eq!(
        research.research_report.as_deref(),
        Some("Deep architecture report")
    );
    assert_eq!(research.last_error.as_deref(), Some(RESEARCH_EDIT_WARNING));
    assert_eq!(backend.calls(), ["cancel:research-child"]);
}

#[tokio::test]
async fn completed_research_without_edits_reuses_captured_report() {
    // Arrange
    let backend = TestSessionBackend::default();
    let mut repository = MockOrchestrationRepository::new();
    repository
        .expect_update_orchestration_task_status()
        .withf(|id, status, error| *id == 1 && status == "Reported" && error.is_none())
        .once()
        .returning(|_, _, _| Ok(()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let coordinator =
        OrchestrationCoordinator::new(Arc::new(event_tx), Arc::new(repository), backend.service());
    let mut research = task(
        1,
        "architecture",
        OrchestrationTaskStatus::Running,
        Some("research-child"),
    );
    research.kind = OrchestrationTaskKind::Research.to_string();
    research.child_status = Some(SessionStatus::Done.to_string());
    research.child_answer = Some("Captured report".to_string());
    research.research_report = Some("Captured report".to_string());
    research.child_has_diff = Some(false);

    // Act
    coordinator
        .reconcile_task(&mut research)
        .await
        .expect("already captured report should settle");

    // Assert
    assert_eq!(
        research.status,
        OrchestrationTaskStatus::Reported.to_string()
    );
    assert!(research.last_error.is_none());
    assert_eq!(backend.calls(), [] as [String; 0]);
}

#[tokio::test]
async fn research_reconciliation_maps_questions_activity_cancellation_and_restart_state() {
    // Arrange
    let backend = TestSessionBackend::default();
    let (coordinator, updates) = coordinator_with_status_recorder(&backend);
    let mut question = task(
        1,
        "question",
        OrchestrationTaskStatus::Running,
        Some("question-child"),
    );
    question.kind = OrchestrationTaskKind::Research.to_string();
    question.child_status = Some(SessionStatus::Question.to_string());
    let mut active = task(
        2,
        "active",
        OrchestrationTaskStatus::Planned,
        Some("active-child"),
    );
    active.kind = OrchestrationTaskKind::Research.to_string();
    active.child_status = Some(SessionStatus::Queued.to_string());
    let mut canceled = task(
        3,
        "canceled",
        OrchestrationTaskStatus::Running,
        Some("canceled-child"),
    );
    canceled.kind = OrchestrationTaskKind::Research.to_string();
    canceled.child_status = Some(SessionStatus::Canceled.to_string());
    let mut captured = canceled.clone();
    captured.id = 4;
    captured.task_key = "captured".to_string();
    captured.research_report = Some("Durable report".to_string());
    captured.child_has_diff = Some(false);
    let mut reported = captured.clone();
    reported.id = 5;
    reported.status = OrchestrationTaskStatus::Reported.to_string();

    // Act
    coordinator
        .reconcile_research_task(&mut question)
        .await
        .expect("question should reconcile");
    coordinator
        .reconcile_research_task(&mut active)
        .await
        .expect("activity should reconcile");
    coordinator
        .reconcile_research_task(&mut canceled)
        .await
        .expect("missing report cancellation should reconcile");
    coordinator
        .reconcile_research_task(&mut captured)
        .await
        .expect("captured report cancellation should reconcile");
    coordinator
        .reconcile_research_task(&mut reported)
        .await
        .expect("reported restart snapshot should be stable");

    // Assert
    assert_eq!(
        question.status,
        OrchestrationTaskStatus::WaitingForInput.to_string()
    );
    assert_eq!(active.status, OrchestrationTaskStatus::Running.to_string());
    assert_eq!(canceled.status, OrchestrationTaskStatus::Failed.to_string());
    assert_eq!(
        captured.status,
        OrchestrationTaskStatus::Reported.to_string()
    );
    assert_eq!(
        reported.status,
        OrchestrationTaskStatus::Reported.to_string()
    );
    assert_eq!(
        updates
            .lock()
            .expect("updates should remain available")
            .len(),
        4
    );
}
