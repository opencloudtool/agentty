use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Number, Value, json};

use crate::{model, schema_contract};

const READ_DESCRIPTION: &str = concat!(
    "Inspect the repository with one bounded read-only action. Use `file` with `path` and ",
    "optional `offset`/`limit` for worktree text; `list` with optional `path`/`limit`; ",
    "`search` with `query` and optional `path`/`limit`; `diff` with optional `path` for ",
    "changes from `main`; or `show` with `path`, `side` (`base` for `main` or `head`), ",
    "and optional `offset`/`limit`."
);
const READ_NAME: &str = "read";
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const WRITE_DESCRIPTION: &str = concat!(
    "Apply one unified diff to one repository-relative text file. To create an empty file, use ",
    "only `--- /dev/null` and `+++ b/<path>` headers."
);
const WRITE_NAME: &str = "write";

/// Built-in tool that can be enabled for a harness run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tool {
    /// Read-only repository inspection.
    Read,
    /// Repository-relative patch writes.
    Write,
}

/// Provider-neutral definition of a native model tool.
///
/// Definitions describe only the wire contract advertised to a model. They do
/// not execute tools or access the filesystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    description: &'static str,
    name: &'static str,
    parameters: Value,
}

impl ToolDefinition {
    /// Defines the native `read` function tool.
    pub fn read() -> Self {
        Self {
            description: READ_DESCRIPTION,
            name: READ_NAME,
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["file", "list", "search", "diff", "show"]
                    },
                    "path": nullable_schema(repository_path_schema()),
                    "query": {
                        "type": ["string", "null"],
                        "minLength": 1,
                        "maxLength": MAX_QUERY_BYTES,
                        "pattern": "^[^\\u0000]*$"
                    },
                    "side": {
                        "type": ["string", "null"],
                        "enum": ["base", "head", null]
                    },
                    "offset": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": u64::MAX
                    },
                    "limit": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": u64::MAX
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    /// Defines the native `write` function tool.
    pub fn write() -> Self {
        Self {
            description: WRITE_DESCRIPTION,
            name: WRITE_NAME,
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": repository_path_schema(),
                    "patch": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_PATCH_BYTES
                    }
                },
                "required": ["path", "patch"],
                "additionalProperties": false
            }),
        }
    }

    /// Returns the description sent with the native function definition.
    pub fn description(&self) -> &'static str {
        self.description
    }

    /// Returns the native function name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the JSON Schema for the native function arguments.
    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
}

fn repository_path_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_PATH_BYTES,
        "pattern": "^(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+)(?:/(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+))*$",
        "not": {
            "type": "string",
            "pattern": "(^|/)\\.[gG][iI][tT](/|$)"
        }
    })
}

fn nullable_schema(mut schema: Value) -> Value {
    schema["type"] = json!(["string", "null"]);

    schema
}

/// Provider-neutral model request for one native tool invocation.
#[derive(Clone, Eq, PartialEq)]
pub struct ToolCall {
    arguments: ToolArguments,
    id: String,
    reasoning_content: Option<String>,
}

impl ToolCall {
    /// Decodes a built-in tool call returned by a model adapter.
    ///
    /// The call identifier must contain non-whitespace text and be at most
    /// 1,024 UTF-8 bytes. Accepted identifiers are preserved verbatim.
    /// Arguments and optional provider reasoning are bounded before decoding.
    /// Structurally valid read-action mistakes are retained for corrective
    /// tool feedback. The harness separately enforces tool permissions,
    /// repository containment, batch identifiers, and execution limits.
    ///
    /// # Errors
    /// Returns [`crate::ModelError`] for an invalid call identifier,
    /// unsupported tool name, oversized content, or invalid JSON arguments,
    /// including unsafe repository paths.
    pub fn from_json(
        id: String,
        name: &str,
        arguments: &str,
        reasoning_content: Option<String>,
    ) -> Result<Self, model::ModelError> {
        if id.len() > MAX_TOOL_CALL_ID_BYTES || id.trim().is_empty() {
            return Err(model::ModelError::InvalidToolCallId);
        }

        schema_contract::ensure_content_size(arguments).map_err(model::ModelError::from)?;
        if let Some(reasoning_content) = &reasoning_content {
            schema_contract::ensure_content_size(reasoning_content)
                .map_err(model::ModelError::from)?;
        }
        let arguments = match name {
            READ_NAME => serde_json::from_str(arguments).map(ToolArguments::Read),
            WRITE_NAME => serde_json::from_str(arguments).map(ToolArguments::Write),
            _ => {
                return Err(model::ModelError::UnsupportedToolName {
                    name: schema_contract::bounded_diagnostic(name),
                });
            }
        }
        .map_err(|error| model::ModelError::InvalidToolArguments {
            reason: schema_contract::bounded_diagnostic(error),
        })?;

        Ok(Self {
            arguments,
            id,
            reasoning_content,
        })
    }

