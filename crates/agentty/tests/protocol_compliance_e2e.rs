use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agentty::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use agentty::infra::app_server_router::RoutingAppServerClient;
use agentty::infra::channel::{
    AgentChannel, AgentRequestKind, StartSessionRequest, TurnRequest, create_agent_channel,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Prompt used for live provider protocol-compliance validation.
const PROTOCOL_COMPLIANCE_PROMPT: &str = concat!(
    "Do not call tools, run commands, or edit files. ",
    "Reply in the structured protocol JSON format only. ",
    "Include at least one `answer` message with concise text."
);

/// Maximum duration allowed for one session start or turn execution.
const TURN_TIMEOUT: Duration = Duration::from_secs(90);

/// Maximum duration allowed for session shutdown cleanup.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum duration allowed for provider CLI readiness preflight checks.
const PROVIDER_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);

/// Verifies real Codex (`gpt-5.3-codex`) turn execution through
/// `create_agent_channel()` yields a non-empty protocol `answer`.
#[tokio::test]
#[ignore = "requires real Codex CLI credentials and network"]
async fn codex_protocol_compliance_e2e() {
    // Arrange
    let model = AgentModel::Gpt53Codex;
    if provider_preflight_skip_reason(AgentKind::Codex)
        .await
        .is_some()
    {
        return;
    }

    // Act
    let result = assert_provider_protocol_compliance(AgentKind::Codex, model).await;

    // Assert
    if let Err(error) = result {
        if is_skippable_provider_environment_failure(&error) {
            return;
        }
        assert!(error.is_empty(), "{error}");
    }
}

/// Verifies real Gemini Flash (`gemini-3-flash-preview`) turn execution
/// through `create_agent_channel()` yields a non-empty protocol `answer`.
#[tokio::test]
#[ignore = "requires real Gemini CLI credentials and network"]
async fn gemini_flash_protocol_compliance_e2e() {
    // Arrange
    let model = AgentModel::Gemini3FlashPreview;
    if provider_preflight_skip_reason(AgentKind::Gemini)
        .await
        .is_some()
    {
        return;
    }

    // Act
    let result = assert_provider_protocol_compliance(AgentKind::Gemini, model).await;

    // Assert
    if let Err(error) = result {
        if is_skippable_provider_environment_failure(&error) {
            return;
        }
        assert!(error.is_empty(), "{error}");
    }
}

/// Verifies real Claude Sonnet (`claude-sonnet-4-6`) turn execution through
/// `create_agent_channel()` yields a non-empty protocol `answer`.
#[tokio::test]
#[ignore = "requires real Claude CLI credentials and network"]
async fn claude_sonnet_protocol_compliance_e2e() {
    // Arrange
    let model = AgentModel::ClaudeSonnet46;
    if provider_preflight_skip_reason(AgentKind::Claude)
        .await
        .is_some()
    {
        return;
    }

    // Act
    let result = assert_provider_protocol_compliance(AgentKind::Claude, model).await;

    // Assert
    if let Err(error) = result {
        if is_skippable_provider_environment_failure(&error) {
            return;
        }
        assert!(error.is_empty(), "{error}");
    }
}

/// Returns a skip reason when the provider CLI is unavailable or unhealthy.
async fn provider_preflight_skip_reason(kind: AgentKind) -> Option<String> {
    let (provider_name, executable_name) = match kind {
        AgentKind::Codex => ("Codex", "codex"),
        AgentKind::Gemini => ("Gemini", "gemini"),
        AgentKind::Claude => ("Claude", "claude"),
    };
    let mut command = tokio::process::Command::new(executable_name);
    command.arg("--version");

    let output = match timeout(PROVIDER_PREFLIGHT_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Some(format!(
                "{provider_name} CLI preflight failed running `{executable_name} --version`: \
                 {error}"
            ));
        }
        Err(_) => {
            return Some(format!(
                "{provider_name} CLI preflight timed out after {} seconds running \
                 `{executable_name} --version`",
                PROVIDER_PREFLIGHT_TIMEOUT.as_secs()
            ));
        }
    };
    if output.status.success() {
        return None;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    Some(format!(
        "{provider_name} CLI preflight failed with exit code {:?}: {}",
        output.status.code(),
        stderr.trim()
    ))
}

