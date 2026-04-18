//! Shared helpers used by forge review-request adapters.
//!
//! Each supported forge (GitHub, GitLab) needs the same normalization for
//! authentication failures, host-resolution failures, status-summary joining,
//! provider label casing, and spawn-time error mapping. Keeping these in one
//! module avoids divergence between adapters.

use super::{ForgeCommandError, ForgeKind, ForgeRemote, ReviewRequestError};

/// Returns whether `detail` looks like a forge CLI authentication failure.
///
/// Parameterized on `forge_kind` so the CLI-specific `{cli} auth login`
/// marker stays accurate across forges while the remaining substrings are
/// shared.
pub(crate) fn looks_like_authentication_failure(detail: &str, forge_kind: ForgeKind) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();
    let auth_login_marker = format!("{} auth login", forge_kind.cli_name());

    normalized_detail.contains(&auth_login_marker)
        || normalized_detail.contains("not logged in")
        || normalized_detail.contains("authentication failed")
        || normalized_detail.contains("authentication required")
        || normalized_detail.contains("http 401")
}

/// Returns whether `detail` looks like a DNS or host-resolution failure.
pub(crate) fn looks_like_host_resolution_failure(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("no such host")
        || normalized_detail.contains("name or service not known")
        || normalized_detail.contains("temporary failure in name resolution")
        || normalized_detail.contains("could not resolve host")
        || normalized_detail.contains("lookup ")
}

/// Joins one ordered list of status-summary parts into a comma-separated
/// label, returning `None` when `parts` is empty.
pub(crate) fn status_summary_parts(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }

    Some(parts.join(", "))
}

/// Formats one provider enum-like label into sentence case words.
pub(crate) fn normalize_provider_label(label: &str) -> String {
    let lowercase = label.replace('_', " ").to_ascii_lowercase();
    let mut characters = lowercase.chars();
    let Some(first_character) = characters.next() else {
        return String::new();
    };

    let mut normalized = first_character.to_uppercase().collect::<String>();
    normalized.push_str(characters.as_str());

    normalized
}

