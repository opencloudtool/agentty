use super::*;

#[tokio::test]
async fn awaiting_integration_continuation_resets_passed_siblings_for_verification() {
    // Arrange
    let (database, project_id) = controller_database().await;
    let (orchestration, tasks, _, _) = persist_approved_two_task_plan(&database).await;
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(tasks[0].id)
            .await
            .expect("failed to claim continued task")
    );
    insert_managed_child(&database, project_id, tasks[0].id, "child-protocol").await;
    seed_verifying_tasks(&database, &orchestration, &tasks).await;
    for task in &tasks {
        assert!(
            database
                .orchestrations()
                .record_orchestration_verdict(
                    orchestration.id,
                    &task.task_key,
                    true,
                    "Earlier verification",
                )
                .await
                .expect("failed to seed earlier verdict")
        );
        database
            .orchestrations()
            .update_orchestration_task_status(
                task.id,
                &OrchestrationTaskStatus::AwaitingIntegration.to_string(),
                None,
            )
            .await
            .expect("failed to park verified task");
    }
    database
        .orchestrations()
        .update_orchestration_status(
            orchestration.id,
            &OrchestrationStatus::AwaitingIntegration.to_string(),
        )
        .await
        .expect("failed to park campaign");
    let mut continuation = subtask("protocol", &["crates/ag-protocol/"]);
    continuation.prompt = "Address verification feedback".to_string();
    let mut response = AgentResponse::plain("Continue the protocol task");
    response.subtasks = vec![continuation];

    // Act
    persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect("continuation should route");
    let campaign = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load campaign")
        .expect("campaign should exist");
    let mut routed = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("failed to load routed tasks");
    database
        .orchestrations()
        .update_orchestration_task_status(
            routed[0].id,
            &OrchestrationTaskStatus::Ready.to_string(),
            None,
        )
        .await
        .expect("failed to settle continuation");
    routed = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("failed to reload settled tasks");
    let decision =
        OrchestrationPolicy::schedule(2, &routed.iter().map(task_status).collect::<Vec<_>>());

    // Assert
    assert_eq!(campaign.status, OrchestrationStatus::Running.to_string());
    assert_eq!(response.subtasks, [] as [ag_protocol::SubtaskItem; 0]);
    assert_eq!(routed[1].status, OrchestrationTaskStatus::Ready.to_string());
    assert_eq!(routed[1].verification_verdict, None);
    assert_eq!(routed[1].verification_reason, None);
    assert!(decision.should_submit);
}

#[tokio::test]
async fn controller_verdicts_admit_only_passed_tasks_to_integration() {
    // Arrange
    let (database, _) = controller_database().await;
    let (orchestration, tasks, _, _) = persist_approved_two_task_plan(&database).await;
    seed_verifying_tasks(&database, &orchestration, &tasks).await;
    let mut response = AgentResponse::plain("One task needs correction");
    response.verification_verdicts = vec![
        VerificationVerdictItem {
            reason: "Protocol criteria pass".to_string(),
            task_key: "  protocol  ".to_string(),
            verdict: VerificationVerdict::Pass,
        },
        VerificationVerdictItem {
            reason: "Duplicate must not override".to_string(),
            task_key: "protocol".to_string(),
            verdict: VerificationVerdict::Flag,
        },
        VerificationVerdictItem {
            reason: "UI criterion is missing".to_string(),
            task_key: "ui".to_string(),
            verdict: VerificationVerdict::Flag,
        },
        VerificationVerdictItem {
            reason: "Ignored blank key".to_string(),
            task_key: String::new(),
            verdict: VerificationVerdict::Pass,
        },
    ];

    // Act
    persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect("verdicts should persist");
    let completed = database
        .orchestrations()
        .complete_orchestration_rollup(orchestration.id)
        .await
        .expect("roll-up should complete");
    let prompt_outcome = approve_orchestration(database.orchestrations(), "controller", None)
        .await
        .expect("prompt eligibility should be inspected");
    let approval = approve_orchestration(
        database.orchestrations(),
        "controller",
        Some(IntegrationApproach::LocalMerge),
    )
    .await
    .expect("gate inspection should succeed");
    let campaign = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load campaign")
        .expect("campaign should exist");
    let verified = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("failed to load verified tasks");

    // Assert
    assert!(completed);
    assert_eq!(prompt_outcome, OrchestrationApprovalOutcome::Unavailable);
    assert_eq!(approval, OrchestrationApprovalOutcome::Unavailable);
    assert_eq!(
        campaign.status,
        OrchestrationStatus::AwaitingIntegration.to_string()
    );
    assert_eq!(
        verified[0].status,
        OrchestrationTaskStatus::AwaitingIntegration.to_string()
    );
    assert_eq!(verified[0].verification_verdict.as_deref(), Some("Pass"));
    assert_eq!(
        verified[1].status,
        OrchestrationTaskStatus::Ready.to_string()
    );
    assert_eq!(verified[1].verification_verdict.as_deref(), Some("Flag"));
    assert_eq!(
        verified[1].verification_reason.as_deref(),
        Some("UI criterion is missing")
    );
}