    /// Returns the typed arguments for this native tool call.
    pub fn arguments(&self) -> ToolCallArguments<'_> {
        match &self.arguments {
            ToolArguments::Read(arguments) => ToolCallArguments::Read(arguments),
            ToolArguments::Write(arguments) => ToolCallArguments::Write(arguments),
        }
    }

    /// Returns typed `read` arguments when this is a `read` call.
    pub fn read_arguments(&self) -> Option<&ReadArguments> {
        match &self.arguments {
            ToolArguments::Read(arguments) => Some(arguments),
            ToolArguments::Write(_) => None,
        }
    }

    /// Returns typed `write` arguments when this is a `write` call.
    pub fn write_arguments(&self) -> Option<&WriteArguments> {
        match &self.arguments {
            ToolArguments::Read(_) => None,
            ToolArguments::Write(arguments) => Some(arguments),
        }
    }

    /// Returns the provider-assigned call identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the requested native function name.
    pub fn name(&self) -> &'static str {
        match self.arguments {
            ToolArguments::Read(_) => READ_NAME,
            ToolArguments::Write(_) => WRITE_NAME,
        }
    }

    /// Serializes the validated arguments for replay to a provider.
    ///
    /// # Errors
    /// Returns an error if the argument values cannot be serialized as JSON.
    pub fn arguments_json(&self) -> Result<String, serde_json::Error> {
        match &self.arguments {
            ToolArguments::Read(arguments) => serde_json::to_string(arguments),
            ToolArguments::Write(arguments) => serde_json::to_string(arguments),
        }
    }

    /// Returns optional provider reasoning needed to replay the assistant
    /// tool call. This sensitive content is redacted from debug output.
    pub fn reasoning_content(&self) -> Option<&str> {
        self.reasoning_content.as_deref()
    }

    pub(crate) fn read(
        id: String,
        arguments: ReadArguments,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            arguments: ToolArguments::Read(arguments),
            id,
            reasoning_content,
        }
    }

    pub(crate) fn write(
        id: String,
        arguments: WriteArguments,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            arguments: ToolArguments::Write(arguments),
            id,
            reasoning_content,
        }
    }
}

impl fmt::Debug for ToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCall")
            .field("arguments", &self.arguments)
            .field("id", &self.id)
            .field("name", &self.name())
            .field(
                "reasoning_content",
                &self.reasoning_content.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ToolArguments {
    Read(ReadArguments),
    Write(WriteArguments),
}

/// Borrowed typed arguments for one native tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallArguments<'a> {
    /// Arguments for a repository read.
    Read(&'a ReadArguments),
    /// Arguments for a repository patch write.
    Write(&'a WriteArguments),
}

/// Structurally validated arguments for one native read-only repository action.
///
/// Omitting `action` preserves the original file-read contract. The other
/// fields are action-specific. Schema-valid field combinations rejected by an
/// action are retained so the harness can return corrective tool feedback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadArguments {
    #[serde(default, skip_serializing_if = "is_default")]
    action: ReadAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<NonZeroU64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<NonZeroU64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReadSide>,
    #[serde(skip)]
    validation_error: Option<&'static str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgumentsWire {
    #[serde(default)]
    action: ReadAction,
    #[serde(default, deserialize_with = "deserialize_optional_positive_integer")]
    limit: Option<NonZeroU64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_integer")]
    offset: Option<NonZeroU64>,
    #[serde(default, deserialize_with = "deserialize_optional_repository_path")]
    path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_query")]
    query: Option<String>,
    side: Option<ReadSide>,
}

impl<'de> Deserialize<'de> for ReadArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let arguments = ReadArgumentsWire::deserialize(deserializer)?;
        let validation_error = match arguments.action {
            ReadAction::Diff
                if arguments.limit.is_none()
                    && arguments.offset.is_none()
                    && arguments.query.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::File
                if arguments.path.is_some()
                    && arguments.query.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::List
                if arguments.offset.is_none()
                    && arguments.query.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::Search
                if arguments.query.is_some()
                    && arguments.offset.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::Show
                if arguments.path.is_some()
                    && arguments.side.is_some()
                    && arguments.query.is_none() =>
            {
                None
            }
            ReadAction::Diff => Some("diff accepts only an optional path"),
            ReadAction::File => Some("file requires a path and accepts only offset and limit"),
            ReadAction::List => Some("list accepts only an optional path and limit"),
            ReadAction::Search => {
                Some("search requires a query and accepts only an optional path and limit")
            }
            ReadAction::Show => {
                Some("show requires a path and side and accepts only offset and limit")
            }
        };

        Ok(Self {
            action: arguments.action,
            limit: arguments.limit,
            offset: arguments.offset,
            path: arguments.path,
            query: arguments.query,
            side: arguments.side,
            validation_error,
        })
    }
}

