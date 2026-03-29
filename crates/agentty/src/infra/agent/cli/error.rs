//! Shared provider-aware CLI exit error formatting.

use crate::domain::agent::AgentKind;

/// Formats one failed agent CLI command into a user-facing error string.
pub(crate) fn format_agent_cli_exit_error(
    agent_kind: AgentKind,
    command_label: &str,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    if let Some(guidance) = known_agent_cli_exit_guidance(agent_kind, command_label, stdout, stderr)
    {
        return guidance;
    }

    let exit_code = exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    let output_detail = agent_cli_output_detail(stdout, stderr);

    format!("{command_label} failed with exit code {exit_code}: {output_detail}")
}

/// Returns provider-specific guidance for known CLI command failures.
fn known_agent_cli_exit_guidance(
    agent_kind: AgentKind,
    command_label: &str,
    stdout: &str,
    stderr: &str,
) -> Option<String> {
    match agent_kind {
        AgentKind::Claude if is_claude_authentication_error(stdout, stderr) => {
            Some(claude_authentication_error_message(command_label))
        }
        AgentKind::Claude | AgentKind::Codex | AgentKind::Gemini => None,
    }
}

/// Builds the actionable Claude authentication refresh guidance message.
fn claude_authentication_error_message(command_label: &str) -> String {
    format!(
        "{command_label} failed because Claude authentication expired or is missing.\nRun `claude \
         auth login` to refresh your Anthropic session, verify with `claude auth status`, then \
         retry."
    )
}

/// Detects Claude CLI authentication failures surfaced through stdout/stderr.
fn is_claude_authentication_error(stdout: &str, stderr: &str) -> bool {
    let combined_output = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    combined_output.contains("oauth token has expired")
        || combined_output.contains("failed to authenticate")
        || combined_output.contains("authentication_error")
}

/// Formats captured stdout/stderr into one compact CLI error detail string.
fn agent_cli_output_detail(stdout: &str, stderr: &str) -> String {
    let trimmed_stdout = stdout.trim();
    let trimmed_stderr = stderr.trim();

    match (trimmed_stdout.is_empty(), trimmed_stderr.is_empty()) {
        (false, false) => format!("stdout: {trimmed_stdout}; stderr: {trimmed_stderr}"),
        (false, true) => format!("stdout: {trimmed_stdout}"),
        (true, false) => format!("stderr: {trimmed_stderr}"),
        (true, true) => "no output".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- format_agent_cli_exit_error ---

    #[test]
    fn test_format_generic_error_with_exit_code_and_stderr() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "",
            "something broke",
        );

        // Assert
        assert_eq!(
            result,
            "claude code failed with exit code 1: stderr: something broke"
        );
    }

    #[test]
    fn test_format_generic_error_with_unknown_exit_code() {
        // Arrange / Act
        let result =
            format_agent_cli_exit_error(AgentKind::Gemini, "gemini", None, "", "crash output");

        // Assert
        assert!(result.contains("exit code unknown"));
    }

    #[test]
    fn test_format_generic_error_with_both_stdout_and_stderr() {
        // Arrange / Act
        let result =
            format_agent_cli_exit_error(AgentKind::Codex, "codex", Some(2), "out text", "err text");

        // Assert
        assert!(result.contains("stdout: out text"));
        assert!(result.contains("stderr: err text"));
    }

    #[test]
    fn test_format_generic_error_with_no_output() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(AgentKind::Claude, "claude", Some(1), "", "");

        // Assert
        assert!(result.contains("no output"));
    }

    // --- Claude authentication error detection ---

    #[test]
    fn test_claude_auth_error_from_expired_oauth_token_in_stderr() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "",
            "OAuth token has expired",
        );

        // Assert
        assert!(result.contains("authentication expired or is missing"));
        assert!(result.contains("claude auth login"));
    }

    #[test]
    fn test_claude_auth_error_from_failed_to_authenticate_in_stdout() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "Failed to authenticate user",
            "",
        );

        // Assert
        assert!(result.contains("authentication expired or is missing"));
    }

    #[test]
    fn test_claude_auth_error_from_authentication_error_keyword() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "",
            "authentication_error: invalid key",
        );

        // Assert
        assert!(result.contains("claude auth login"));
    }

    #[test]
    fn test_claude_auth_detection_is_case_insensitive() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude",
            Some(1),
            "OAUTH TOKEN HAS EXPIRED",
            "",
        );

        // Assert
        assert!(result.contains("authentication expired or is missing"));
    }

    #[test]
    fn test_non_claude_provider_ignores_auth_keywords() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Gemini,
            "gemini",
            Some(1),
            "",
            "OAuth token has expired",
        );

        // Assert
        assert!(!result.contains("claude auth login"));
        assert!(result.contains("exit code 1"));
    }

    // --- agent_cli_output_detail ---

    #[test]
    fn test_output_detail_with_only_stdout() {
        // Arrange / Act
        let detail = agent_cli_output_detail("output text", "");

        // Assert
        assert_eq!(detail, "stdout: output text");
    }

    #[test]
    fn test_output_detail_with_only_stderr() {
        // Arrange / Act
        let detail = agent_cli_output_detail("", "error text");

        // Assert
        assert_eq!(detail, "stderr: error text");
    }

    #[test]
    fn test_output_detail_with_both_streams() {
        // Arrange / Act
        let detail = agent_cli_output_detail("out", "err");

        // Assert
        assert_eq!(detail, "stdout: out; stderr: err");
    }

    #[test]
    fn test_output_detail_with_empty_streams() {
        // Arrange / Act
        let detail = agent_cli_output_detail("", "");

        // Assert
        assert_eq!(detail, "no output");
    }

    #[test]
    fn test_output_detail_trims_whitespace() {
        // Arrange / Act
        let detail = agent_cli_output_detail("  out  ", "  err  ");

        // Assert
        assert_eq!(detail, "stdout: out; stderr: err");
    }

    // --- is_claude_authentication_error ---

    #[test]
    fn test_is_claude_auth_error_returns_false_for_unrelated_output() {
        // Arrange / Act / Assert
        assert!(!is_claude_authentication_error(
            "normal output",
            "normal error"
        ));
    }

    #[test]
    fn test_is_claude_auth_error_returns_true_for_mixed_case_in_stderr() {
        // Arrange / Act / Assert
        assert!(is_claude_authentication_error("", "FAILED TO AUTHENTICATE"));
    }

    // --- known_agent_cli_exit_guidance ---

    #[test]
    fn test_known_guidance_returns_none_for_non_auth_claude_error() {
        // Arrange / Act
        let result =
            known_agent_cli_exit_guidance(AgentKind::Claude, "claude", "normal out", "normal err");

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_known_guidance_returns_none_for_codex() {
        // Arrange / Act
        let result =
            known_agent_cli_exit_guidance(AgentKind::Codex, "codex", "OAuth token has expired", "");

        // Assert
        assert!(result.is_none());
    }
}
