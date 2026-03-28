//! Shared app-server prompt shaping helpers.

use crate::infra::agent;
use crate::infra::app_server::{AppServerError, AppServerTurnRequest};
use crate::infra::channel::{AgentRequestKind, TurnPrompt};

/// Reads the latest session output, preferring the live buffer over the
/// stale snapshot.
///
/// The live buffer (`live_session_output`) accumulates all streamed content
/// in real time, including output from a turn that failed mid-stream. When
/// available, it provides a more complete transcript than the snapshot
/// captured at turn-enqueue time.
pub(crate) fn read_latest_session_output(request: &AppServerTurnRequest) -> Option<String> {
    if let Some(live_output) = &request.live_session_output
        && let Ok(guard) = live_output.lock()
    {
        let output = guard.clone();
        if !output.trim().is_empty() {
            return Some(output);
        }
    }

    request
        .request_kind
        .session_output()
        .map(ToString::to_string)
}

/// Returns the turn prompt, applying protocol preamble and optional context
/// replay.
///
///
/// The returned prompt always includes the shared protocol preamble, which
/// carries both repo-root-relative file path guidance and structured response
/// instructions so providers see one consistent contract.
///
/// # Errors
/// Returns an error when Askama prompt rendering fails after a context reset.
pub fn turn_prompt_for_runtime(
    prompt: impl Into<TurnPrompt>,
    request_kind: &AgentRequestKind,
    replay_session_output: Option<&str>,
    context_reset: bool,
) -> Result<TurnPrompt, AppServerError> {
    let prompt = prompt.into();
    let turn_prompt = agent::prepare_prompt_text(agent::PromptPreparationRequest {
        prompt: &prompt.text,
        protocol_profile: request_kind.protocol_profile(),
        replay_session_output,
        should_replay_session_output: context_reset,
    })
    .map_err(|error| AppServerError::PromptRender(error.to_string()))?;

    Ok(TurnPrompt {
        attachments: prompt.attachments,
        text: turn_prompt,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::domain::agent::ReasoningLevel;

    #[test]
    fn read_latest_session_output_prefers_live_buffer() {
        // Arrange
        let live_output = Arc::new(Mutex::new("live content".to_string()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp/test"),
            live_session_output: Some(live_output),
            model: "test-model".to_string(),
            prompt: TurnPrompt::from("hello"),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            session_id: "test-session".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, Some("live content".to_string()));
    }

    #[test]
    fn read_latest_session_output_falls_back_when_live_buffer_is_empty() {
        // Arrange
        let live_output = Arc::new(Mutex::new("  ".to_string()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp/test"),
            live_session_output: Some(live_output),
            model: "test-model".to_string(),
            prompt: TurnPrompt::from("hello"),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            session_id: "test-session".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert!(output.is_none());
    }

    #[test]
    fn read_latest_session_output_returns_none_when_no_live_buffer() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp/test"),
            live_session_output: None,
            model: "test-model".to_string(),
            prompt: TurnPrompt::from("hello"),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            session_id: "test-session".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert!(output.is_none());
    }

    #[test]
    fn turn_prompt_for_runtime_includes_protocol_preamble() {
        // Arrange
        let prompt = TurnPrompt::from("fix the bug");
        let request_kind = AgentRequestKind::SessionStart;

        // Act
        let result = turn_prompt_for_runtime(prompt, &request_kind, None, false);

        // Assert
        let turn_prompt = result.expect("prompt rendering should succeed");
        assert!(turn_prompt.text.contains("fix the bug"));
    }
}
