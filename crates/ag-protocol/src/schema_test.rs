use super::*;

#[test]
/// Builds a schema object with required top-level response fields.
fn test_agent_response_output_schema_contains_required_fields() {
    // Arrange / Act
    let schema = agent_response_output_schema(SchemaRequiredPolicy::AllProperties);
    let required_fields = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("schema required fields should exist");
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("schema properties should exist");

    // Assert
    assert!(
        required_fields
            .iter()
            .any(|value| value.as_str() == Some("answer"))
    );
    assert!(
        required_fields
            .iter()
            .any(|value| value.as_str() == Some("questions"))
    );
    assert!(
        required_fields
            .iter()
            .any(|value| value.as_str() == Some("review_comment_outcomes"))
    );
    assert!(
        required_fields
            .iter()
            .any(|value| value.as_str() == Some("subtasks"))
    );
    assert!(properties.contains_key("answer"));
    assert!(properties.contains_key("questions"));
    assert!(properties.contains_key("review_comment_outcomes"));
    assert!(properties.contains_key("subtasks"));
}

#[test]
/// Leaves a schema untouched when a guidance-carrying property is absent,
/// so injection cannot panic or invent properties for partial schemas.
fn test_inject_dynamic_schema_guidance_skips_absent_properties() {
    // Arrange
    let mut schema = serde_json::json!({
        "properties": {
            "answer": { "type": "string" }
        }
    });

    // Act
    inject_dynamic_schema_guidance(&mut schema);

    // Assert
    assert_eq!(
        schema,
        serde_json::json!({
            "properties": {
                "answer": { "type": "string" }
            }
        })
    );
}

#[test]
/// Routes the `subtasks` schema description through the shared template
/// helper so the prompt-visible cap cannot drift from the parser cap.
fn test_agent_response_json_schema_injects_subtasks_description() {
    // Arrange / Act
    let schema = agent_response_json_schema();
    let response_properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("response properties should exist");
    let subtask_properties = schema_definition_properties(&schema, "SubtaskItem");

    // Assert
    assert_eq!(
        response_properties
            .get("subtasks")
            .and_then(|value| value.get("description"))
            .and_then(Value::as_str),
        Some(subtasks_field_description().as_str())
    );
    assert!(subtask_properties.contains_key("prompt"));
    assert!(subtask_properties.contains_key("kind"));
    assert!(subtask_properties.contains_key("task_key"));
    assert!(subtask_properties.contains_key("title"));
    assert!(subtask_properties.contains_key("touched_areas"));
}

#[test]
/// Ensures all transport schema object properties are listed in
/// `required`.
fn test_agent_response_output_schema_all_properties_are_required() {
    // Arrange / Act
    let schema = agent_response_output_schema(SchemaRequiredPolicy::AllProperties);

    // Assert
    assert!(
        all_properties_in_required(&schema),
        "every object with `properties` should list all keys in `required`"
    );
}

#[test]
/// Ensures the minimum-key policy demands only `answer`, so validators that
/// enforce `required` literally still accept replies that omit optional
/// protocol keys.
fn test_agent_response_output_schema_minimum_policy_requires_only_answer() {
    // Arrange / Act
    let schema = agent_response_output_schema(SchemaRequiredPolicy::MinimumProtocolKeys);
    let required_fields = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("schema required fields should exist");

    // Assert
    assert_eq!(
        required_fields,
        &vec![Value::String("answer".to_string())],
        "only `answer` should be required; demanding optional response fields rejects ordinary \
         replies that omit them"
    );
}

#[test]
/// Ensures generated schema avoids `oneOf` so Codex `outputSchema`
/// validation accepts the payload.
fn test_agent_response_output_schema_does_not_contain_one_of() {
    // Arrange / Act
    let schema = agent_response_output_schema(SchemaRequiredPolicy::AllProperties);

    // Assert
    assert!(!contains_schema_key(&schema, "oneOf"));
}

