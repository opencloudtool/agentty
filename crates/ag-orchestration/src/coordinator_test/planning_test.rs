use super::*;

#[tokio::test]
async fn controller_response_without_subtasks_does_not_create_plan() {
    // Arrange
    let (database, _) = controller_database().await;
    let mut response = AgentResponse::plain("Use a regular session");

    // Act
    persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect("empty plan handling should succeed");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("orchestration lookup should succeed");

    // Assert
    assert!(orchestration.is_none());
    assert_eq!(response.questions, [] as [ag_protocol::QuestionItem; 0]);
}

#[tokio::test]
async fn controller_plan_persists_before_approval() {
    // Arrange
    let (database, _) = controller_database().await;
    database
        .settings()
        .upsert_setting(SettingName::OrchestrationParallelism, "4")
        .await
        .expect("failed to seed orchestration parallelism");
    let unchanged_prompt = TurnPrompt::from_text("ordinary work".to_string());
    let controller_turn = controller_prompt(
        &database,
        "controller",
        TurnPrompt::from_text("Build it".to_string()),
    )
    .await;
    let ordinary_turn = controller_prompt(&database, "missing", unchanged_prompt.clone()).await;
    let mut invalid_response = AgentResponse::plain("Invalid plan");
    invalid_response.subtasks = vec![subtask("protocol", &["crates/ag-protocol/"])];
    persist_controller_plan(&database, "controller", &mut invalid_response)
        .await
        .expect("invalid plan handling should succeed");

    // Act
    let (orchestration, tasks, response, approved_metadata) =
        persist_approved_two_task_plan(&database).await;
    let snapshot = controller_snapshot(&database, "controller").await;

    // Assert
    assert!(
        controller_turn
            .agent_text()
            .contains("single-goal Agentty campaign")
    );
    assert_eq!(controller_turn.text_source, TurnPromptTextSource::AgentData);
    assert_eq!(ordinary_turn, unchanged_prompt);
    assert_eq!(
        invalid_response.subtasks,
        [] as [ag_protocol::SubtaskItem; 0]
    );
    assert!(invalid_response.questions[0].text.contains("at least two"));
    assert_eq!(
        invalid_response.questions[0].options,
        ["Revise the plan", "Use a regular session"]
    );
    assert!(controller_turn.agent_text().ends_with("Build it"));
    assert_eq!(response.questions, [] as [ag_protocol::QuestionItem; 0]);
    assert_eq!(orchestration.goal_statement, "Plan");
    assert_eq!(orchestration.max_parallelism, 4);
    assert_eq!(tasks.len(), 2);
    let snapshot = serde_json::from_str::<serde_json::Value>(&snapshot)
        .expect("controller snapshot should be JSON");
    assert_eq!(snapshot["phase"], "Running");
    assert_eq!(snapshot["max_parallelism"], 4);
    assert_eq!(snapshot["omitted_task_count"], 0);
    assert_eq!(snapshot["tasks"][0]["task_key"], "protocol");
    assert_eq!(snapshot["tasks"][0]["status"], "Planned");
    assert_eq!(
        snapshot["tasks"][0]["touched_areas"],
        serde_json::json!(["crates/ag-protocol/"])
    );
    assert_eq!(snapshot["tasks"][0]["metadata_truncated"], false);
    assert!(snapshot["tasks"][0].get("title").is_none());
    assert!(snapshot["tasks"][0].get("acceptance_criteria").is_none());
    assert_eq!(
        approved_metadata.progress.as_deref(),
        Some("0 running, 0 waiting on you")
    );
}

#[tokio::test]
async fn research_only_plan_auto_approves_by_default_and_can_be_parked_by_setting() {
    // Arrange
    let (auto_database, _) = controller_database().await;
    let mut auto_response = AgentResponse::plain("Research the architecture");
    auto_response.subtasks = vec![research_subtask("architecture")];
    let (parked_database, _) = controller_database().await;
    parked_database
        .settings()
        .upsert_setting(SettingName::AutoApproveOrchestrationResearch, "false")
        .await
        .expect("research auto-approval setting should persist");
    let mut parked_response = AgentResponse::plain("Research security");
    parked_response.subtasks = vec![research_subtask("security")];

    // Act
    persist_controller_plan(&auto_database, "controller", &mut auto_response)
        .await
        .expect("default research wave should persist");
    persist_controller_plan(&parked_database, "controller", &mut parked_response)
        .await
        .expect("parked research wave should persist");
    let auto_orchestration = auto_database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("auto-approved orchestration should load")
        .expect("auto-approved orchestration should exist");
    let auto_tasks = auto_database
        .orchestrations()
        .load_orchestration_tasks(auto_orchestration.id)
        .await
        .expect("auto-approved research tasks should load");
    let parked_orchestration = parked_database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("parked orchestration should load")
        .expect("parked orchestration should exist");

    // Assert
    assert_eq!(
        auto_orchestration.status,
        OrchestrationStatus::Running.to_string()
    );
    assert_eq!(auto_tasks.len(), 1);
    assert_eq!(
        auto_tasks[0].kind,
        OrchestrationTaskKind::Research.to_string()
    );
    assert_eq!(
        auto_tasks[0].status,
        OrchestrationTaskStatus::Planned.to_string()
    );
    assert_eq!(auto_tasks[0].touched_areas, "[]");
    assert_eq!(
        parked_orchestration.status,
        OrchestrationStatus::AwaitingApproval.to_string()
    );
}