#[tokio::test]
async fn controller_verdicts_reject_unknown_task_keys() {
    // Arrange
    let (database, _) = controller_database().await;
    let (orchestration, tasks, _, _) = persist_approved_two_task_plan(&database).await;
    seed_verifying_tasks(&database, &orchestration, &tasks).await;
    let mut response = AgentResponse::plain("Unknown task verdict");
    response.verification_verdicts = vec![VerificationVerdictItem {
        reason: "Looks complete".to_string(),
        task_key: "unknown-task".to_string(),
        verdict: VerificationVerdict::Pass,
    }];

    // Act
    let error = persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect_err("unknown verdict keys should fail explicitly");

    // Assert
    assert!(matches!(
        error,
        DbError::InvalidData {
            entity: "orchestration verification verdict",
            reason,
        } if reason == format!(
            "task `unknown-task` did not match a ready task in orchestration {}",
            orchestration.id
        )
    ));
}

#[tokio::test]
async fn flagged_research_report_blocks_the_integration_gate() {
    // Arrange
    let (database, _) = controller_database().await;
    let mut response = AgentResponse::plain("Research architecture");
    response.subtasks = vec![research_subtask("architecture")];
    persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect("research plan should persist");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("research orchestration should load")
        .expect("research orchestration should exist");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("research task should load")
        .remove(0);
    database
        .orchestrations()
        .update_orchestration_task_status(
            task.id,
            &OrchestrationTaskStatus::Reported.to_string(),
            None,
        )
        .await
        .expect("research report should settle");
    database
        .orchestrations()
        .update_orchestration_status(
            orchestration.id,
            &OrchestrationStatus::Verifying.to_string(),
        )
        .await
        .expect("research wave should enter verification");
    assert!(
        database
            .orchestrations()
            .record_orchestration_verdict(
                orchestration.id,
                "architecture",
                false,
                "Missing dependency analysis",
            )
            .await
            .expect("research verdict should persist")
    );
    database
        .orchestrations()
        .complete_orchestration_rollup(orchestration.id)
        .await
        .expect("research roll-up should park at integration");

    // Act
    let outcome = approve_orchestration(
        database.orchestrations(),
        "controller",
        Some(IntegrationApproach::LocalMerge),
    )
    .await
    .expect("research gate should be inspected");

    // Assert
    assert_eq!(outcome, OrchestrationApprovalOutcome::Unavailable);
}

#[tokio::test]
async fn managed_child_evidence_records_paths_outside_expected_area_hints() {
    // Arrange
    let (database, project_id) = controller_database().await;
    let (orchestration, tasks, _, _) = persist_approved_two_task_plan(&database).await;
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(tasks[0].id)
            .await
            .expect("failed to claim task")
    );
    insert_managed_child(&database, project_id, tasks[0].id, "child-protocol").await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_diff_changed_files()
        .withf(|path, base| path == Path::new("/tmp/child-protocol") && base == "main")
        .once()
        .return_once(|_, _| {
            Box::pin(async {
                Ok(vec![
                    "crates/ag-protocol/src/model.rs".to_string(),
                    "README.md".to_string(),
                ])
            })
        });

    // Act
    persist_managed_child_area_compliance(
        &database,
        &git_client,
        "child-protocol",
        Path::new("/tmp/child-protocol"),
    )
    .await
    .expect("evidence should persist");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("task query should succeed")
        .remove(0);

    // Assert
    assert_eq!(task.areas_compliant, Some(false));
    assert_eq!(task.area_violations, r#"["README.md"]"#);

    // Arrange
    git_client
        .expect_diff_changed_files()
        .once()
        .return_once(|_, _| {
            Box::pin(async { Ok(vec!["crates/ag-protocol/src/lib.rs".to_string()]) })
        });

    // Act
    persist_managed_child_area_compliance(
        &database,
        &git_client,
        "child-protocol",
        Path::new("/tmp/child-protocol"),
    )
    .await
    .expect("compliant evidence should persist");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("task query should succeed")
        .remove(0);

    // Assert
    assert_eq!(task.areas_compliant, Some(true));
    assert_eq!(task.area_violations, "[]");
}

#[tokio::test]
async fn changed_managed_child_without_area_hints_remains_unchecked() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let (database, project_id) = controller_database().await;
    let (orchestration, tasks, _, _) = persist_approved_plan(
        &database,
        vec![
            subtask("protocol", &[]),
            subtask("ui", &["crates/agentty/src/ui/"]),
        ],
    )
    .await;
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(tasks[0].id)
            .await?
    );
    insert_managed_child(&database, project_id, tasks[0].id, "child-protocol").await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_diff_changed_files()
        .once()
        .return_once(|_, _| Box::pin(async { Ok(vec!["README.md".to_string()]) }));

    // Act
    persist_managed_child_area_compliance(
        &database,
        &git_client,
        "child-protocol",
        Path::new("/tmp/child-protocol"),
    )
    .await
    .map_err(std::io::Error::other)?;
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await?
        .into_iter()
        .find(|task| task.task_key == "protocol")
        .ok_or_else(|| std::io::Error::other("protocol task should exist"))?;
    let rollup = rollup_message("Complete the campaign", std::slice::from_ref(&task));

    // Assert
    assert_eq!(task.areas_compliant, None);
    assert_eq!(task.area_violations, "[]");
    assert_eq!(campaign_task_evidence(&task), "; areas not provided");
    assert!(rollup.contains("Expected areas: not provided"));
    assert!(rollup.contains("Expected-area comparison: not checked (areas not provided)"));

    Ok(())
}