#[test]
/// Ensures generated transport schemas omit `$schema` metadata so Claude
/// native schema validation does not need a bundled meta-schema resolver.
fn test_agent_response_output_schema_does_not_contain_schema_metadata() {
    // Arrange / Act
    let schema = agent_response_output_schema(SchemaRequiredPolicy::MinimumProtocolKeys);

    // Assert
    assert!(!contains_schema_key(&schema, "$schema"));
}

#[test]
/// Ensures the prompt schema requires `answer` so empty objects are
/// rejected by schema validation the same way the parser rejects them.
fn test_agent_response_json_schema_requires_answer_key() {
    // Arrange / Act
    let schema = agent_response_json_schema();
    let required_fields = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("schema required fields should exist");

    // Assert
    assert!(
        required_fields
            .iter()
            .any(|value| value.as_str() == Some("answer")),
        "prompt schema should require `answer` to align with parser key-presence check"
    );
}

#[test]
/// Ensures every schema object with `properties` declares
/// `additionalProperties: false` so prompt guidance and transport
/// enforcement tell models not to add extra fields.
fn test_agent_response_json_schema_sets_additional_properties_false() {
    // Arrange / Act
    let schema = agent_response_json_schema();

    // Assert
    assert!(
        all_properties_objects_deny_additional(&schema),
        "every object with `properties` should set `additionalProperties: false`"
    );
}

#[test]
/// `inject_additional_properties_false` preserves a pre-existing
/// `additionalProperties` value instead of overwriting it.
fn test_inject_additional_properties_false_preserves_existing_value() {
    // Arrange
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "extra": { "type": "object", "additionalProperties": { "type": "string" } }
        }
    });

    // Act
    inject_additional_properties_false(&mut schema);

    // Assert - top-level gets injected (was absent)
    assert_eq!(schema["additionalProperties"], Value::Bool(false));
    // Assert - nested keeps its original map-type constraint (was present)
    assert_eq!(
        schema["properties"]["extra"]["additionalProperties"],
        serde_json::json!({ "type": "string" })
    );
}

#[test]
/// Ensures no schema object uses `$ref` with sibling keys.
fn test_agent_response_output_schema_ref_objects_have_no_sibling_keywords() {
    // Arrange / Act
    let schema = agent_response_output_schema(SchemaRequiredPolicy::AllProperties);

    // Assert
    assert!(!contains_ref_with_sibling_keywords(&schema));
}

#[test]
/// Exposes a parseable pretty JSON schema string for prompt templating.
fn test_agent_response_json_schema_json_is_parseable_value() {
    // Arrange / Act
    let schema_json = agent_response_json_schema_json();
    let parsed_schema: Value =
        serde_json::from_str(&schema_json).expect("schema string should parse as JSON");
    let schema_value = agent_response_json_schema();

    // Assert
    assert_eq!(parsed_schema, schema_value);
}

#[test]
fn focused_review_json_schema_describes_structured_findings() {
    // Arrange / Act
    let schema_json = focused_review_json_schema_json();
    let schema: Value =
        serde_json::from_str(&schema_json).expect("focused review schema should parse");
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("focused review properties should exist");
    let suggestion_properties = schema_definition_properties(&schema, "FocusedReviewSuggestion");

    // Assert
    assert_eq!(
        schema.get("title").and_then(Value::as_str),
        Some("FocusedReview")
    );
    assert_eq!(
        schema.get("additionalProperties"),
        Some(&Value::Bool(false))
    );
    assert!(properties.contains_key("project_impact"));
    assert!(properties.contains_key("suggestions"));
    assert_eq!(
        suggestion_properties.get("additionalProperties"),
        None,
        "definition properties should contain only fields"
    );
    assert!(suggestion_properties.contains_key("details"));
    assert!(suggestion_properties.contains_key("severity"));
    assert_eq!(
        schema["$defs"]["FocusedReviewSuggestion"]["additionalProperties"],
        Value::Bool(false)
    );
}

#[test]
fn focused_review_output_schema_is_transport_compatible() {
    // Arrange / Act
    let schema = focused_review_output_schema();

    // Assert
    assert_eq!(schema.get("$schema"), None);
    assert_eq!(
        schema.get("required"),
        Some(&serde_json::json!(["project_impact", "suggestions"]))
    );
    assert_eq!(schema["additionalProperties"], Value::Bool(false));
    assert_eq!(schema["properties"].get("answer"), None);
}