impl ReadArguments {
    /// Returns the selected repository inspection action.
    pub fn action(&self) -> ReadAction {
        self.action
    }

    /// Returns the optional positive maximum line count.
    pub fn limit(&self) -> Option<u64> {
        self.limit.map(NonZeroU64::get)
    }

    /// Returns the optional one-based starting line.
    pub fn offset(&self) -> Option<u64> {
        self.offset.map(NonZeroU64::get)
    }

    /// Returns the repository-relative path to read.
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("")
    }

    /// Returns an optional path filter for actions that do not require a path.
    pub fn path_filter(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Returns the literal search query for a `search` action.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Returns the selected revision side for a `show` action.
    pub fn side(&self) -> Option<ReadSide> {
        self.side
    }

    pub(crate) fn validation_error(&self) -> Option<&'static str> {
        self.validation_error
    }
}

/// Read-only operation selected within the built-in `read` tool.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadAction {
    /// Host-bound review diff.
    Diff,
    /// Current worktree file content.
    #[default]
    File,
    /// Repository path discovery.
    List,
    /// Literal repository text search.
    Search,
    /// File content from the base or `HEAD` revision.
    Show,
}

impl ReadAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::File => "file",
            Self::List => "list",
            Self::Search => "search",
            Self::Show => "show",
        }
    }
}

/// Revision side available to the `show` read action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSide {
    /// Built-in `main` review base.
    Base,
    /// Current `HEAD` commit.
    Head,
}

/// Validated arguments for the native `write` function.
///
/// `path` names exactly one repository-relative text file and `patch` is a
/// standard unified diff that creates or updates that same file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteArguments {
    #[serde(deserialize_with = "deserialize_bounded_patch")]
    patch: String,
    #[serde(deserialize_with = "deserialize_repository_path")]
    path: String,
}

impl WriteArguments {
    /// Returns the unified diff supplied by the model.
    pub fn patch(&self) -> &str {
        &self.patch
    }

    /// Returns the repository-relative path to write.
    pub fn path(&self) -> &str {
        &self.path
    }
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

fn deserialize_bounded_patch<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let patch = String::deserialize(deserializer)?;
    if patch.is_empty() {
        return Err(de::Error::custom("patch must not be empty"));
    }
    if patch.len() > MAX_PATCH_BYTES {
        return Err(de::Error::custom("patch exceeds the byte limit"));
    }

    Ok(patch)
}

fn deserialize_optional_positive_integer<'de, D>(
    deserializer: D,
) -> Result<Option<NonZeroU64>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(number) = Option::<Number>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_positive_json_integer(&number.to_string())
        .and_then(NonZeroU64::new)
        .map(Some)
        .ok_or_else(|| de::Error::custom("number must be an integer from 1 through u64::MAX"))
}

fn deserialize_optional_repository_path<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(validate_repository_path)
        .transpose()
        .map_err(de::Error::custom)
}

fn deserialize_optional_query<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(validate_query)
        .transpose()
        .map_err(de::Error::custom)
}

