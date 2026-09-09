//! JSON Schema generation and transport-compatibility normalization for the
//! structured response protocol.

use serde_json::Value;

use super::model::{AgentResponse, questions_field_description, subtasks_field_description};
use super::review::FocusedReview;

/// Selects how a provider transport lists `required` schema properties.
///
/// Providers disagree on what a valid schema looks like. Codex rejects schemas
/// whose `properties` contain keys missing from `required`, so it needs every
/// key listed. Validators that enforce `required` literally, such as Claude,
/// must only demand `answer`; listing optional keys there rejects ordinary
/// replies that omit optional array fields, even though the parser accepts
/// them through `#[serde(default)]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaRequiredPolicy {
    /// Lists every `properties` key in `required` for Codex compatibility.
    AllProperties,
    /// Lists only the minimum protocol keys the parser insists on.
    MinimumProtocolKeys,
}

/// Returns the JSON Schema used for structured assistant output.
///
/// The returned value is passed directly to providers that support enforced
/// output schemas. It starts from the self-descriptive response schema and then
/// applies compatibility normalization required by schema-enforcing agents.
/// `required_policy` selects how strictly optional protocol keys are demanded.
pub fn agent_response_output_schema(required_policy: SchemaRequiredPolicy) -> Value {
    let mut value = agent_response_json_schema();
    normalize_schema_for_transport(&mut value, required_policy);

    value
}

/// Returns a pretty-printed JSON Schema string for prompt instruction
/// templating.
///
/// This keeps the raw `schemars` metadata intact so inline prompt guidance can
/// show a fully self-descriptive schema document.
pub fn agent_response_json_schema_json() -> String {
    let schema = agent_response_json_schema();

    stringify_schema_json(&schema)
}

/// Returns a pretty-printed, transport-normalized JSON Schema string.
///
/// Provider adapters use this serialized schema document when their native
/// structured-output API accepts JSON text. `required_policy` selects the
/// provider-compatible `required` field normalization.
pub fn agent_response_output_schema_json(required_policy: SchemaRequiredPolicy) -> String {
    let schema = agent_response_output_schema(required_policy);

    stringify_schema_json(&schema)
}

/// Returns a pretty-printed JSON Schema string for focused-review prompt
/// instructions.
///
/// Focused review uses a direct request-specific object so native structured
/// output transports can enforce every review field.
pub fn focused_review_json_schema_json() -> String {
    let schema_value = focused_review_json_schema();

    stringify_schema_json(&schema_value)
}

/// Returns the transport-normalized JSON Schema for direct focused-review
/// output.
pub fn focused_review_output_schema() -> Value {
    let mut value = focused_review_json_schema();
    normalize_schema_for_transport(&mut value, SchemaRequiredPolicy::AllProperties);

    value
}

/// Returns the output schema selected for one protocol request profile.
pub fn protocol_output_schema(
    profile: super::model::ProtocolRequestProfile,
    required_policy: SchemaRequiredPolicy,
) -> Value {
    if matches!(profile, super::model::ProtocolRequestProfile::FocusedReview) {
        return focused_review_output_schema();
    }

    agent_response_output_schema(required_policy)
}

/// Returns the prompt-facing schema selected for one protocol request profile.
pub(crate) fn protocol_json_schema_json(profile: super::model::ProtocolRequestProfile) -> String {
    if matches!(profile, super::model::ProtocolRequestProfile::FocusedReview) {
        return focused_review_json_schema_json();
    }

    agent_response_json_schema_json()
}

/// Returns the self-descriptive focused-review JSON Schema.
fn focused_review_json_schema() -> Value {
    let schema = schemars::schema_for!(FocusedReview);
    let mut schema_value = serde_json::to_value(schema).unwrap_or(Value::Null);

    inject_additional_properties_false(&mut schema_value);

    schema_value
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
    inject_additional_properties_false(&mut schema_value);
    inject_minimum_required_protocol_key(&mut schema_value);

    schema_value
}

/// Injects dynamic prompt guidance that depends on runtime constants into the
/// schema metadata shown to providers.
fn inject_dynamic_schema_guidance(schema: &mut Value) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };

    for (property_name, description) in [
        ("questions", questions_field_description()),
        ("subtasks", subtasks_field_description()),
    ] {
        let Some(property) = properties
            .get_mut(property_name)
            .and_then(Value::as_object_mut)
        else {
            continue;
        };

        property.insert("description".to_string(), Value::String(description));
    }
}

/// Recursively injects `additionalProperties: false` into every schema object
/// that declares `properties` and does not already set `additionalProperties`.
///
/// Most wire-format structs omit `#[serde(deny_unknown_fields)]` so their
/// deserialization tolerates extra fields that LLM providers sometimes add.
/// Focused-review structs are stricter because their dedicated parser must
/// match the direct transport schema. This function restores the
/// `additionalProperties: false` constraint elsewhere so prompt-level guidance
/// still tells models not to add extra fields. Pre-existing
/// `additionalProperties` values (e.g. on map-like schema fields) are
/// preserved.
fn inject_additional_properties_false(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.contains_key("properties") && !object.contains_key("additionalProperties") {
                object.insert("additionalProperties".to_string(), Value::Bool(false));
            }

            for nested_value in object.values_mut() {
                inject_additional_properties_false(nested_value);
            }
        }
        Value::Array(array) => {
            for nested_value in array {
                inject_additional_properties_false(nested_value);
            }
        }
        _ => {}
    }
}

/// Ensures the top-level `required` array includes `answer` so the prompt
/// schema rejects `{}` the same way the parser does.
///
/// The parser requires at least one recognized protocol key. The schema is
/// intentionally stricter: it
/// requires `answer` specifically, because models should always include it.
/// If a model omits `answer` but includes another recognized key, the
/// parser still accepts the payload gracefully thanks to `#[serde(default)]`.
fn inject_minimum_required_protocol_key(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };

    let required = object
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));

    let Some(required_array) = required.as_array_mut() else {
        return;
    };

    let already_listed = required_array
        .iter()
        .any(|value| value.as_str() == Some("answer"));

    if !already_listed {
        required_array.push(Value::String("answer".to_string()));
    }
}

/// Normalizes one schema tree for transport-level provider compatibility.
///
/// Claude rejects schemas with a top-level `$schema` URI when its validator
/// cannot resolve that meta-schema. Codex rejects schemas that use `oneOf` for
/// enum-like constants. Schemars can emit both shapes, so this normalizer
/// strips transport-only metadata and rewrites enum fragments to string `enum`
/// definitions. `required_policy` decides whether optional properties are also
/// forced into `required`.
fn normalize_schema_for_transport(value: &mut Value, required_policy: SchemaRequiredPolicy) {
    match value {
        Value::Object(object) => {
            object.remove("$schema");

            for nested_value in object.values_mut() {
                normalize_schema_for_transport(nested_value, required_policy);
            }

            normalize_ref_object_for_codex(object);
            if required_policy == SchemaRequiredPolicy::AllProperties {
                normalize_required_for_codex(object);
            }

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
                normalize_schema_for_transport(nested_value, required_policy);
            }
        }
        _ => {}
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

/// Pretty-prints one schema document for prompt or transport wiring.
fn stringify_schema_json(schema: &Value) -> String {
    serde_json::to_string_pretty(schema).unwrap_or("null".to_string())
}

#[cfg(test)]
#[path = "schema_test.rs"]
mod tests;