#[test]
/// Keeps response schemas self-descriptive so inline schema docs include
/// explicit top-level `schemars` metadata.
fn test_agent_response_json_schema_preserves_explicit_payload_metadata() {
    // Arrange / Act
    let schema = agent_response_json_schema();

    // Assert
    assert_eq!(
        schema.get("title").and_then(Value::as_str),
        Some("AgentResponse")
    );
    assert_eq!(
        schema.get("description").and_then(Value::as_str),
        Some(
            "Wire-format protocol payload used for schema-driven provider output. Return this \
             object as the entire assistant response payload. Providers that support output \
             schemas (for example, Codex app-server) are asked to emit this object directly."
        )
    );
}

#[test]
/// Keeps nested response-schema models self-descriptive for inline docs.
fn test_agent_response_json_schema_preserves_nested_metadata() {
    // Arrange / Act
    let schema = agent_response_json_schema();
    let question_definition = schema
        .get("$defs")
        .and_then(|value| value.get("QuestionItem"))
        .and_then(Value::as_object)
        .expect("question definition should exist");
    let review_comment_outcome_definition = schema
        .get("$defs")
        .and_then(|value| value.get("ReviewCommentOutcome"))
        .and_then(Value::as_object)
        .expect("review comment outcome definition should exist");
    let review_comment_resolution_definition = schema
        .get("$defs")
        .and_then(|value| value.get("ReviewCommentResolution"))
        .and_then(Value::as_object)
        .expect("review comment resolution definition should exist");

    // Assert
    assert_eq!(
        question_definition.get("title").and_then(Value::as_str),
        Some("QuestionItem")
    );
    assert_eq!(
        review_comment_outcome_definition
            .get("title")
            .and_then(Value::as_str),
        Some("ReviewCommentOutcome")
    );
    assert_eq!(
        review_comment_resolution_definition
            .get("title")
            .and_then(Value::as_str),
        Some("ReviewCommentResolution")
    );
}

#[test]
/// Keeps response-schema fields self-descriptive for inline schema docs.
fn test_agent_response_json_schema_preserves_field_metadata() {
    // Arrange / Act
    let schema = agent_response_json_schema();
    let response_properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("response properties should exist");
    let question_properties = schema_definition_properties(&schema, "QuestionItem");
    let review_comment_outcome_properties =
        schema_definition_properties(&schema, "ReviewCommentOutcome");
    let expected_questions_description = questions_field_description();

    // Assert
    assert_schema_property_title_and_description(
        response_properties,
        "answer",
        "answer",
        "Markdown answer text for delivered work, status updates, or concise completion notes. \
         Keep clarification requests out of this field and emit them through `questions` instead.",
    );
    assert_eq!(
        response_properties
            .get("questions")
            .and_then(|value| value.get("description"))
            .and_then(Value::as_str),
        Some(expected_questions_description.as_str())
    );
    assert_schema_property_title_and_description(
        response_properties,
        "review_comment_outcomes",
        "review_comment_outcomes",
        "Per-thread outcomes for an agent-driven forge comment-resolution turn. Emit an empty \
         array unless the prompt explicitly supplies forge thread IDs. Copy each reported \
         `thread_id` exactly from the prompt.",
    );
    assert_schema_property_title_and_description(
        question_properties,
        "text",
        "text",
        "Human-readable markdown text for this question. Ask one specific actionable question \
         instead of bundling multiple decisions into one item.",
    );
    assert_schema_property_title(question_properties, "options", "options");
    assert_schema_property_title_and_description(
        review_comment_outcome_properties,
        "reply",
        "reply",
        "Concise reply suitable for posting to the forge review thread.",
    );
    assert_schema_property_title_and_description(
        review_comment_outcome_properties,
        "resolution",
        "resolution",
        "Whether the targeted thread was fixed or required no change.",
    );
    assert_schema_property_title_and_description(
        review_comment_outcome_properties,
        "thread_id",
        "thread_id",
        "Opaque forge thread identifier copied exactly from the turn prompt.",
    );
}