fn validate_query(query: String) -> Result<String, &'static str> {
    if query.is_empty() {
        return Err("query must not be empty");
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err("query exceeds the byte limit");
    }
    if query.contains('\0') {
        return Err("query must not contain NUL");
    }

    Ok(query)
}

fn parse_positive_json_integer(number: &str) -> Option<u64> {
    let (mantissa, exponent) = match number.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (number, 0),
    };
    if mantissa.starts_with('-') {
        return None;
    }
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole);
    digits.push_str(fraction);
    let scale = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    let appended_zeros = if scale < 0 {
        let removed_digits = usize::try_from(scale.unsigned_abs()).ok()?;
        if removed_digits >= digits.len()
            || !digits[digits.len() - removed_digits..]
                .bytes()
                .all(|digit| digit == b'0')
        {
            return None;
        }
        digits.truncate(digits.len() - removed_digits);

        0
    } else {
        usize::try_from(scale).ok()?
    };
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() || digits.len().checked_add(appended_zeros)? > 20 {
        return None;
    }
    let value = digits.parse::<u64>().ok()?;

    (0..appended_zeros).try_fold(value, |value, _| value.checked_mul(10))
}

fn deserialize_repository_path<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let path = String::deserialize(deserializer)?;
    validate_repository_path(path).map_err(de::Error::custom)
}

