use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::domain::agent::AgentKind;
use crate::domain::permission::PermissionMode;
use crate::domain::session::SessionStats;

const RESUME_WITH_SESSION_OUTPUT_PROMPT_TEMPLATE: &str =
    include_str!("../../resources/resume_with_session_output_prompt.md");

/// Parsed agent response including content text and usage statistics.
pub struct ParsedResponse {
    pub content: String,
    pub stats: SessionStats,
}

#[cfg_attr(test, mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// One-time setup in agent folder before first run (e.g. config files).
    fn setup(&self, folder: &Path);
    /// Build a Command for an initial task.
    fn build_start_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
    ) -> Command;
    /// Build a Command for resuming/replying.
    ///
    /// Implementations may intentionally start a fresh conversation when
    /// `session_output` is provided (for example, to replay history after a
    /// model switch).
    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
        session_output: Option<String>,
    ) -> Command;
}

fn build_resume_prompt(prompt: &str, session_output: Option<&str>) -> String {
    let Some(session_output) = session_output
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return prompt.to_string();
    };

    RESUME_WITH_SESSION_OUTPUT_PROMPT_TEMPLATE
        .trim_end()
        .replace("{session_output}", session_output)
        .replace("{prompt}", prompt)
}

pub struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) {
        // Gemini CLI needs no config files
    }

    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
        session_output: Option<String>,
    ) -> Command {
        let has_history_replay = session_output
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let mut cmd = self.build_start_command(
            folder,
            &prompt,
            model,
            permission_mode,
            is_initial_plan_prompt,
        );

        if !has_history_replay {
            cmd.arg("--resume").arg("latest");
        }

        cmd
    }

    fn build_start_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
    ) -> Command {
        let prompt = permission_mode.apply_to_prompt(prompt, is_initial_plan_prompt);
        let approval_mode = match permission_mode {
            PermissionMode::AutoEdit | PermissionMode::Plan => "auto_edit",
            PermissionMode::Autonomous => "yolo",
        };
        let mut cmd = Command::new("gemini");
        cmd.arg("--prompt")
            .arg(prompt.as_ref())
            .arg("--model")
            .arg(model)
            .arg("--approval-mode")
            .arg(approval_mode)
            .arg("--output-format")
            .arg("stream-json")
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }
}

pub struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn setup(&self, _folder: &Path) {
        // Claude Code needs no config files
    }

    fn build_start_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
    ) -> Command {
        let prompt = permission_mode.apply_to_prompt(prompt, is_initial_plan_prompt);
        let mut cmd = Command::new("claude");
        cmd.arg("-p").arg(prompt.as_ref());
        Self::apply_permission_args(&mut cmd, permission_mode);
        cmd.arg("--verbose")
            .arg("--output-format")
            .arg("stream-json")
            .env("ANTHROPIC_MODEL", model)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }

    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
        session_output: Option<String>,
    ) -> Command {
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let prompt = permission_mode.apply_to_prompt(&prompt, is_initial_plan_prompt);
        let mut cmd = Command::new("claude");
        cmd.arg("-c").arg("-p").arg(prompt.as_ref());
        Self::apply_permission_args(&mut cmd, permission_mode);
        cmd.arg("--verbose")
            .arg("--output-format")
            .arg("stream-json")
            .env("ANTHROPIC_MODEL", model)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }
}

impl ClaudeBackend {
    fn apply_permission_args(cmd: &mut Command, permission_mode: PermissionMode) {
        match permission_mode {
            PermissionMode::AutoEdit | PermissionMode::Plan => {
                cmd.arg("--allowedTools").arg("Edit");
            }
            PermissionMode::Autonomous => {
                cmd.arg("--dangerously-skip-permissions");
            }
        }
    }
}