#[test]
/// Preserves optional prompt fields in the raw schema instead of forcing
/// transport-only requirements into prompt docs.
fn test_agent_response_json_schema_keeps_optional_question_options() {
    // Arrange / Act
    let schema = agent_response_json_schema();
    let question_definition = schema
        .get("$defs")
        .and_then(|value| value.get("QuestionItem"))
        .and_then(Value::as_object)
        .expect("question definition should exist");
    let question_required_fields = question_definition
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Assert
    assert!(
        question_required_fields
            .iter()
            .all(|field| field.as_str() != Some("options")),
        "question schema should keep `options` optional for omitted empty lists"
    );
}

#[test]
/// Exposes a parseable pretty JSON schema string for transport-level
/// schema enforcement.
fn test_agent_response_output_schema_json_is_parseable_value() {
    // Arrange / Act
    let schema_json = agent_response_output_schema_json(SchemaRequiredPolicy::MinimumProtocolKeys);
    let parsed_schema: Value =
        serde_json::from_str(&schema_json).expect("schema string should parse as JSON");
    let schema_value = agent_response_output_schema(SchemaRequiredPolicy::MinimumProtocolKeys);

    // Assert
    assert_eq!(parsed_schema, schema_value);
}

/// Recursively checks whether one JSON value tree contains a schema key.
fn contains_schema_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(object) => {
            if object.contains_key(key) {
                return true;
            }

            object
                .values()
                .any(|nested_value| contains_schema_key(nested_value, key))
        }
        Value::Array(array) => array
            .iter()
            .any(|nested_value| contains_schema_key(nested_value, key)),
        _ => false,
    }
}

/// Recursively checks whether any `$ref` object has extra sibling keys.
fn contains_ref_with_sibling_keywords(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            if object.contains_key("$ref") && object.len() > 1 {
                return true;
            }

            object.values().any(contains_ref_with_sibling_keywords)
        }
        Value::Array(array) => array.iter().any(contains_ref_with_sibling_keywords),
        _ => false,
    }
}

/// Recursively checks that every object with `properties` lists all
/// property keys in `required`.
fn all_properties_in_required(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                let required_keys: Vec<&str> = object
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|array| array.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();

                for key in properties.keys() {
                    if !required_keys.contains(&key.as_str()) {
                        return false;
                    }
                }
            }

            object.values().all(all_properties_in_required)
        }
        Value::Array(array) => array.iter().all(all_properties_in_required),
        _ => true,
    }
}

/// Recursively checks that every object with `properties` sets
/// `additionalProperties: false`.
fn all_properties_objects_deny_additional(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            if object.contains_key("properties")
                && object.get("additionalProperties") != Some(&Value::Bool(false))
            {
                return false;
            }

            object.values().all(all_properties_objects_deny_additional)
        }
        Value::Array(array) => array.iter().all(all_properties_objects_deny_additional),
        _ => true,
    }
}

/// Returns the properties object for one named schema definition.
fn schema_definition_properties<'a>(
    schema: &'a Value,
    definition_name: &str,
) -> &'a serde_json::Map<String, Value> {
    schema
        .get("$defs")
        .and_then(|value| value.get(definition_name))
        .and_then(|value| value.get("properties"))
        .and_then(Value::as_object)
        .expect("schema definition properties should exist")
}

/// Asserts one property schema has the expected `title`.
fn assert_schema_property_title(
    properties: &serde_json::Map<String, Value>,
    property_name: &str,
    expected_title: &str,
) {
    assert_eq!(
        properties
            .get(property_name)
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str),
        Some(expected_title)
    );
}

/// Asserts one property schema has the expected `title` and
/// `description`.
fn assert_schema_property_title_and_description(
    properties: &serde_json::Map<String, Value>,
    property_name: &str,
    expected_title: &str,
    expected_description: &str,
) {
    assert_schema_property_title(properties, property_name, expected_title);
    assert_eq!(
        properties
            .get(property_name)
            .and_then(|value| value.get("description"))
            .and_then(Value::as_str),
        Some(expected_description)
    );
}
