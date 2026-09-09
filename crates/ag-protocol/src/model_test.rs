use super::*;

#[test]
/// Ensures the dynamic `questions` field description renders from the
/// checked-in prompt-schema template.
fn test_questions_field_description_renders_template_limit() {
    // Arrange
    let expected_limit = format!("Emit at most {MAX_QUESTIONS} items");

    // Act
    let description = questions_field_description();
    let normalized_description = description.split_whitespace().collect::<Vec<_>>().join(" ");

    // Assert
    assert!(normalized_description.contains(&expected_limit));
    assert!(normalized_description.contains("Emit an empty array when no input is required"));
    assert!(normalized_description.contains("field defaults to an empty array when omitted"));
    assert!(normalized_description.contains("genuinely ambiguous requirement"));
    assert!(normalized_description.contains("Never request permission for agreed work"));
    assert!(normalized_description.contains("ask for satisfaction or sign-off"));
    assert!(normalized_description.contains("Execute agreed work"));
    assert!(!description.contains("{{ max_questions }}"));
}

#[test]
/// Ensures display text includes the answer and clarification questions in
/// order.
fn test_agent_response_to_display_text_joins_answer_and_questions() {
    // Arrange
    let response = AgentResponse {
        answer: "Primary answer".to_string(),
        questions: vec![QuestionItem::new("Need one clarification.")],
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let display_text = response.to_display_text();

    // Assert
    assert_eq!(display_text, "Primary answer\n\nNeed one clarification.");
}

#[test]
/// Preserves review-comment outcomes through the wire JSON contract.
fn test_agent_response_review_comment_outcomes_round_trip() {
    // Arrange
    let response = AgentResponse {
        answer: "Addressed the comment.".to_string(),
        questions: Vec::new(),
        review_comment_outcomes: vec![ReviewCommentOutcome {
            reply: "Added the missing validation.".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-42".to_string(),
        }],
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let serialized = serde_json::to_string(&response).expect("response should serialize");
    let deserialized =
        serde_json::from_str::<AgentResponse>(&serialized).expect("response should deserialize");

    // Assert
    assert_eq!(deserialized, response);
    assert!(serialized.contains(r#""resolution":"fixed""#));
}

#[test]
/// Ensures question extraction respects the protocol question cap.
fn test_agent_response_question_items_applies_question_cap() {
    // Arrange
    let response = AgentResponse {
        answer: String::new(),
        questions: (0..=MAX_QUESTIONS)
            .map(|index| QuestionItem::new(format!("Question {index}")))
            .collect(),
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let questions = response.question_items();

    // Assert
    assert_eq!(questions.len(), MAX_QUESTIONS);
}

#[test]
/// Ensures subtask extraction respects the protocol subtask cap so a
/// runaway plan cannot fan out past the bounded child-session budget.
fn test_agent_response_subtask_items_applies_subtask_cap() {
    // Arrange
    let response = AgentResponse {
        answer: String::new(),
        questions: Vec::new(),
        review_comment_outcomes: Vec::new(),
        subtasks: (0..=MAX_SUBTASKS).map(test_subtask).collect(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let subtasks = response.subtask_items();

    // Assert
    assert_eq!(subtasks.len(), MAX_SUBTASKS);
    assert_eq!(subtasks[0].task_key, "task-0");
}

#[test]
/// Preserves proposed subtasks through the wire JSON contract and keeps
/// them absent from ordinary responses.
fn test_agent_response_subtasks_round_trip() {
    // Arrange
    let response = AgentResponse {
        answer: "Proposed a plan.".to_string(),
        questions: Vec::new(),
        review_comment_outcomes: Vec::new(),
        subtasks: vec![test_subtask(1)],
        verification_verdicts: Vec::new(),
    };

    // Act
    let serialized = serde_json::to_string(&response).expect("response should serialize");
    let deserialized =
        serde_json::from_str::<AgentResponse>(&serialized).expect("response should deserialize");

    // Assert
    assert_eq!(deserialized, response);
    assert!(serialized.contains(r#""task_key":"task-1""#));
    assert_eq!(
        AgentResponse::plain("no plan").subtask_items(),
        [] as [crate::subtask::SubtaskItem; 0]
    );
}

#[test]
/// Preserves typed verification decisions through JSON and applies the
/// same bounded task count as orchestration plans.
fn test_agent_response_verification_verdicts_round_trip_and_cap() {
    // Arrange
    let response = AgentResponse {
        answer: "Verified the settled tasks.".to_string(),
        questions: Vec::new(),
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: (0..=MAX_SUBTASKS)
            .map(|index| VerificationVerdictItem {
                reason: format!("Evidence {index}"),
                task_key: format!("task-{index}"),
                verdict: crate::VerificationVerdict::Pass,
            })
            .collect(),
    };

    // Act
    let serialized = serde_json::to_string(&response).expect("response should serialize");
    let deserialized =
        serde_json::from_str::<AgentResponse>(&serialized).expect("response should deserialize");
    let verdicts = deserialized.verification_verdict_items();

    // Assert
    assert_eq!(verdicts.len(), MAX_SUBTASKS);
    assert_eq!(verdicts[0].task_key, "task-0");
    assert!(serialized.contains(r#""verdict":"pass""#));
}

#[test]
/// Ensures optional `touched_areas` planning guidance defaults to an empty
/// list instead of failing the whole turn.
fn test_subtask_item_defaults_touched_areas() {
    // Arrange
    let raw = r#"{"prompt":"Do the work","task_key":"task-1","title":"Work"}"#;

    // Act
    let subtask =
        serde_json::from_str::<SubtaskItem>(raw).expect("subtask should parse without areas");

    // Assert
    assert_eq!(subtask.kind, crate::SubtaskKind::Implementation);
    assert_eq!(subtask.touched_areas, [] as [std::string::String; 0]);
}

#[test]
/// Keeps the injected `subtasks` schema description in sync with the
/// server-side cap the parser enforces.
fn test_subtasks_field_description_reports_the_subtask_cap() {
    // Arrange
    let expected_limit = format!("at most {MAX_SUBTASKS} items");

    // Act
    let description = subtasks_field_description();
    let normalized_description = description.split_whitespace().collect::<Vec<_>>().join(" ");

    // Assert
    assert!(description.contains(&expected_limit));
    assert!(
        normalized_description.contains("Emit an empty array when no decomposition was requested")
    );
    assert!(normalized_description.contains("field defaults to an empty array when omitted"));
    assert!(normalized_description.contains("Ordinary session and utility turns"));
    assert!(normalized_description.contains("unattended in its own worktree"));
    assert!(normalized_description.contains("independently completable"));
    assert!(normalized_description.contains("without wildcards"));
    assert!(description.contains("Areas may overlap"));
    assert!(normalized_description.contains("fewer than two independent subtasks"));
    assert!(!description.contains("{{"));
}

#[test]
/// Substitutes a cap placeholder that markdown reflowing wrapped across a
/// line break, so reformatting a template cannot leak raw `{{ ... }}` text
/// into the schema description models read.
fn test_field_description_template_survives_a_wrapped_placeholder() {
    // Arrange
    let template = "Emit at most {{\nmax_items }} items, and no more.\n";

    // Act
    let rendered = render_field_description_template(template, "{{ max_items }}", 4);

    // Assert
    assert_eq!(rendered, "Emit at most 4 items, and no more.");
}

#[test]
/// Leaves an unterminated placeholder untouched instead of truncating the
/// remaining guidance text.
fn test_field_description_template_keeps_unterminated_placeholder_text() {
    // Arrange
    let template = "Emit at most {{ max_items items.";

    // Act
    let rendered = render_field_description_template(template, "{{ max_items }}", 4);

    // Assert
    assert_eq!(rendered, "Emit at most {{ max_items items.");
}

/// Builds one deterministic subtask with a touched-area planning hint.
fn test_subtask(index: usize) -> SubtaskItem {
    SubtaskItem {
        acceptance_criteria: vec![format!("Work item {index} is complete")],
        kind: crate::SubtaskKind::Implementation,
        prompt: format!("Complete work item {index}"),
        task_key: format!("task-{index}"),
        title: format!("Work item {index}"),
        touched_areas: vec![format!("crates/area-{index}/")],
    }
}
