//! Shared app-server prompt shaping helpers.

use crate::infra::agent;
use crate::infra::agent::InstructionDeliveryMode;
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
/// replay according to the selected instruction delivery mode.
///
///
/// `BootstrapFull` and `BootstrapWithReplay` include the full shared protocol
/// preamble, while `DeltaOnly` emits only a compact reminder for provider
/// contexts that already received that contract.
///
/// # Errors
/// Returns an error when Askama prompt rendering fails after a context reset.
pub(crate) fn turn_prompt_for_runtime(
    prompt: impl Into<TurnPrompt>,
    request_kind: &AgentRequestKind,
    replay_session_output: Option<&str>,
    instruction_delivery_mode: InstructionDeliveryMode,
) -> Result<TurnPrompt, AppServerError> {
    let prompt = prompt.into();
    let agent_prompt = prompt.agent_text();
    let turn_prompt = agent::prepare_prompt_text(agent::PromptPreparationRequest {
        instruction_delivery_mode,
        prompt: &agent_prompt,
        protocol_profile: request_kind.protocol_profile(),
        replay_session_output,
    })
    .map_err(|error| AppServerError::PromptRender(error.to_string()))?;

    Ok(TurnPrompt {
        attachments: prompt.attachments,
        text: turn_prompt,
    })
}

/// Plans how one app-server turn should deliver Agentty's instruction
/// contract for the active runtime context.
pub(crate) fn instruction_delivery_mode_for_runtime(
    request: &AppServerTurnRequest,
    runtime_provider_conversation_id: Option<&str>,
    should_replay_session_output: bool,
) -> InstructionDeliveryMode {
    agent::plan_app_server_instruction_delivery(
        &request.request_kind,
        runtime_provider_conversation_id,
        request.persisted_instruction_conversation_id.as_deref(),
        should_replay_session_output,
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::domain::agent::ReasoningLevel;

    /// Returns one persisted bootstrap marker that matches the active
    /// app-server instruction contract for session turns.
    fn persisted_instruction_conversation_id_for_session_turn(
        provider_conversation_id: Option<&str>,
    ) -> Option<String> {
        agent::normalize_instruction_conversation_id(provider_conversation_id)
    }

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
            persisted_instruction_conversation_id: None,
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
            persisted_instruction_conversation_id: None,
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
            persisted_instruction_conversation_id: None,
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
        let result = turn_prompt_for_runtime(
            prompt,
            &request_kind,
            None,
            InstructionDeliveryMode::BootstrapFull,
        );

        // Assert
        let turn_prompt = result.expect("prompt rendering should succeed");
        assert!(turn_prompt.text.contains("fix the bug"));
        assert!(turn_prompt.text.contains("Structured response protocol:"));
    }

    #[test]
    fn turn_prompt_for_runtime_uses_compact_refresh_reminder_for_delta_only() {
        // Arrange
        let prompt = TurnPrompt::from("continue the fix");
        let request_kind = AgentRequestKind::SessionResume {
            session_output: None,
        };

        // Act
        let result = turn_prompt_for_runtime(
            prompt,
            &request_kind,
            None,
            InstructionDeliveryMode::DeltaOnly,
        );

        // Assert
        let turn_prompt = result.expect("prompt rendering should succeed");
        assert!(turn_prompt.text.contains("Protocol refresh reminder:"));
        assert!(!turn_prompt.text.contains("Authoritative JSON Schema:"));
    }

    #[test]
    fn turn_prompt_for_runtime_rewrites_user_at_lookups_for_agent_delivery() {
        // Arrange
        let prompt = TurnPrompt::from("review @src/main.rs");
        let request_kind = AgentRequestKind::SessionStart;

        // Act
        let result = turn_prompt_for_runtime(
            prompt,
            &request_kind,
            None,
            InstructionDeliveryMode::BootstrapFull,
        );

        // Assert
        let turn_prompt = result.expect("prompt rendering should succeed");
        assert!(turn_prompt.text.contains("\"looked/up/src/main.rs\""));
        assert!(!turn_prompt.text.contains("@src/main.rs"));
    }

    #[test]
    fn instruction_delivery_mode_for_runtime_reuses_matching_bootstrap_state() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp/test"),
            live_session_output: None,
            model: "test-model".to_string(),
            prompt: TurnPrompt::from("hello"),
            provider_conversation_id: Some("thread-123".to_string()),
            persisted_instruction_conversation_id:
                persisted_instruction_conversation_id_for_session_turn(Some("thread-123")),
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionResume {
                session_output: None,
            },
            session_id: "test-session".to_string(),
        };

        // Act
        let delivery_mode =
            instruction_delivery_mode_for_runtime(&request, Some("thread-123"), false);

        // Assert
        assert_eq!(delivery_mode, InstructionDeliveryMode::DeltaOnly);
    }
}