#[tokio::test]
async fn verified_research_can_route_a_separate_implementation_wave() {
    // Arrange
    let (database, _) = controller_database().await;
    let mut research_response = AgentResponse::plain("Research the architecture");
    research_response.subtasks = vec![research_subtask("architecture")];
    persist_controller_plan(&database, "controller", &mut research_response)
        .await
        .expect("research wave should persist");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("research orchestration should load")
        .expect("research orchestration should exist");
    let research_task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("research task should load")
        .remove(0);
    database
        .orchestrations()
        .update_orchestration_task_status(
            research_task.id,
            &OrchestrationTaskStatus::Reported.to_string(),
            None,
        )
        .await
        .expect("research task should report");
    database
        .orchestrations()
        .update_orchestration_status(
            orchestration.id,
            &OrchestrationStatus::Verifying.to_string(),
        )
        .await
        .expect("research wave should verify");
    let mut implementation_response = AgentResponse::plain("Implement the verified design");
    implementation_response.verification_verdicts = vec![VerificationVerdictItem {
        reason: "Architecture boundaries are mapped".to_string(),
        task_key: "architecture".to_string(),
        verdict: VerificationVerdict::Pass,
    }];
    implementation_response.subtasks = vec![
        subtask("protocol", &["crates/ag-protocol/"]),
        subtask("ui", &["crates/agentty/src/ui/"]),
    ];

    // Act
    persist_controller_plan(&database, "controller", &mut implementation_response)
        .await
        .expect("implementation wave should route after research");
    let routed_orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("routed orchestration should load")
        .expect("routed orchestration should exist");
    let routed_tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("routed tasks should load");

    // Assert
    assert_eq!(
        routed_orchestration.status,
        OrchestrationStatus::AwaitingApproval.to_string()
    );
    assert_eq!(routed_tasks.len(), 3);
    assert_eq!(
        routed_tasks[0].verification_verdict.as_deref(),
        Some("Pass")
    );
    assert!(routed_tasks[1..].iter().all(|task| {
        task.kind == OrchestrationTaskKind::Implementation.to_string()
            && task.status == OrchestrationTaskStatus::Proposed.to_string()
    }));
    assert_eq!(implementation_response.subtasks, []);
    assert_eq!(implementation_response.questions, []);
}

#[tokio::test]
async fn active_research_correction_restarts_a_temporary_child_with_the_same_task_key() {
    // Arrange
    let (database, _) = controller_database().await;
    let mut initial = AgentResponse::plain("Research the architecture");
    initial.subtasks = vec![research_subtask("architecture")];
    persist_controller_plan(&database, "controller", &mut initial)
        .await
        .expect("research wave should persist");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("orchestration should load")
        .expect("orchestration should exist");
    let original_task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("research task should load")
        .remove(0);
    database
        .orchestrations()
        .update_orchestration_task_status(
            original_task.id,
            &OrchestrationTaskStatus::Reported.to_string(),
            None,
        )
        .await
        .expect("research task should settle");
    let mut correction = AgentResponse::plain("Deepen the research");
    correction.subtasks = vec![SubtaskItem {
        prompt: "Inspect architecture and dependency boundaries".to_string(),
        ..research_subtask("architecture")
    }];

    // Act
    persist_controller_plan(&database, "controller", &mut correction)
        .await
        .expect("research correction should route");
    let refreshed_orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("refreshed orchestration should load")
        .expect("refreshed orchestration should exist");
    let refreshed_tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("refreshed research task should load");

    // Assert
    assert_eq!(
        refreshed_orchestration.status,
        OrchestrationStatus::Running.to_string()
    );
    assert_eq!(refreshed_tasks.len(), 1);
    assert_eq!(refreshed_tasks[0].id, original_task.id);
    assert_eq!(
        refreshed_tasks[0].status,
        OrchestrationTaskStatus::Planned.to_string()
    );
    assert_eq!(
        refreshed_tasks[0].prompt,
        "Inspect architecture and dependency boundaries"
    );
    assert!(refreshed_tasks[0].research_report.is_none());
    assert_eq!(correction.subtasks, []);
}

