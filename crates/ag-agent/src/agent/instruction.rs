//! Provider-managed instruction bootstrap planning for app-server sessions.

use crate::channel::AgentRequestKind;

/// Normalizes one provider-native conversation id for persisted bootstrap
/// reuse tracking.
pub fn normalize_instruction_conversation_id(
    provider_conversation_id: Option<&str>,
) -> Option<String> {
    provider_conversation_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// Prompt-shaping mode used for one app-server turn attempt.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum InstructionDeliveryMode {
    /// Send the full instruction contract without transcript replay.
    BootstrapFull,
    /// Reuse the existing provider-managed bootstrap and send only a compact
    /// reminder.
    DeltaOnly,
    /// Re-send the full instruction contract while replaying the transcript
    /// after context loss.
    BootstrapWithReplay,
}

/// Plans how one app-server turn should deliver Agentty's instruction
/// contract.
pub(crate) fn plan_app_server_instruction_delivery(
    request_kind: &AgentRequestKind,
    current_provider_conversation_id: Option<&str>,
    persisted_instruction_conversation_id: Option<&str>,
    should_replay_transcript: bool,
) -> InstructionDeliveryMode {
    if should_replay_transcript {
        return InstructionDeliveryMode::BootstrapWithReplay;
    }

    if matches!(
        request_kind,
        AgentRequestKind::FocusedReview
            | AgentRequestKind::UtilityPrompt
            | AgentRequestKind::AccountRead
    ) {
        return InstructionDeliveryMode::BootstrapFull;
    }

    let current_id = normalize_instruction_conversation_id(current_provider_conversation_id);
    if current_id.is_some() && current_id.as_deref() == persisted_instruction_conversation_id {
        return InstructionDeliveryMode::DeltaOnly;
    }

    InstructionDeliveryMode::BootstrapFull
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_conversation_ids_require_full_bootstrap() {
        // Arrange
        let ids = [None, Some(""), Some("   ")];

        for current_id in ids {
            // Act
            let mode = plan_app_server_instruction_delivery(
                &AgentRequestKind::SessionStart,
                current_id,
                None,
                false,
            );

            // Assert
            assert_eq!(mode, InstructionDeliveryMode::BootstrapFull);
        }
    }

    #[test]
    /// Reuses the provider-managed bootstrap only when the persisted state
    /// still matches the active provider conversation.
    fn test_plan_app_server_instruction_delivery_uses_delta_only_for_matching_state() {
        // Arrange
        let persisted_instruction_conversation_id =
            normalize_instruction_conversation_id(Some("thread-123"));

        // Act
        let mode = plan_app_server_instruction_delivery(
            &AgentRequestKind::SessionResume,
            Some("thread-123"),
            persisted_instruction_conversation_id.as_deref(),
            false,
        );

        // Assert
        assert_eq!(mode, InstructionDeliveryMode::DeltaOnly);
    }

    #[test]
    /// Forces a replay bootstrap whenever the runtime lost provider-managed
    /// context for the active turn.
    fn test_plan_app_server_instruction_delivery_uses_bootstrap_with_replay_after_reset() {
        // Arrange
        let persisted_instruction_conversation_id =
            normalize_instruction_conversation_id(Some("thread-123"));

        // Act
        let mode = plan_app_server_instruction_delivery(
            &AgentRequestKind::SessionResume,
            Some("thread-456"),
            persisted_instruction_conversation_id.as_deref(),
            true,
        );

        // Assert
        assert_eq!(mode, InstructionDeliveryMode::BootstrapWithReplay);
    }

    #[test]
    /// Requires a fresh bootstrap when the provider conversation changed.
    fn test_plan_app_server_instruction_delivery_bootstraps_full_for_new_context() {
        // Arrange
        let persisted_instruction_conversation_id =
            normalize_instruction_conversation_id(Some("thread-123"));

        // Act
        let mode = plan_app_server_instruction_delivery(
            &AgentRequestKind::SessionResume,
            Some("thread-456"),
            persisted_instruction_conversation_id.as_deref(),
            false,
        );

        // Assert
        assert_eq!(mode, InstructionDeliveryMode::BootstrapFull);
    }

    #[test]
    /// Keeps one-shot utility prompts on the full bootstrap path because they
    /// do not reuse long-lived provider context.
    fn test_plan_app_server_instruction_delivery_bootstraps_full_for_utility_prompt() {
        // Arrange
        let persisted_instruction_conversation_id =
            normalize_instruction_conversation_id(Some("thread-123"));

        // Act
        let mode = plan_app_server_instruction_delivery(
            &AgentRequestKind::UtilityPrompt,
            Some("thread-123"),
            persisted_instruction_conversation_id.as_deref(),
            false,
        );

        // Assert
        assert_eq!(mode, InstructionDeliveryMode::BootstrapFull);
    }
}