/// Uses non-interactive Codex commands so Agentty can capture piped output.
///
/// Interactive `codex` requires a TTY and fails in this app with
/// `Error: stdout is not a terminal`, so we run `codex exec --full-auto`
/// and `codex exec resume --last --full-auto` instead.
pub struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) {
        // Codex CLI needs no config files
    }

    fn build_start_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
    ) -> Command {
        let prompt = permission_mode.apply_to_prompt(prompt, is_initial_plan_prompt);
        let approval_flag = Self::approval_flag(permission_mode);
        let mut cmd = Command::new("codex");
        cmd.arg("exec")
            .arg("--model")
            .arg(model)
            .arg(approval_flag)
            .arg("--json")
            .arg(prompt.as_ref())
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }

    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
        session_output: Option<String>,
    ) -> Command {
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let prompt = permission_mode.apply_to_prompt(&prompt, is_initial_plan_prompt);
        let approval_flag = Self::approval_flag(permission_mode);
        let mut cmd = Command::new("codex");
        cmd.arg("exec")
            .arg("resume")
            .arg("--last")
            .arg("--model")
            .arg(model)
            .arg(approval_flag)
            .arg("--json")
            .arg(prompt.as_ref())
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }
}

impl CodexBackend {
    fn approval_flag(permission_mode: PermissionMode) -> &'static str {
        match permission_mode {
            PermissionMode::AutoEdit | PermissionMode::Plan => "--full-auto",
            PermissionMode::Autonomous => "--yolo",
        }
    }
}

/// Claude CLI JSON response shape (`--output-format json`).
#[derive(Deserialize)]
struct ClaudeResponse {
    result: Option<String>,
    usage: Option<ClaudeUsage>,
}

/// Token usage from a Claude CLI response.
#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

/// Gemini CLI JSON response shape (`--output-format json`).
#[derive(Deserialize)]
struct GeminiResponse {
    response: Option<String>,
    stats: Option<GeminiStats>,
}

/// Gemini CLI stream event shape (`--output-format stream-json`).
#[derive(Deserialize)]
struct GeminiStreamEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    stats: Option<GeminiStreamResultStats>,
}

/// Token usage from a Gemini stream `result` event.
#[derive(Deserialize)]
struct GeminiStreamResultStats {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

/// Top-level `stats` object from the Gemini CLI JSON output.
#[derive(Deserialize)]
struct GeminiStats {
    models: Option<std::collections::HashMap<String, GeminiModelStats>>,
}

/// Per-model statistics from Gemini CLI.
#[derive(Deserialize)]
struct GeminiModelStats {
    tokens: Option<GeminiTokens>,
}

/// Token counts from a single Gemini model.
#[derive(Deserialize)]
struct GeminiTokens {
    /// Uncached prompt tokens (`max(0, prompt - cached)`).
    input: Option<i64>,
    /// Output/completion tokens generated by the model.
    candidates: Option<i64>,
}

/// Single NDJSON event emitted by Codex CLI (`--json`).
#[derive(Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    item: Option<CodexItem>,
    usage: Option<CodexUsage>,
}

/// Token usage from a Codex `turn.completed` event.
#[derive(Deserialize)]
struct CodexUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

/// Nested `item` inside a Codex event.
#[derive(Deserialize)]
struct CodexItem {
    #[serde(rename = "type")]
    item_type: Option<String>,
    text: Option<String>,
}

/// Create the corresponding backend.
pub fn create_backend(kind: AgentKind) -> Box<dyn AgentBackend> {
    match kind {
        AgentKind::Gemini => Box::new(GeminiBackend),
        AgentKind::Claude => Box::new(ClaudeBackend),
        AgentKind::Codex => Box::new(CodexBackend),
    }
}

/// Extracts the response message and usage statistics from raw agent JSON
/// output.
///
/// Each agent CLI produces a different JSON schema. This method
/// dispatches to the appropriate parser and falls back to raw text
/// when JSON parsing fails.
pub fn parse_response(kind: AgentKind, stdout: &str, stderr: &str) -> ParsedResponse {
    match kind {
        AgentKind::Claude => parse_claude_response(stdout),
        AgentKind::Gemini => parse_gemini_response(stdout),
        AgentKind::Codex => parse_codex_response(stdout),
    }
    .unwrap_or_else(|| ParsedResponse {
        content: fallback_response(stdout, stderr),
        stats: SessionStats::default(),
    })
}