/// Runs one real provider turn through the channel layer and validates that
/// the final assistant payload contains a non-empty `answer` message.
async fn assert_provider_protocol_compliance(
    kind: AgentKind,
    model: AgentModel,
) -> Result<(), String> {
    let folder = resolve_workspace_folder()?;
    let session_id = format!("protocol-e2e-{}", model.as_str());
    let app_server_client = Arc::new(RoutingAppServerClient::new());
    let channel = create_agent_channel(kind, Some(app_server_client));
    let start_request = StartSessionRequest {
        folder: folder.clone(),
        session_id: session_id.clone(),
    };
    let session_ref = timeout(TURN_TIMEOUT, channel.start_session(start_request))
        .await
        .map_err(|_| {
            format!(
                "timed out after {} seconds while starting `{}` session",
                TURN_TIMEOUT.as_secs(),
                model.as_str()
            )
        })?
        .map_err(|error| format!("failed to start `{}` session: {}", model.as_str(), error))?;
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let turn_request = build_turn_request(folder, model);
    let run_result = timeout(
        TURN_TIMEOUT,
        channel.run_turn(session_ref.session_id.clone(), turn_request, events_tx),
    )
    .await;

    let turn_result = match run_result {
        Ok(Ok(turn_result)) => turn_result,
        Ok(Err(error)) => {
            let _ = shutdown_channel_session(&channel, &session_ref.session_id).await;

            return Err(format!(
                "channel turn failed for `{}`: {}",
                model.as_str(),
                error
            ));
        }
        Err(_) => {
            let _ = shutdown_channel_session(&channel, &session_ref.session_id).await;

            return Err(format!(
                "channel turn timed out after {} seconds for `{}`",
                TURN_TIMEOUT.as_secs(),
                model.as_str()
            ));
        }
    };
    let has_non_empty_answer = turn_result
        .assistant_message
        .answers()
        .iter()
        .any(|answer| !answer.trim().is_empty());
    if !has_non_empty_answer {
        let _ = shutdown_channel_session(&channel, &session_ref.session_id).await;

        return Err(format!(
            "assistant response has no non-empty `answer` message for `{}`\ncontext_reset: \
             {}\ninput_tokens: {}\noutput_tokens: {}\ndisplay_text:\n{}",
            model.as_str(),
            turn_result.context_reset,
            turn_result.input_tokens,
            turn_result.output_tokens,
            turn_result.assistant_message.to_display_text()
        ));
    }

    shutdown_channel_session(&channel, &session_ref.session_id).await
}

/// Returns whether a failed live-provider run should be treated as an
/// environment skip instead of a protocol regression.
fn is_skippable_provider_environment_failure(error: &str) -> bool {
    error.contains("EPERM: operation not permitted")
        || error.contains("Lock acquisition failed")
        || error.contains("Timed out waiting for app-server response `init-")
        || error.contains("Failed to authenticate")
        || error.contains("authentication_error")
        || error.contains("OAuth token has expired")
}

/// Resolves the workspace folder used to execute real provider turns.
fn resolve_workspace_folder() -> Result<PathBuf, String> {
    std::env::current_dir()
        .map_err(|error| format!("failed to resolve current workspace directory: {error}"))
}

/// Builds one standard turn request for protocol-compliance validation.
fn build_turn_request(folder: PathBuf, model: AgentModel) -> TurnRequest {
    TurnRequest {
        folder,
        live_session_output: None,
        model: model.as_str().to_string(),
        request_kind: AgentRequestKind::SessionStart,
        prompt: PROTOCOL_COMPLIANCE_PROMPT.to_string().into(),
        provider_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
    }
}

/// Shuts down one channel session with timeout protection.
async fn shutdown_channel_session(
    channel: &Arc<dyn AgentChannel>,
    session_id: &str,
) -> Result<(), String> {
    timeout(
        SHUTDOWN_TIMEOUT,
        channel.shutdown_session(session_id.to_string()),
    )
    .await
    .map_err(|_| {
        format!(
            "timed out after {} seconds while shutting down session `{session_id}`",
            SHUTDOWN_TIMEOUT.as_secs()
        )
    })?
    .map_err(|error| format!("failed to shut down session `{session_id}`: {error}"))
}
