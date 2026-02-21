//! Version discovery helpers for update notifications.

use std::process::Command;

use semver::Version;
use serde::Deserialize;

const AGENTTY_NPM_PACKAGE: &str = "agentty";
const NPM_REGISTRY_LATEST_URL: &str = "https://registry.npmjs.org/agentty/latest";

#[derive(Debug, Deserialize)]
struct NpmRegistryLatestResponse {
    version: String,
}

/// Returns the latest npmjs version tag (`vX.Y.Z`) for `agentty`.
pub async fn latest_npm_version_tag() -> Option<String> {
    tokio::task::spawn_blocking(fetch_latest_npm_version_tag_sync)
        .await
        .ok()
        .flatten()
}

/// Returns `true` when `candidate_version` is newer than `current_version`.
pub(crate) fn is_newer_than_current_version(
    current_version: &str,
    candidate_version: &str,
) -> bool {
    let Some(current_version) = parse_version(current_version) else {
        return false;
    };

    let Some(candidate_version) = parse_version(candidate_version) else {
        return false;
    };

    candidate_version > current_version
}

fn fetch_latest_npm_version_tag_sync() -> Option<String> {
    if let Some(latest_version) = fetch_latest_version_with_npm_cli() {
        return Some(version_tag(&latest_version));
    }

    let latest_version = fetch_latest_version_with_registry_curl()?;

    Some(version_tag(&latest_version))
}

fn fetch_latest_version_with_npm_cli() -> Option<Version> {
    let output = Command::new("npm")
        .args(["view", AGENTTY_NPM_PACKAGE, "version", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let response = String::from_utf8_lossy(&output.stdout);

    parse_npm_cli_version_response(response.as_ref())
}

fn parse_npm_cli_version_response(response: &str) -> Option<Version> {
    let version: String = serde_json::from_str(response).ok()?;

    parse_version(&version)
}

fn fetch_latest_version_with_registry_curl() -> Option<Version> {
    let output = Command::new("curl")
        .args(["-fsSL", NPM_REGISTRY_LATEST_URL])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let response = String::from_utf8_lossy(&output.stdout);

    parse_registry_latest_response(response.as_ref())
}

fn parse_registry_latest_response(response: &str) -> Option<Version> {
    let payload: NpmRegistryLatestResponse = serde_json::from_str(response).ok()?;

    parse_version(&payload.version)
}

fn parse_version(version: &str) -> Option<Version> {
    let normalized_version = version.strip_prefix('v').unwrap_or(version);

    Version::parse(normalized_version).ok()
}

fn version_tag(version: &Version) -> String {
    format!("v{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_accepts_prefixed_version() {
        // Arrange
        let version = "v1.2.3";

        // Act
        let parsed_version = parse_version(version);

        // Assert
        assert_eq!(parsed_version, Some(Version::new(1, 2, 3)));
    }

    #[test]
    fn test_parse_version_rejects_invalid_version() {
        // Arrange
        let version = "vnext";

        // Act
        let parsed_version = parse_version(version);

        // Assert
        assert_eq!(parsed_version, None);
    }

    #[test]
    fn test_parse_npm_cli_version_response_accepts_json_string() {
        // Arrange
        let response = "\"0.1.14\"";

        // Act
        let parsed_version = parse_npm_cli_version_response(response);

        // Assert
        assert_eq!(parsed_version, Some(Version::new(0, 1, 14)));
    }

    #[test]
    fn test_parse_registry_latest_response_extracts_version() {
        // Arrange
        let response = r#"{"name":"agentty","version":"0.1.14"}"#;

        // Act
        let parsed_version = parse_registry_latest_response(response);

        // Assert
        assert_eq!(parsed_version, Some(Version::new(0, 1, 14)));
    }

    #[test]
    fn test_version_tag_prefixes_semver_with_v() {
        // Arrange
        let version = Version::new(0, 1, 14);

        // Act
        let version_tag = version_tag(&version);

        // Assert
        assert_eq!(version_tag, "v0.1.14");
    }

    #[test]
    fn test_is_newer_than_current_version_returns_true_when_candidate_is_newer() {
        // Arrange
        let current_version = "0.1.11";
        let candidate_version = "v0.1.12";

        // Act
        let is_newer = is_newer_than_current_version(current_version, candidate_version);

        // Assert
        assert!(is_newer);
    }

    #[test]
    fn test_is_newer_than_current_version_returns_false_when_candidate_is_not_newer() {
        // Arrange
        let current_version = "0.1.12";
        let candidate_version = "v0.1.11";

        // Act
        let is_newer = is_newer_than_current_version(current_version, candidate_version);

        // Assert
        assert!(!is_newer);
    }
}