/// Extracts a displayable incremental stream update from one stdout line.
///
/// The returned tuple is `(text, is_response_content)`, where
/// `is_response_content` is `true` when `text` is model-authored content
/// that should prevent duplicate final output append.
pub(crate) fn parse_stream_output_line(
    kind: AgentKind,
    stdout_line: &str,
) -> Option<(String, bool)> {
    match kind {
        AgentKind::Claude => parse_claude_stream_output_line(stdout_line),
        AgentKind::Gemini => parse_gemini_stream_output_line(stdout_line),
        AgentKind::Codex => parse_codex_stream_output_line(stdout_line),
    }
}

fn parse_claude_response(stdout: &str) -> Option<ParsedResponse> {
    let trimmed_stdout = stdout.trim();
    if let Some(parsed_response) = parse_claude_response_payload(trimmed_stdout) {
        return Some(parsed_response);
    }

    for line in stdout.lines().rev() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }
        if let Some(parsed_response) = parse_claude_response_payload(trimmed_line) {
            return Some(parsed_response);
        }
    }

    None
}

fn parse_claude_stream_output_line(stdout_line: &str) -> Option<(String, bool)> {
    let trimmed_line = stdout_line.trim();
    if trimmed_line.is_empty() {
        return None;
    }

    if let Some(content) = extract_claude_stream_result(trimmed_line) {
        return Some((content, true));
    }

    let stream_event = serde_json::from_str::<serde_json::Value>(trimmed_line).ok()?;
    let progress_message = compact_progress_message_from_json(&stream_event)?;

    Some((progress_message, false))
}

fn parse_gemini_response(stdout: &str) -> Option<ParsedResponse> {
    let trimmed_stdout = stdout.trim();
    if let Some(parsed_response) = parse_gemini_response_payload(trimmed_stdout) {
        return Some(parsed_response);
    }

    let mut latest_legacy_response: Option<ParsedResponse> = None;
    let mut stream_response = String::new();
    let mut stream_stats: Option<SessionStats> = None;

    for line in stdout.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        if let Some(parsed_response) = parse_gemini_response_payload(trimmed_line) {
            latest_legacy_response = Some(parsed_response);

            continue;
        }

        if let Some(stream_chunk) = extract_gemini_stream_response(trimmed_line) {
            stream_response.push_str(&stream_chunk);

            continue;
        }

        if let Some(parsed_stream_stats) = extract_gemini_stream_stats(trimmed_line) {
            stream_stats = Some(parsed_stream_stats);
        }
    }

    if let Some(parsed_response) = latest_legacy_response {
        return Some(parsed_response);
    }

    if stream_response.is_empty() && stream_stats.is_none() {
        return None;
    }

    let stats = stream_stats.unwrap_or_default();

    Some(ParsedResponse {
        content: stream_response,
        stats,
    })
}

fn parse_gemini_stream_output_line(stdout_line: &str) -> Option<(String, bool)> {
    let trimmed_line = stdout_line.trim();
    if trimmed_line.is_empty() {
        return None;
    }

    let stream_event = serde_json::from_str::<serde_json::Value>(trimmed_line).ok()?;

    if let Some(content) = extract_gemini_stream_response_from_json(&stream_event) {
        return Some((content, true));
    }

    let progress_message = compact_progress_message_from_json(&stream_event)?;

    Some((progress_message, false))
}

fn parse_claude_response_payload(stdout: &str) -> Option<ParsedResponse> {
    let response = serde_json::from_str::<ClaudeResponse>(stdout).ok()?;
    let content = response.result?;
    let stats = SessionStats {
        input_tokens: response
            .usage
            .as_ref()
            .and_then(|usage| usage.input_tokens)
            .unwrap_or(0)
            .cast_unsigned(),
        output_tokens: response
            .usage
            .as_ref()
            .and_then(|usage| usage.output_tokens)
            .unwrap_or(0)
            .cast_unsigned(),
    };

    Some(ParsedResponse { content, stats })
}