#[tokio::test]
async fn ordinary_child_has_no_orchestration_evidence_scope() {
    // Arrange
    let (database, _) = controller_database().await;
    let git_client = MockGitClient::new();

    // Act
    let result = persist_managed_child_area_compliance(
        &database,
        &git_client,
        "not-managed",
        Path::new("/tmp/not-managed"),
    )
    .await;

    // Assert
    assert_eq!(result, Ok(()));
}

#[tokio::test]
async fn failed_task_retry_reuses_key_and_exposes_child_metadata() {
    // Arrange
    let (database, project_id) = controller_database().await;
    let (orchestration, tasks, _, _) = persist_approved_two_task_plan(&database).await;
    database
        .orchestrations()
        .update_orchestration_task_status(
            tasks[0].id,
            &OrchestrationTaskStatus::Failed.to_string(),
            Some("failed".to_string()),
        )
        .await
        .expect("failed to settle failed task");
    database
        .orchestrations()
        .update_orchestration_task_status(
            tasks[1].id,
            &OrchestrationTaskStatus::Ready.to_string(),
            None,
        )
        .await
        .expect("failed to settle ready task");
    database
        .orchestrations()
        .update_orchestration_status(orchestration.id, &OrchestrationStatus::Done.to_string())
        .await
        .expect("failed to settle orchestration");
    let mut retry_response = AgentResponse::plain("Retry");
    retry_response.subtasks = vec![subtask("protocol", &["crates/ag-protocol/"])];

    // Act
    persist_controller_plan(&database, "controller", &mut retry_response)
        .await
        .expect("retry should persist");
    let retried_tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("failed to load retried tasks");
    approve_orchestration(database.orchestrations(), "controller", None)
        .await
        .expect("retry approval should start orchestration");
    let claimed = database
        .orchestrations()
        .claim_orchestration_task(tasks[0].id)
        .await
        .expect("failed to claim retried task");
    database
        .sessions()
        .insert_session(
            "child-protocol",
            AgentKind::Codex.default_model().as_str(),
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert orchestration child");
    let linked = database
        .orchestrations()
        .link_orchestration_task_child(tasks[0].id, "child-protocol")
        .await
        .expect("failed to link orchestration child");
    let mut session_metadata = session_metadata_for_project(&database, project_id).await;
    let controller_metadata = session_metadata
        .remove("controller")
        .expect("controller metadata should load");
    let child_metadata = session_metadata
        .remove("child-protocol")
        .expect("child metadata should load");
    let active_child_count = running_child_count(&database, "controller").await;

    // Assert
    assert!(claimed);
    assert!(linked);
    assert_eq!(retried_tasks.len(), 2);
    assert_eq!(retried_tasks[0].id, tasks[0].id);
    assert_eq!(
        retried_tasks[0].status,
        OrchestrationTaskStatus::Proposed.to_string()
    );
    assert_eq!(
        retried_tasks[1].status,
        OrchestrationTaskStatus::Ready.to_string()
    );
    assert_eq!(
        controller_metadata.progress.as_deref(),
        Some("1 running, 0 waiting on you")
    );
    assert_eq!(
        child_metadata.controller_session_id,
        Some(SessionId::from("controller"))
    );
    assert_eq!(active_child_count, 1);
}

#[tokio::test]
async fn running_child_count_includes_reverse_linked_child() {
    // Arrange
    let (database, project_id) = controller_database().await;
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("orchestration should persist");
    let task_id = database
        .orchestrations()
        .upsert_orchestration_task(PersistedOrchestrationTask {
            acceptance_criteria: r#"["Protocol is implemented"]"#.to_string(),
            kind: OrchestrationTaskKind::Implementation.to_string(),
            merge_position: 0,
            prompt: "Implement protocol".to_string(),
            session_orchestration_id: orchestration_id,
            task_key: "protocol".to_string(),
            title: "Protocol".to_string(),
            touched_areas: r#"["crates/ag-protocol/"]"#.to_string(),
        })
        .await
        .expect("task should persist");
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("task should be claimed")
    );
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "reverse-linked-child",
            is_draft: false,
            model: AgentKind::Codex.default_model().as_str(),
            orchestration_task_id: Some(task_id),
            parent_session_id: None,
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: None,
            speed_mode: SpeedMode::Normal,
            status: "InProgress",
        })
        .await
        .expect("reverse-linked child should persist");

    // Act
    let active_child_count = running_child_count(&database, "controller").await;

    // Assert
    assert_eq!(active_child_count, 1);
}