/// Maps one spawn-time failure into a normalized review-request error for the
/// forge owning `remote`.
pub(crate) fn map_spawn_error(
    remote: &ForgeRemote,
    error: ForgeCommandError,
) -> ReviewRequestError {
    let forge_kind = remote.forge_kind;

    match error {
        ForgeCommandError::ExecutableNotFound { .. } => {
            ReviewRequestError::CliNotInstalled { forge_kind }
        }
        ForgeCommandError::SpawnFailed { message, .. } => {
            if looks_like_host_resolution_failure(&message) {
                return ReviewRequestError::HostResolutionFailed {
                    forge_kind,
                    host: remote.host.clone(),
                };
            }

            ReviewRequestError::OperationFailed {
                forge_kind,
                message: format!("failed to execute `{}`: {message}", forge_kind.cli_name()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_authentication_failure_matches_github_cli_login_prompt() {
        // Arrange
        let detail = "You are not logged into any GitHub hosts. Run `gh auth login`.";

        // Act
        let matched = looks_like_authentication_failure(detail, ForgeKind::GitHub);

        // Assert
        assert!(matched);
    }

    #[test]
    fn looks_like_authentication_failure_matches_gitlab_cli_login_prompt() {
        // Arrange
        let detail = "You are not logged in. Run `glab auth login`.";

        // Act
        let matched = looks_like_authentication_failure(detail, ForgeKind::GitLab);

        // Assert
        assert!(matched);
    }

    #[test]
    fn looks_like_authentication_failure_matches_http_401() {
        // Arrange
        let detail = "HTTP 401 Unauthorized";

        // Act
        let matched_github = looks_like_authentication_failure(detail, ForgeKind::GitHub);
        let matched_gitlab = looks_like_authentication_failure(detail, ForgeKind::GitLab);

        // Assert
        assert!(matched_github);
        assert!(matched_gitlab);
    }

    #[test]
    fn looks_like_authentication_failure_returns_false_for_unrelated_detail() {
        // Arrange
        let detail = "Request failed: rate limit exceeded";

        // Act
        let matched = looks_like_authentication_failure(detail, ForgeKind::GitHub);

        // Assert
        assert!(!matched);
    }

    #[test]
    fn looks_like_host_resolution_failure_matches_common_dns_errors() {
        // Arrange
        let details = [
            "dial tcp: lookup github.com: no such host",
            "Name or service not known",
            "Temporary failure in name resolution",
            "Could not resolve host: gitlab.example.internal",
        ];

        // Act & Assert
        for detail in details {
            assert!(
                looks_like_host_resolution_failure(detail),
                "expected `{detail}` to match",
            );
        }
    }

    #[test]
    fn looks_like_host_resolution_failure_returns_false_for_unrelated_detail() {
        // Arrange
        let detail = "HTTP 500 Internal Server Error";

        // Act
        let matched = looks_like_host_resolution_failure(detail);

        // Assert
        assert!(!matched);
    }

    #[test]
    fn status_summary_parts_returns_none_for_empty_input() {
        // Arrange
        let parts: Vec<String> = Vec::new();

        // Act
        let summary = status_summary_parts(&parts);

        // Assert
        assert_eq!(summary, None);
    }

    #[test]
    fn status_summary_parts_joins_values_with_commas() {
        // Arrange
        let parts = vec![
            "Draft".to_string(),
            "Approved".to_string(),
            "Mergeable".to_string(),
        ];

        // Act
        let summary = status_summary_parts(&parts);

        // Assert
        assert_eq!(summary.as_deref(), Some("Draft, Approved, Mergeable"));
    }

    #[test]
    fn normalize_provider_label_capitalizes_first_letter_and_replaces_underscores() {
        // Arrange
        let label = "CHANGES_REQUESTED";

        // Act
        let normalized = normalize_provider_label(label);

        // Assert
        assert_eq!(normalized, "Changes requested");
    }

    #[test]
    fn normalize_provider_label_returns_empty_string_for_empty_input() {
        // Arrange
        let label = "";

        // Act
        let normalized = normalize_provider_label(label);

        // Assert
        assert_eq!(normalized, String::new());
    }

    #[test]
    fn map_spawn_error_maps_executable_not_found_to_cli_not_installed() {
        // Arrange
        let remote = sample_remote(ForgeKind::GitHub);
        let error = ForgeCommandError::ExecutableNotFound {
            executable: "gh".to_string(),
        };

        // Act
        let review_request_error = map_spawn_error(&remote, error);

        // Assert
        assert_eq!(
            review_request_error,
            ReviewRequestError::CliNotInstalled {
                forge_kind: ForgeKind::GitHub,
            }
        );
    }

    #[test]
    fn map_spawn_error_maps_host_resolution_failure_for_gitlab() {
        // Arrange
        let remote = sample_remote(ForgeKind::GitLab);
        let error = ForgeCommandError::SpawnFailed {
            executable: "glab".to_string(),
            message: "dial tcp: lookup gitlab.example.internal: no such host".to_string(),
        };

        // Act
        let review_request_error = map_spawn_error(&remote, error);

        // Assert
        assert_eq!(
            review_request_error,
            ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitLab,
                host: "gitlab.example.internal".to_string(),
            }
        );
    }

    #[test]
    fn map_spawn_error_falls_back_to_operation_failed_with_cli_name() {
        // Arrange
        let remote = sample_remote(ForgeKind::GitHub);
        let error = ForgeCommandError::SpawnFailed {
            executable: "gh".to_string(),
            message: "permission denied".to_string(),
        };

        // Act
        let review_request_error = map_spawn_error(&remote, error);

        // Assert
        assert_eq!(
            review_request_error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "failed to execute `gh`: permission denied".to_string(),
            }
        );
    }

    fn sample_remote(forge_kind: ForgeKind) -> ForgeRemote {
        let host = match forge_kind {
            ForgeKind::GitHub => "github.com",
            ForgeKind::GitLab => "gitlab.example.internal",
        };
        ForgeRemote {
            command_working_directory: None,
            forge_kind,
            host: host.to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: format!("https://{host}/agentty-xyz/agentty.git"),
            web_url: format!("https://{host}/agentty-xyz/agentty"),
        }
    }
}