fn parse_gemini_response_payload(stdout: &str) -> Option<ParsedResponse> {
    let response = serde_json::from_str::<GeminiResponse>(stdout).ok()?;
    let content = response.response?;
    let stats = extract_gemini_stats(response.stats);

    Some(ParsedResponse { content, stats })
}

fn extract_claude_stream_result(stdout_line: &str) -> Option<String> {
    let response = serde_json::from_str::<ClaudeResponse>(stdout_line).ok()?;

    response.result
}

fn extract_gemini_stream_response(stdout_line: &str) -> Option<String> {
    let stream_event = serde_json::from_str::<serde_json::Value>(stdout_line).ok()?;

    extract_gemini_stream_response_from_json(&stream_event)
}

fn extract_gemini_stream_response_from_json(stream_event: &serde_json::Value) -> Option<String> {
    if let Some(legacy_response) = stream_event
        .get("response")
        .and_then(serde_json::Value::as_str)
    {
        return Some(legacy_response.to_string());
    }

    if stream_event.get("type").and_then(serde_json::Value::as_str) != Some("message") {
        return None;
    }

    if stream_event.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
        return None;
    }

    stream_event
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

fn extract_gemini_stream_stats(stdout_line: &str) -> Option<SessionStats> {
    let stream_event = serde_json::from_str::<GeminiStreamEvent>(stdout_line).ok()?;
    if stream_event.event_type.as_deref() != Some("result") {
        return None;
    }

    let stats = stream_event
        .stats
        .map_or_else(SessionStats::default, |stats| SessionStats {
            input_tokens: stats.input_tokens.unwrap_or(0).cast_unsigned(),
            output_tokens: stats.output_tokens.unwrap_or(0).cast_unsigned(),
        });

    Some(stats)
}

fn compact_progress_message_from_json(stream_event: &serde_json::Value) -> Option<String> {
    let item_type = stream_event
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(serde_json::Value::as_str);
    if let Some(progress_message) = item_type.and_then(compact_progress_message_from_stream_label) {
        return Some(progress_message);
    }

    let tool_name = stream_event
        .get("tool_name")
        .and_then(serde_json::Value::as_str);
    if let Some(progress_message) = tool_name.and_then(compact_progress_message_from_stream_label) {
        return Some(progress_message);
    }

    let name = stream_event.get("name").and_then(serde_json::Value::as_str);
    if let Some(progress_message) = name.and_then(compact_progress_message_from_stream_label) {
        return Some(progress_message);
    }

    let tool_name = stream_event
        .get("tool")
        .and_then(|tool| tool.get("name"))
        .and_then(serde_json::Value::as_str);
    if let Some(progress_message) = tool_name.and_then(compact_progress_message_from_stream_label) {
        return Some(progress_message);
    }

    let event = stream_event
        .get("event")
        .and_then(serde_json::Value::as_str);
    if let Some(progress_message) = event.and_then(compact_progress_message_from_stream_label) {
        return Some(progress_message);
    }

    let subtype = stream_event
        .get("subtype")
        .and_then(serde_json::Value::as_str);
    if let Some(progress_message) = subtype.and_then(compact_progress_message_from_stream_label) {
        return Some(progress_message);
    }

    let event_type = stream_event.get("type").and_then(serde_json::Value::as_str);
    if let Some(progress_message) = event_type.and_then(compact_progress_message_from_stream_label)
    {
        return Some(progress_message);
    }

    None
}

fn compact_progress_message_from_stream_label(label: &str) -> Option<String> {
    let normalized_label = label.to_ascii_lowercase().replace('-', "_");
    if normalized_label.contains("search") {
        return Some("Searching the web".to_string());
    }

    if normalized_label.contains("reasoning")
        || normalized_label.contains("thinking")
        || normalized_label.contains("thought")
    {
        return Some("Thinking".to_string());
    }

    if normalized_label.contains("command")
        || normalized_label.contains("bash")
        || normalized_label.contains("terminal")
        || normalized_label.contains("shell")
        || normalized_label.contains("tool_use")
        || normalized_label.contains("tool_call")
        || normalized_label.contains("toolcall")
        || normalized_label.contains("execute")
    {
        return Some("Running a command".to_string());
    }

    None
}