#[tokio::test]
async fn revised_controller_plan_replaces_the_parked_plan() {
    // Arrange
    let (database, _) = controller_database().await;
    let mut response = AgentResponse::plain("Plan");
    response.subtasks = vec![
        subtask("protocol", &["crates/ag-protocol/"]),
        subtask("ui", &["crates/agentty/src/ui/"]),
    ];
    persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect("plan should persist");
    let original_id = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("orchestration should load")
        .expect("orchestration should exist")
        .id;
    let mut revised_response = AgentResponse::plain("Revised plan");
    revised_response.subtasks = vec![
        subtask("core", &["crates/agentty/src/app/"]),
        subtask("docs", &["docs/site/content/docs/"]),
    ];

    // Act
    persist_controller_plan(&database, "controller", &mut revised_response)
        .await
        .expect("revision should replace the plan");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("orchestration should load")
        .expect("orchestration should exist");

    // Assert
    assert_ne!(orchestration.id, original_id);
    assert_eq!(orchestration.goal_statement, "Revised plan");
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::AwaitingApproval.to_string()
    );
}

#[tokio::test]
async fn running_orchestration_discards_repeated_plan_output() {
    // Arrange
    let (database, _) = controller_database().await;
    let mut initial_response = AgentResponse::plain("Plan");
    initial_response.subtasks = vec![
        subtask("protocol", &["crates/ag-protocol/"]),
        subtask("ui", &["crates/agentty/src/ui/"]),
    ];
    persist_controller_plan(&database, "controller", &mut initial_response)
        .await
        .expect("initial plan should persist");
    approve_orchestration(database.orchestrations(), "controller", None)
        .await
        .expect("approval should start orchestration");
    let mut repeated_response = AgentResponse::plain("Approval received");
    repeated_response.subtasks = vec![
        subtask("protocol", &["crates/ag-protocol/"]),
        subtask("ui", &["crates/agentty/src/ui/"]),
    ];
    repeated_response.questions = vec![QuestionItem::new("Which worker needs more context?")];

    // Act
    persist_controller_plan(&database, "controller", &mut repeated_response)
        .await
        .expect("active plan handling should succeed");
    let mut discussion_response = AgentResponse::plain("Current status?");
    persist_controller_plan(&database, "controller", &mut discussion_response)
        .await
        .expect("active discussion should not replace the plan");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("orchestration should load")
        .expect("one orchestration should remain");
    let persisted_tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("orchestration tasks should load");

    // Assert
    assert_eq!(
        repeated_response.subtasks,
        [] as [ag_protocol::SubtaskItem; 0]
    );
    assert_eq!(
        repeated_response
            .questions
            .iter()
            .map(|question| question.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Which worker needs more context?"]
    );
    assert_eq!(
        persisted_tasks
            .iter()
            .map(|task| task.task_key.as_str())
            .collect::<Vec<_>>(),
        vec!["protocol", "ui"]
    );

    assert_eq!(
        orchestration.status,
        OrchestrationStatus::Running.to_string()
    );
}

#[tokio::test]
async fn mixed_follow_up_continues_live_child_and_gates_new_scope() {
    // Arrange
    let (database, project_id) = controller_database().await;
    let (initial_orchestration, tasks, _, _) = persist_approved_two_task_plan(&database).await;
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(tasks[0].id)
            .await
            .expect("failed to claim existing task")
    );
    insert_managed_child(&database, project_id, tasks[0].id, "child-protocol").await;
    database
        .orchestrations()
        .update_orchestration_task_status(
            tasks[0].id,
            &OrchestrationTaskStatus::Ready.to_string(),
            None,
        )
        .await
        .expect("failed to settle existing task");
    let mut continuation = subtask("protocol", &["docs/"]);
    continuation.prompt = "Add the missing validation".to_string();
    continuation.acceptance_criteria = vec!["Validation is covered".to_string()];
    let mut response = AgentResponse::plain("Routing feedback and new scope");
    response.subtasks = vec![continuation, subtask("docs", &["docs/site/content/docs/"])];
    // Act
    persist_controller_plan(&database, "controller", &mut response)
        .await
        .expect("mixed follow-up should route");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load campaign")
        .expect("campaign should exist");
    let routed_tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .expect("failed to load routed tasks");
    let continued = routed_tasks
        .iter()
        .find(|task| task.task_key == "protocol")
        .expect("continued task should remain");
    let continuation_prompt = continued.continuation_prompt.as_deref();
    let proposed = routed_tasks
        .iter()
        .find(|task| task.task_key == "docs")
        .expect("new task should be proposed");
    let approved = database
        .orchestrations()
        .approve_orchestration_plan(orchestration.id)
        .await
        .expect("new scope approval should succeed");

    // Assert
    assert_eq!(response.subtasks, [] as [ag_protocol::SubtaskItem; 0]);
    assert_eq!(orchestration.id, initial_orchestration.id);
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::AwaitingApproval.to_string()
    );
    assert_eq!(
        continued.status,
        OrchestrationTaskStatus::ContinuationPending.to_string()
    );
    assert_eq!(
        continued.child_session_id.as_deref(),
        Some("child-protocol")
    );
    assert_eq!(continued.continuation_generation, 1);
    assert_eq!(continuation_prompt, Some("Add the missing validation"));
    assert_eq!(continued.touched_areas, r#"["docs/"]"#);
    assert_eq!(
        proposed.status,
        OrchestrationTaskStatus::Proposed.to_string()
    );
    assert!(approved);
}
