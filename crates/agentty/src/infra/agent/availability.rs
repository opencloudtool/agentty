//! Machine-scoped agent executable discovery.

use std::env;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::domain::agent::AgentKind;

/// Detects which provider CLIs are locally runnable on the current machine.
#[cfg_attr(test, mockall::automock)]
pub trait AgentAvailabilityProbe: Send + Sync {
    /// Returns the agent kinds whose backing CLI executable is available.
    fn available_agent_kinds(&self) -> Vec<AgentKind>;
}

/// Production availability probe backed by `PATH` executable discovery.
pub struct RealAgentAvailabilityProbe;

impl AgentAvailabilityProbe for RealAgentAvailabilityProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        available_agent_kinds_from_path(env::var_os("PATH").as_deref())
    }
}

/// Returns the CLI executable name used by the provided agent kind.
#[must_use]
pub fn executable_name(agent_kind: AgentKind) -> &'static str {
    match agent_kind {
        AgentKind::Gemini => "gemini",
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
    }
}

/// Returns agent kinds whose executables are present on one `PATH` value.
fn available_agent_kinds_from_path(path_value: Option<&OsStr>) -> Vec<AgentKind> {
    AgentKind::ALL
        .iter()
        .copied()
        .filter(|agent_kind| is_executable_on_path(path_value, executable_name(*agent_kind)))
        .collect()
}

/// Returns whether one executable name resolves to a file on `PATH`.
fn is_executable_on_path(path_value: Option<&OsStr>, executable_name: &str) -> bool {
    path_value
        .map(env::split_paths)
        .into_iter()
        .flatten()
        .map(|path_entry| candidate_path_for_executable_name(&path_entry, executable_name))
        .any(|candidate_path| is_executable_file(&candidate_path))
}

/// Returns the candidate filesystem path for one executable name within a
/// single `PATH` entry.
fn candidate_path_for_executable_name(path_entry: &Path, executable_name: &str) -> PathBuf {
    path_entry.join(executable_name)
}

/// Returns whether the candidate path is a regular file with at least one
/// execute bit set.
fn is_executable_file(candidate_path: &Path) -> bool {
    let Ok(metadata) = candidate_path.metadata() else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    /// Ensures executable names stay aligned with provider command names.
    fn test_executable_name_matches_agent_cli_names() {
        // Arrange / Act / Assert
        assert_eq!(executable_name(AgentKind::Gemini), "gemini");
        assert_eq!(executable_name(AgentKind::Claude), "claude");
        assert_eq!(executable_name(AgentKind::Codex), "codex");
    }

    #[test]
    /// Ensures the production probe reports only agent kinds whose
    /// executables are present on the current `PATH`.
    fn test_real_agent_availability_probe_filters_missing_executables() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        let gemini_path = temp_directory.path().join("gemini");
        fs::write(&codex_path, "").expect("failed to create codex executable");
        fs::write(&gemini_path, "").expect("failed to create gemini executable");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark codex executable");
        fs::set_permissions(&gemini_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark gemini executable");
        let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

        // Act
        let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

        // Assert
        assert_eq!(
            available_agent_kinds,
            vec![AgentKind::Gemini, AgentKind::Codex]
        );
    }

    #[test]
    /// Ensures probe discovery ignores non-executable files even when their
    /// names match supported agent CLIs.
    fn test_real_agent_availability_probe_ignores_non_executable_files() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        fs::write(&codex_path, "").expect("failed to create codex file");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o644))
            .expect("failed to mark codex non-executable");
        let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

        // Act
        let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

        // Assert
        assert!(available_agent_kinds.is_empty());
    }
}