fn extract_gemini_stats(stats: Option<GeminiStats>) -> SessionStats {
    let Some(models) = stats.and_then(|stat| stat.models) else {
        return SessionStats::default();
    };

    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;

    for model_stats in models.values() {
        if let Some(tokens) = &model_stats.tokens {
            total_input += tokens.input.unwrap_or(0).cast_unsigned();
            total_output += tokens.candidates.unwrap_or(0).cast_unsigned();
        }
    }

    SessionStats {
        input_tokens: total_input,
        output_tokens: total_output,
    }
}

fn parse_codex_response(stdout: &str) -> Option<ParsedResponse> {
    let mut last_message: Option<String> = None;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<CodexEvent>(trimmed) else {
            continue;
        };

        if event.event_type.as_deref() == Some("turn.completed")
            && let Some(usage) = event.usage
        {
            total_input_tokens += usage.input_tokens.unwrap_or(0).cast_unsigned();
            total_output_tokens += usage.output_tokens.unwrap_or(0).cast_unsigned();
        }

        if event.event_type.as_deref() != Some("item.completed") {
            continue;
        }
        let Some(item) = event.item else {
            continue;
        };
        if item.item_type.as_deref() != Some("agent_message") {
            continue;
        }
        if let Some(text) = item.text {
            last_message = Some(text);
        }
    }

    let stats = SessionStats {
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
    };

    last_message.map(|content| ParsedResponse { content, stats })
}

fn parse_codex_stream_output_line(stdout_line: &str) -> Option<(String, bool)> {
    let trimmed = stdout_line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let event = serde_json::from_str::<CodexEvent>(trimmed).ok()?;
    if event.event_type.as_deref() == Some("item.started") {
        let item = event.item?;
        let item_type = item.item_type.as_deref()?;
        let progress_message = compact_codex_progress_message(item_type)?;

        return Some((progress_message, false));
    }

    if event.event_type.as_deref() != Some("item.completed") {
        return None;
    }

    let item = event.item?;
    if item.item_type.as_deref() != Some("agent_message") {
        return None;
    }

    let text = item.text?;

    Some((text, true))
}

fn compact_codex_progress_message(item_type: &str) -> Option<String> {
    match item_type {
        "agent_message" => None,
        "command_execution" => Some("Running a command".to_string()),
        "reasoning" => Some("Thinking".to_string()),
        "web_search" => Some("Searching the web".to_string()),
        other => Some(format!("Working: {}", other.replace('_', " "))),
    }
}

fn fallback_response(stdout: &str, stderr: &str) -> String {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return stderr.trim().to_string();
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_gemini_setup_creates_no_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        AgentBackend::setup(&backend, dir.path());

        // Assert
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }

    #[test]
    fn test_claude_plan_mode_uses_allowed_tools_edit() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_start_command(
            &backend,
            dir.path(),
            "Plan prompt",
            "claude-sonnet-4-6",
            PermissionMode::Plan,
            true,
        );
        let debug = format!("{command:?}");

        // Assert
        assert!(debug.contains("--allowedTools"));
        assert!(debug.contains("Edit"));
        assert!(!debug.contains("--permission-mode"));
    }

    #[test]
    fn test_claude_parse_response_reads_result_payload() {
        // Arrange
        let stdout =
            r#"{"result":"Planned response","usage":{"input_tokens":11,"output_tokens":7}}"#;

        // Act
        let parsed = parse_response(AgentKind::Claude, stdout, "");

        // Assert
        assert_eq!(parsed.content, "Planned response");
        assert_eq!(parsed.stats.input_tokens, 11);
        assert_eq!(parsed.stats.output_tokens, 7);
    }
}
