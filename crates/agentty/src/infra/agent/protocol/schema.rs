//! JSON Schema generation and transport-compatibility normalization for the
//! structured response protocol.

use serde_json::Value;

use super::model::{AgentResponse, MAX_QUESTIONS};

/// Returns the JSON Schema used for structured assistant output.
///
/// The returned value is passed directly to providers that support enforced
/// output schemas. It starts from the self-descriptive response schema and then
/// applies compatibility normalization required by schema-enforcing agents.
pub(crate) fn agent_response_output_schema() -> Value {
    let mut value = agent_response_json_schema();
    normalize_schema_for_transport(&mut value);

    value
}

/// Returns a pretty-printed JSON Schema string for prompt instruction
/// templating.
///
/// This keeps the raw `schemars` metadata intact so inline prompt guidance can
/// show a fully self-descriptive schema document.
pub(crate) fn agent_response_json_schema_json() -> String {
    let schema = agent_response_json_schema();

    stringify_schema_json(&schema)
}

/// Returns a pretty-printed JSON Schema string for prompt instruction
/// templating.
///
/// This is used by prompt builders for providers that cannot enforce
/// `outputSchema` at transport level and must be guided by in-prompt schema
/// text instead, or by native schema-validation flags that accept a serialized
/// schema document.
pub(crate) fn agent_response_output_schema_json() -> String {
    let schema = agent_response_output_schema();

    stringify_schema_json(&schema)
}

/// Returns the self-descriptive JSON Schema for the response payload.
///
/// This preserves the raw `schemars` output, including metadata such as
/// `title` and `description`, so prompt templates can show models the richest
/// possible schema contract.
fn agent_response_json_schema() -> Value {
    let schema = schemars::schema_for!(AgentResponse);
    let mut schema_value = serde_json::to_value(schema).unwrap_or(Value::Null);

    inject_dynamic_schema_guidance(&mut schema_value);

    schema_value
}

/// Injects dynamic prompt guidance that depends on runtime constants into the
/// schema metadata shown to providers.
fn inject_dynamic_schema_guidance(schema: &mut Value) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(questions_property) = properties
        .get_mut("questions")
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    questions_property.insert(
        "description".to_string(),
        Value::String(format!(
            "Ordered clarification questions emitted for this turn. Emit at most {MAX_QUESTIONS} \
             items, and use an empty array when no user input is required. Defaults to an empty \
             array when omitted."
        )),
    );
}

/// Pretty-prints one schema document for prompt or transport wiring.
fn stringify_schema_json(schema: &Value) -> String {
    match serde_json::to_string_pretty(schema) {
        Ok(schema_json) => schema_json,
        Err(_) => "null".to_string(),
    }
}

/// Normalizes one schema tree for transport-level provider compatibility.
///
/// Codex rejects schemas that use `oneOf` for enum-like constants. Schemars
/// can emit this shape for simple Rust enums, so this normalizer rewrites
/// those fragments to string `enum` definitions.
fn normalize_schema_for_transport(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for nested_value in object.values_mut() {
                normalize_schema_for_transport(nested_value);
            }

            normalize_ref_object_for_codex(object);
            normalize_required_for_codex(object);

            let one_of_values = object
                .get("oneOf")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_object)
                        .map(|item| item.get("const").and_then(Value::as_str))
                        .collect::<Option<Vec<_>>>()
                })
                .map(|option| {
                    option.map(|values| {
                        values
                            .into_iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                });

            if let Some(Some(enum_variants)) = one_of_values {
                object.remove("oneOf");
                object.insert("type".to_string(), Value::String("string".to_string()));
                object.insert(
                    "enum".to_string(),
                    Value::Array(enum_variants.into_iter().map(Value::String).collect()),
                );
            }
        }
        Value::Array(array) => {
            for nested_value in array {
                normalize_schema_for_transport(nested_value);
            }
        }
        _ => {}
    }
}

/// Ensures all `properties` keys appear in `required` for Codex compatibility.
///
/// Codex rejects schemas where `properties` contains keys not listed in
/// `required`. Schemars omits optional fields from `required`, so this
/// normalizer adds any missing property keys.
fn normalize_required_for_codex(object: &mut serde_json::Map<String, Value>) {
    let Some(properties) = object.get("properties").and_then(Value::as_object) else {
        return;
    };

    let property_keys: Vec<String> = properties.keys().cloned().collect();
    if property_keys.is_empty() {
        return;
    }

    let required = object
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));

    let Some(required_array) = required.as_array_mut() else {
        return;
    };

    for key in &property_keys {
        let already_listed = required_array
            .iter()
            .any(|value| value.as_str() == Some(key));

        if !already_listed {
            required_array.push(Value::String(key.clone()));
        }
    }
}