fn validate_repository_path(path: String) -> Result<String, &'static str> {
    if path.is_empty() {
        return Err("path must not be empty");
    }
    if path.len() > MAX_PATH_BYTES {
        return Err("path exceeds the byte limit");
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err("path must be repository-relative");
    }
    if path.contains('\0') {
        return Err("path must not contain NUL");
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(
            "path must not contain empty, current-directory, or parent-directory components",
        );
    }
    if path
        .split('/')
        .any(|component| component.eq_ignore_ascii_case(".git"))
    {
        return Err("path must not access Git administrative state");
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use jsonschema::Validator;
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_call_from_json_rejects_blank_and_oversized_identifiers() {
        // Arrange
        let identifiers = [
            String::new(),
            " \t\n".to_string(),
            "x".repeat(MAX_TOOL_CALL_ID_BYTES + 1),
            "é".repeat(MAX_TOOL_CALL_ID_BYTES / 2 + 1),
        ];

        // Act
        let results =
            identifiers.map(|id| ToolCall::from_json(id, "read", r#"{"path":"name.txt"}"#, None));

        // Assert
        for result in results {
            let error = result.expect_err("invalid identifier must be rejected");
            assert!(matches!(error, model::ModelError::InvalidToolCallId));
            assert_eq!(error.error_type(), model::ModelErrorType::InvalidToolCall);
            assert_eq!(error.http_status(), None);
            assert_eq!(
                error.to_string(),
                "model returned a blank or oversized tool call identifier"
            );
        }
    }

    #[test]
    fn tool_call_from_json_preserves_identifiers_within_the_byte_limit() {
        // Arrange
        let identifiers = [
            "x".to_string(),
            " provider-id ".to_string(),
            "x".repeat(MAX_TOOL_CALL_ID_BYTES),
            "é".repeat(MAX_TOOL_CALL_ID_BYTES / 2),
        ];

        // Act
        let calls = identifiers.clone().map(|id| {
            ToolCall::from_json(id, "read", r#"{"path":"name.txt"}"#, None)
                .expect("valid identifier must be accepted")
        });

        // Assert
        for (call, expected_id) in calls.iter().zip(identifiers) {
            assert_eq!(call.id(), expected_id);
        }
    }

    #[test]
    fn tool_call_from_json_preserves_read_and_write_payloads() {
        // Arrange
        let inputs = [
            (
                "read",
                r#"{"path":"Cargo.toml"}"#,
                Some("reasoning".to_string()),
            ),
            ("write", r#"{"path":"name.txt","patch":"patch"}"#, None),
        ];

        // Act
        let calls = inputs.clone().map(|(name, arguments, reasoning)| {
            ToolCall::from_json("call-id".to_string(), name, arguments, reasoning)
                .expect("valid built-in call should decode")
        });

        // Assert
        for (call, (name, arguments, reasoning)) in calls.iter().zip(inputs) {
            assert_eq!(call.id(), "call-id");
            assert_eq!(call.name(), name);
            assert_eq!(call.reasoning_content(), reasoning.as_deref());
            assert_eq!(
                serde_json::from_str::<Value>(&call.arguments_json().expect("arguments encode"))
                    .expect("encoded arguments are JSON"),
                serde_json::from_str::<Value>(arguments).expect("fixture is JSON")
            );
        }
    }

    #[test]
    fn tool_call_from_json_rejects_unsupported_names_and_invalid_arguments() {
        // Arrange
        let inputs = [
            ("bash", "{}"),
            ("read", "{"),
            ("read", r#"{"path":"../secret"}"#),
            ("read", r#"{"path":"name.txt","limit":0}"#),
            ("write", r#"{"path":"name.txt","patch":""}"#),
            (
                "write",
                r#"{"path":"name.txt","patch":"patch","extra":true}"#,
            ),
        ];

        // Act
        let results = inputs.map(|(name, arguments)| {
            ToolCall::from_json("call-id".to_string(), name, arguments, None)
        });

        // Assert
        assert!(matches!(
            results[0],
            Err(model::ModelError::UnsupportedToolName { .. })
        ));
        assert!(
            results[1..].iter().all(|result| matches!(
                result,
                Err(model::ModelError::InvalidToolArguments { .. })
            ))
        );
    }

    #[test]
    fn tool_call_from_json_bounds_arguments_and_reasoning() {
        // Arrange
        let oversized = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
        let inputs = [
            (oversized.as_str(), None),
            (r#"{"path":"name.txt"}"#, Some(oversized.clone())),
        ];

        // Act
        let results = inputs.map(|(arguments, reasoning)| {
            ToolCall::from_json("call-id".to_string(), "read", arguments, reasoning)
        });

        // Assert
        assert!(
            results
                .iter()
                .all(|result| matches!(result, Err(model::ModelError::ResponseContentTooLarge)))
        );
    }

    #[test]
    fn tool_call_from_json_retains_correctable_read_action_errors() {
        // Arrange
        let arguments = r#"{"action":"search"}"#;

        // Act
        let call = ToolCall::from_json("call-id".to_string(), "read", arguments, None)
            .expect("structurally valid action should reach tool feedback");

        // Assert
        assert_eq!(
            call.read_arguments()
                .and_then(ReadArguments::validation_error),
            Some("search requires a query and accepts only an optional path and limit")
        );
    }

    #[test]
    fn read_definition_exposes_native_function_contract() {
        // Arrange and Act
        let definition = ToolDefinition::read();
        let validator =
            Validator::new(definition.parameters()).expect("read argument schema should compile");

        // Assert
        assert_eq!(definition.name(), "read");
        assert_eq!(definition.description(), READ_DESCRIPTION);
        assert!(validator.is_valid(&json!({ "path": "Cargo.toml" })));
        assert!(validator.is_valid(&json!({ "action": "file", "path": "Cargo.toml" })));
        assert!(validator.is_valid(&json!({
            "action": "file",
            "path": "crates/ag-harness/src/lib.rs",
            "offset": 1,
            "limit": 12
        })));
        assert!(validator.is_valid(&json!({
            "action": "file",
            "path": "Cargo.toml",
            "offset": u64::MAX,
            "limit": u64::MAX
        })));
        assert!(validator.is_valid(&json!({
            "action": "file",
            "path": "Cargo.toml",
            "offset": 1.0,
            "limit": 1e0
        })));
        assert!(validator.is_valid(&json!({
            "action": "diff",
            "path": null,
            "query": null,
            "side": null,
            "offset": null,
            "limit": null
        })));
    }

    #[test]
    fn read_definition_rejects_invalid_arguments() {
        // Arrange
        let definition = ToolDefinition::read();
        let validator =
            Validator::new(definition.parameters()).expect("read argument schema should compile");
        let offset_above_maximum = serde_json::from_str(
            r#"{"action":"file","path":"Cargo.toml","offset":18446744073709551616}"#,
        )
        .expect("out-of-range offset fixture should be valid JSON");
        let limit_above_maximum = serde_json::from_str(
            r#"{"action":"file","path":"Cargo.toml","limit":18446744073709551616}"#,
        )
        .expect("out-of-range limit fixture should be valid JSON");
        let invalid_arguments = [
            json!({ "action": "file", "path": "" }),
            json!({ "action": "file", "path": "/Cargo.toml" }),
            json!({ "action": "file", "path": "C:\\Cargo.toml" }),
            json!({ "action": "file", "path": "../Cargo.toml" }),
            json!({ "action": "file", "path": ".git/config" }),
            json!({ "action": "file", "path": "nested/.GIT/index" }),
            json!({ "action": "file", "path": "Cargo\0.toml" }),
            json!({ "action": "search", "query": "needle\0suffix" }),
            json!({ "action": "file", "path": "Cargo.toml", "offset": 0 }),
            json!({ "action": "file", "path": "Cargo.toml", "limit": 0 }),
            json!({ "action": "file", "path": "Cargo.toml", "unexpected": true }),
            offset_above_maximum,
            limit_above_maximum,
        ];

        // Act
        let results = invalid_arguments.map(|arguments| validator.is_valid(&arguments));

        // Assert
        assert!(results.into_iter().all(|is_valid| !is_valid));
    }

    #[test]
    fn read_arguments_decode_every_closed_inspection_action() {
        // Arrange
        let values = [
            json!({ "action": "file", "path": "src/lib.rs" }),
            json!({ "action": "list", "path": "src", "limit": 10 }),
            json!({ "action": "search", "query": "Harness", "limit": 5 }),
            json!({ "action": "diff" }),
            json!({
                "action": "show",
                "side": "base",
                "path": "src/lib.rs",
                "offset": 2
            }),
        ];

        // Act
        let arguments = values.map(|value| {
            serde_json::from_value::<ReadArguments>(value)
                .expect("closed read action should decode")
        });

        // Assert
        assert_eq!(arguments[0].action(), ReadAction::File);
        assert_eq!(arguments[1].action(), ReadAction::List);
        assert_eq!(arguments[2].action(), ReadAction::Search);
        assert_eq!(arguments[3].action(), ReadAction::Diff);
        assert_eq!(arguments[4].action(), ReadAction::Show);
        assert_eq!(arguments[2].query(), Some("Harness"));
        assert_eq!(arguments[0].query(), None);
        assert_eq!(arguments[3].path_filter(), None);
        assert_eq!(arguments[4].side(), Some(ReadSide::Base));
        assert_eq!(arguments[0].side(), None);
        assert!(
            arguments
                .iter()
                .all(|arguments| arguments.validation_error().is_none())
        );
    }

    #[test]
    fn read_actions_have_stable_names() {
        // Arrange
        let actions = [
            ReadAction::Diff,
            ReadAction::File,
            ReadAction::List,
            ReadAction::Search,
            ReadAction::Show,
        ];

        // Act
        let names = actions.map(ReadAction::as_str);

        // Assert
        assert_eq!(names, ["diff", "file", "list", "search", "show"]);
    }

    #[test]
    fn read_arguments_retain_schema_valid_action_rejections() {
        // Arrange
        let values = [
            json!({}),
            json!({ "action": "file" }),
            json!({ "action": "list", "offset": 1 }),
            json!({ "action": "search" }),
            json!({ "action": "show", "side": "base" }),
            json!({ "action": "diff", "query": "unexpected" }),
        ];
        let definition = ToolDefinition::read();
        let validator =
            Validator::new(definition.parameters()).expect("read argument schema should compile");

        // Act
        let arguments = values
            .iter()
            .cloned()
            .map(serde_json::from_value::<ReadArguments>)
            .collect::<Result<Vec<_>, _>>()
            .expect("schema-valid arguments should decode for a correctable rejection");

        // Assert
        assert!(values.iter().all(|value| validator.is_valid(value)));
        assert!(
            arguments
                .iter()
                .all(|arguments| arguments.validation_error().is_some())
        );
    }

    #[test]
    fn read_arguments_reject_schema_invalid_input() {
        // Arrange
        let values = [
            json!({ "action": "search", "query": "" }),
            json!({ "action": "search", "query": "needle\0suffix" }),
            json!({ "action": "search", "query": "x".repeat(MAX_QUERY_BYTES + 1) }),
            json!({ "action": "show", "side": "other", "path": "src/lib.rs" }),
            json!({ "action": "unknown" }),
        ];
        let definition = ToolDefinition::read();
        let validator =
            Validator::new(definition.parameters()).expect("read argument schema should compile");

        // Act
        let errors = values
            .iter()
            .cloned()
            .map(serde_json::from_value::<ReadArguments>)
            .collect::<Vec<_>>();

        // Assert
        assert!(values.iter().all(|value| !validator.is_valid(value)));
        assert!(errors.into_iter().all(|result| result.is_err()));
    }

    #[test]
    fn write_definition_exposes_native_function_contract() {
        // Arrange and Act
        let definition = ToolDefinition::write();
        let validator =
            Validator::new(definition.parameters()).expect("write argument schema should compile");

        // Assert
        assert_eq!(definition.name(), "write");
        assert_eq!(
            definition.description(),
            concat!(
                "Apply one unified diff to one repository-relative text file. To create an empty ",
                "file, use only `--- /dev/null` and `+++ b/<path>` headers."
            )
        );
        assert!(validator.is_valid(&json!({
            "path": "src/lib.rs",
            "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
        })));
    }

    #[test]
    fn write_definition_and_arguments_reject_invalid_input() {
        // Arrange
        let definition = ToolDefinition::write();
        let validator =
            Validator::new(definition.parameters()).expect("write argument schema should compile");
        let values = [
            json!({}),
            json!({ "path": "src/lib.rs" }),
            json!({ "path": "src/lib.rs", "patch": "" }),
            json!({ "path": "../lib.rs", "patch": "patch" }),
            json!({ "path": ".git/config", "patch": "patch" }),
            json!({ "path": "nested/.GIT/index", "patch": "patch" }),
            json!({ "path": "src/lib.rs", "patch": "patch", "extra": true }),
            json!({ "path": "a".repeat(MAX_PATH_BYTES + 1), "patch": "patch" }),
            json!({ "path": "src/lib.rs", "patch": "x".repeat(MAX_PATCH_BYTES + 1) }),
        ];

        // Act
        let schema_results = values.clone().map(|value| validator.is_valid(&value));
        let decode_results = values.map(serde_json::from_value::<WriteArguments>);

        // Assert
        assert!(schema_results.into_iter().all(|valid| !valid));
        assert!(decode_results.into_iter().all(|result| result.is_err()));
    }

    #[test]
    fn tool_call_exposes_matching_typed_arguments_and_serialization() {
        // Arrange
        let read_arguments = serde_json::from_value(json!({
            "action": "file",
            "path": "Cargo.toml"
        }))
        .expect("read arguments should decode");
        let write_arguments = serde_json::from_value(json!({
            "path": "src/lib.rs",
            "patch": "patch"
        }))
        .expect("write arguments should decode");
        let read = ToolCall::read(
            "read-id".to_string(),
            read_arguments,
            Some("secret".to_string()),
        );
        let write = ToolCall::write("write-id".to_string(), write_arguments, None);

        // Act
        let read_json = read.arguments_json().expect("read arguments should encode");
        let write_json = write
            .arguments_json()
            .expect("write arguments should encode");

        // Assert
        assert!(read.read_arguments().is_some());
        assert!(read.write_arguments().is_none());
        assert!(write.read_arguments().is_none());
        assert_eq!(read.name(), "read");
        assert_eq!(write.name(), "write");
        let write_arguments = write
            .write_arguments()
            .expect("write arguments should be exposed");
        assert_eq!(write_arguments.path(), "src/lib.rs");
        assert_eq!(write_arguments.patch(), "patch");
        assert_eq!(read_json, r#"{"path":"Cargo.toml"}"#);
        assert_eq!(write_json, r#"{"patch":"patch","path":"src/lib.rs"}"#);
        assert_eq!(read.reasoning_content(), Some("secret"));
        assert!(format!("{read:?}").contains("[REDACTED]"));
        assert!(matches!(
            read.arguments(),
            ToolCallArguments::Read(arguments) if arguments.path() == "Cargo.toml"
        ));
        assert!(matches!(
            write.arguments(),
            ToolCallArguments::Write(arguments)
                if arguments.path() == "src/lib.rs" && arguments.patch() == "patch"
        ));
    }

    #[test]
    fn read_arguments_reject_invalid_repository_paths() {
        // Arrange
        let invalid_paths = [
            "",
            "/Cargo.toml",
            "C:\\Cargo.toml",
            "server\\share",
            "src//lib.rs",
            "src/./lib.rs",
            "../lib.rs",
            ".git",
            ".git/config",
            "nested/.GIT/index",
            "Cargo\0.toml",
        ];

        // Act
        let errors = invalid_paths.map(|path| {
            serde_json::from_value::<ReadArguments>(json!({ "action": "file", "path": path }))
                .expect_err("invalid path should be rejected")
        });

        // Assert
        assert!(
            errors
                .into_iter()
                .all(|error| !error.to_string().is_empty())
        );
    }

    #[test]
    fn read_arguments_treat_optional_null_fields_as_omitted() {
        // Arrange
        let values = [
            json!({ "action": "file", "path": "Cargo.toml" }),
            json!({ "action": "file", "path": "Cargo.toml", "offset": null }),
            json!({ "action": "file", "path": "Cargo.toml", "limit": null }),
            json!({
                "action": "diff",
                "path": null,
                "query": null,
                "side": null,
                "offset": null,
                "limit": null
            }),
        ];

        // Act
        let arguments = values.map(|value| {
            serde_json::from_value::<ReadArguments>(value)
                .expect("optional null fields should decode as omissions")
        });

        // Assert
        assert!(
            arguments
                .iter()
                .all(|arguments| arguments.offset().is_none())
        );
        assert!(
            arguments
                .iter()
                .all(|arguments| arguments.limit().is_none())
        );
        assert_eq!(arguments[3].action(), ReadAction::Diff);
        assert_eq!(arguments[3].path_filter(), None);
    }

    #[test]
    fn read_arguments_accept_maximum_ranges() {
        // Arrange
        let value = json!({
            "action": "file",
            "path": "Cargo.toml",
            "offset": u64::MAX,
            "limit": u64::MAX
        });

        // Act
        let arguments = serde_json::from_value::<ReadArguments>(value)
            .expect("maximum u64 ranges should decode");

        // Assert
        assert_eq!(arguments.offset(), Some(u64::MAX));
        assert_eq!(arguments.limit(), Some(u64::MAX));
    }

    #[test]
    fn read_arguments_accept_integral_decimal_and_exponent_ranges() {
        // Arrange
        let values = [
            (
                r#"{"action":"file","path":"Cargo.toml","offset":1.0,"limit":1e0}"#,
                (1, 1),
            ),
            (
                r#"{"action":"file","path":"Cargo.toml","offset":1e2,"limit":100e-2}"#,
                (100, 1),
            ),
            (
                r#"{"action":"file","path":"Cargo.toml","offset":18446744073709551615.0,"limit":18446744073709551615e0}"#,
                (u64::MAX, u64::MAX),
            ),
        ];

        // Act
        let arguments = values.map(|(value, expected)| {
            serde_json::from_str::<ReadArguments>(value)
                .map(|arguments| (arguments, expected))
                .expect("integral numeric forms should decode")
        });

        // Assert
        assert!(arguments.iter().all(|(arguments, expected)| {
            arguments.offset() == Some(expected.0) && arguments.limit() == Some(expected.1)
        }));
    }

    #[test]
    fn read_arguments_reject_non_integral_or_out_of_range_numbers() {
        // Arrange
        let values = [
            r#"{"action":"file","path":"Cargo.toml","offset":-1}"#,
            r#"{"action":"file","path":"Cargo.toml","limit":1.5}"#,
            r#"{"action":"file","path":"Cargo.toml","limit":1e-1}"#,
            r#"{"action":"file","path":"Cargo.toml","offset":18446744073709551616}"#,
            r#"{"action":"file","path":"Cargo.toml","offset":100000000000000000000}"#,
            r#"{"action":"file","path":"Cargo.toml","offset":1e999999999999999999999}"#,
        ];

        // Act
        let errors = values.map(|value| {
            serde_json::from_str::<ReadArguments>(value)
                .expect_err("non-integral or out-of-range number should fail")
        });

        // Assert
        assert!(
            errors
                .into_iter()
                .all(|error| !error.to_string().is_empty())
        );
    }
}