/// Rewrites one `$ref` schema object to Codex-compatible form.
///
/// Codex rejects sibling keywords alongside `$ref` (for example
/// `{ "$ref": "...", "description": "..." }`), so this keeps only the
/// reference key when present.
fn normalize_ref_object_for_codex(object: &mut serde_json::Map<String, Value>) {
    let Some(reference) = object.get("$ref").cloned() else {
        return;
    };

    object.clear();
    object.insert("$ref".to_string(), reference);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Builds a schema object with required top-level response fields.
    fn test_agent_response_output_schema_contains_required_fields() {
        // Arrange / Act
        let schema = agent_response_output_schema();
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
                .any(|value| value.as_str() == Some("summary"))
        );
        assert!(properties.contains_key("answer"));
        assert!(properties.contains_key("questions"));
        assert!(properties.contains_key("summary"));
    }

    #[test]
    /// Ensures all transport schema object properties are listed in
    /// `required`.
    fn test_agent_response_output_schema_all_properties_are_required() {
        // Arrange / Act
        let schema = agent_response_output_schema();

        // Assert
        assert!(
            all_properties_in_required(&schema),
            "every object with `properties` should list all keys in `required`"
        );
    }

    #[test]
    /// Ensures generated schema avoids `oneOf` so Codex `outputSchema`
    /// validation accepts the payload.
    fn test_agent_response_output_schema_does_not_contain_one_of() {
        // Arrange / Act
        let schema = agent_response_output_schema();

        // Assert
        assert!(!contains_schema_key(&schema, "oneOf"));
    }

    #[test]
    /// Ensures no schema object uses `$ref` with sibling keys.
    fn test_agent_response_output_schema_ref_objects_have_no_sibling_keywords() {
        // Arrange / Act
        let schema = agent_response_output_schema();

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
        let summary_definition = schema
            .get("$defs")
            .and_then(|value| value.get("AgentResponseSummary"))
            .and_then(Value::as_object)
            .expect("summary definition should exist");

        // Assert
        assert_eq!(
            question_definition.get("title").and_then(Value::as_str),
            Some("QuestionItem")
        );
        assert_eq!(
            summary_definition.get("title").and_then(Value::as_str),
            Some("AgentResponseSummary")
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
        let question_definition = schema
            .get("$defs")
            .and_then(|value| value.get("QuestionItem"))
            .and_then(Value::as_object)
            .expect("question definition should exist");
        let question_properties = question_definition
            .get("properties")
            .and_then(Value::as_object)
            .expect("question properties should exist");
        let summary_definition = schema
            .get("$defs")
            .and_then(|value| value.get("AgentResponseSummary"))
            .and_then(Value::as_object)
            .expect("summary definition should exist");
        let summary_properties = summary_definition
            .get("properties")
            .and_then(Value::as_object)
            .expect("summary properties should exist");
        let expected_questions_description = format!(
            "Ordered clarification questions emitted for this turn. Emit at most {MAX_QUESTIONS} \
             items, and use an empty array when no user input is required. Defaults to an empty \
             array when omitted."
        );

        // Assert
        assert_schema_property_title_and_description(
            response_properties,
            "answer",
            "answer",
            "Markdown answer text for delivered work, status updates, or concise completion \
             notes. Keep clarification requests out of this field and emit them through \
             `questions` instead. Defaults to an empty string when omitted.",
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
            "summary",
            "summary",
            "Structured summary for session-discussion turns, kept outside `answer` markdown. Use \
             `null` for one-shot prompts and legacy payloads.",
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
            summary_properties,
            "turn",
            "turn",
            "Concise summary of only the work completed in the current turn.",
        );
        assert_schema_property_title_and_description(
            summary_properties,
            "session",
            "session",
            "Cumulative summary of active changes on the current session branch.",
        );
    }

    #[test]
    /// Preserves optional prompt fields in the raw schema instead of forcing
    /// transport-only requirements into prompt docs.
    fn test_agent_response_json_schema_keeps_optional_summary_field() {
        // Arrange / Act
        let schema = agent_response_json_schema();
        let response_required_fields = schema
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
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
            response_required_fields
                .iter()
                .all(|field| field.as_str() != Some("summary")),
            "raw prompt schema should keep optional summary fields optional"
        );
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
        let schema_json = agent_response_output_schema_json();
        let parsed_schema: Value =
            serde_json::from_str(&schema_json).expect("schema string should parse as JSON");
        let schema_value = agent_response_output_schema();

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
}
