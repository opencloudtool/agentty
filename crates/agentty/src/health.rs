use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::agent::AgentKind;

/// Terminal and transitional status values for health checks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HealthStatus {
    Fail,
    Pass,
    Pending,
    Running,
}

/// Renderable state for a single health check row.
#[derive(Clone, Debug)]
pub struct HealthEntry {
    pub label: String,
    pub message: String,
    pub status: HealthStatus,
}

/// Runs all health checks asynchronously and returns shared check entries.
pub fn run_health_checks(git_branch: Option<String>) -> Arc<Mutex<Vec<HealthEntry>>> {
    let checks = health_checks();
    let entries = checks
        .iter()
        .map(|check| HealthEntry {
            label: check.label().to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        })
        .collect();

    let context = HealthContext { git_branch };
    let shared = Arc::new(Mutex::new(entries));
    let shared_bg = Arc::clone(&shared);

    tokio::spawn(async move {
        for (index, check) in checks.iter().enumerate() {
            update_status(&shared_bg, index, HealthStatus::Running, "Checking...");
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;

            let result = check.run(&context).await;
            apply_result(&shared_bg, index, result);
        }
    });

    shared
}

type CheckFuture<'a> = Pin<Box<dyn Future<Output = HealthResult> + Send + 'a>>;

trait HealthCheck: Send + Sync {
    fn label(&self) -> &'static str;

    fn run<'a>(&'a self, context: &'a HealthContext) -> CheckFuture<'a>;
}

/// Health check for agent CLI binaries (`claude`, `gemini`, `codex`).
struct AgentCliCheck {
    agent_kind: AgentKind,
    label: &'static str,
}

impl AgentCliCheck {
    async fn check_agent_cli_tool(agent_kind: AgentKind) -> (HealthStatus, String) {
        let command = agent_kind.to_string();

        match tokio::process::Command::new(&command)
            .arg("--version")
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                let raw_version = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();

                let version = Self::normalize_agent_cli_version(agent_kind, &raw_version);

                (HealthStatus::Pass, version)
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

                (HealthStatus::Fail, format!("Exit code error: {stderr}"))
            }
            Err(_) => (HealthStatus::Fail, "Not found in PATH".to_string()),
        }
    }

    fn normalize_agent_cli_version(agent_kind: AgentKind, raw_version: &str) -> String {
        let trimmed_version = raw_version.trim();

        let normalized_version = match agent_kind {
            AgentKind::Claude => trimmed_version.trim_end_matches(" (Claude Code)"),
            AgentKind::Codex => trimmed_version
                .strip_prefix("codex-cli ")
                .unwrap_or(trimmed_version),
            AgentKind::Gemini => trimmed_version,
        };

        normalized_version.to_string()
    }
}

impl HealthCheck for AgentCliCheck {
    fn label(&self) -> &'static str {
        self.label
    }

    fn run<'a>(&'a self, _context: &'a HealthContext) -> CheckFuture<'a> {
        Box::pin(async move {
            let (status, message) = Self::check_agent_cli_tool(self.agent_kind).await;

            HealthResult::new(status, message)
        })
    }
}

/// Health check for GitHub CLI authentication state.
struct GitHubAuthCheck;

impl GitHubAuthCheck {
    async fn check_github_auth() -> (HealthStatus, String) {
        match tokio::process::Command::new("gh")
            .args(["auth", "status"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let account = Self::parse_auth_field(&stdout, "account ")
                    .and_then(|value| value.split_whitespace().next());
                let scopes = Self::parse_auth_field(&stdout, "Token scopes:")
                    .map(|value| value.replace('\'', ""));
                let detail = match (account, scopes) {
                    (Some(account), Some(scopes)) => format!("{account} ({scopes})"),
                    (Some(account), None) => account.to_string(),
                    _ => "Authenticated".to_string(),
                };

                (HealthStatus::Pass, detail)
            }
            Ok(_) => (HealthStatus::Fail, "Not authenticated".to_string()),
            Err(_) => (HealthStatus::Fail, "gh not found".to_string()),
        }
    }

    fn parse_auth_field<'a>(output: &'a str, prefix: &str) -> Option<&'a str> {
        output.lines().find_map(|line| {
            let start = line.find(prefix)?;

            Some(line[start + prefix.len()..].trim())
        })
    }
}

impl HealthCheck for GitHubAuthCheck {
    fn label(&self) -> &'static str {
        "Auth Status"
    }

    fn run<'a>(&'a self, _context: &'a HealthContext) -> CheckFuture<'a> {
        Box::pin(async move {
            let (status, message) = Self::check_github_auth().await;

            HealthResult::new(status, message)
        })
    }
}

/// Health check for GitHub CLI availability.
struct GitHubCliCheck;

impl GitHubCliCheck {
    async fn check_cli_tool(command: &str) -> (HealthStatus, String) {
        match tokio::process::Command::new(command)
            .arg("--version")
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                (HealthStatus::Pass, version)
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

                (HealthStatus::Fail, format!("Exit code error: {stderr}"))
            }
            Err(_) => (HealthStatus::Fail, "Not found in PATH".to_string()),
        }
    }
}

impl HealthCheck for GitHubCliCheck {
    fn label(&self) -> &'static str {
        "GitHub CLI"
    }

    fn run<'a>(&'a self, _context: &'a HealthContext) -> CheckFuture<'a> {
        Box::pin(async move {
            let (status, message) = Self::check_cli_tool("gh").await;

            HealthResult::new(status, message)
        })
    }
}

/// Health check for git repository presence and branch detection.
struct GitRepoCheck;

impl GitRepoCheck {
    fn check_git_repo(git_branch: Option<&str>) -> (HealthStatus, String) {
        match git_branch {
            Some(branch) => (HealthStatus::Pass, format!("Branch: {branch}")),
            None => (HealthStatus::Fail, "Not a git repository".to_string()),
        }
    }
}

impl HealthCheck for GitRepoCheck {
    fn label(&self) -> &'static str {
        "Git Repository"
    }

    fn run<'a>(&'a self, context: &'a HealthContext) -> CheckFuture<'a> {
        Box::pin(async move {
            let (status, message) = Self::check_git_repo(context.git_branch.as_deref());

            HealthResult::new(status, message)
        })
    }
}

/// Runtime context shared by health checks.
struct HealthContext {
    git_branch: Option<String>,
}

/// Internal normalized result produced by each health check.
struct HealthResult {
    message: String,
    status: HealthStatus,
}

impl HealthResult {
    fn new(message_status: HealthStatus, message_text: String) -> Self {
        Self {
            message: message_text,
            status: message_status,
        }
    }
}

fn health_checks() -> Vec<Box<dyn HealthCheck>> {
    vec![
        Box::new(GitRepoCheck),
        Box::new(GitHubCliCheck),
        Box::new(GitHubAuthCheck),
        Box::new(AgentCliCheck {
            agent_kind: AgentKind::Claude,
            label: "Claude Code",
        }),
        Box::new(AgentCliCheck {
            agent_kind: AgentKind::Gemini,
            label: "Gemini CLI",
        }),
        Box::new(AgentCliCheck {
            agent_kind: AgentKind::Codex,
            label: "Codex CLI",
        }),
    ]
}

fn update_status(
    entries: &Arc<Mutex<Vec<HealthEntry>>>,
    index: usize,
    status: HealthStatus,
    message: &str,
) {
    if let Ok(mut lock) = entries.lock() {
        if let Some(entry) = lock.get_mut(index) {
            entry.message = message.to_string();
            entry.status = status;
        }
    }
}

fn apply_result(entries: &Arc<Mutex<Vec<HealthEntry>>>, index: usize, result: HealthResult) {
    if let Ok(mut lock) = entries.lock() {
        if let Some(entry) = lock.get_mut(index) {
            entry.message = result.message;
            entry.status = result.status;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn test_health_check_labels() {
        // Arrange & Act
        let labels: Vec<&str> = health_checks().iter().map(|check| check.label()).collect();

        // Assert
        assert_eq!(
            labels,
            vec![
                "Git Repository",
                "GitHub CLI",
                "Auth Status",
                "Claude Code",
                "Gemini CLI",
                "Codex CLI"
            ]
        );
    }

    #[test]
    fn test_health_entry_defaults() {
        // Arrange & Act
        let entry = HealthEntry {
            label: "Database".to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        };

        // Assert
        assert_eq!(entry.label, "Database");
        assert_eq!(entry.status, HealthStatus::Pending);
        assert!(entry.message.is_empty());
    }

    #[test]
    fn test_update_status() {
        // Arrange
        let entries = Arc::new(Mutex::new(vec![HealthEntry {
            label: "Database".to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        }]));

        // Act
        update_status(&entries, 0, HealthStatus::Pass, "OK");

        // Assert
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock[0].status, HealthStatus::Pass);
        assert_eq!(lock[0].message, "OK");
    }

    #[test]
    fn test_update_status_out_of_bounds() {
        // Arrange
        let entries = Arc::new(Mutex::new(vec![HealthEntry {
            label: "Database".to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        }]));

        // Act — should not panic
        update_status(&entries, 99, HealthStatus::Fail, "Error");

        // Assert — original entry unchanged
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock[0].status, HealthStatus::Pending);
    }

    #[test]
    fn test_apply_result() {
        // Arrange
        let entries = Arc::new(Mutex::new(vec![HealthEntry {
            label: "Database".to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        }]));

        // Act
        apply_result(
            &entries,
            0,
            HealthResult::new(HealthStatus::Pass, "OK".to_string()),
        );

        // Assert
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock[0].status, HealthStatus::Pass);
        assert_eq!(lock[0].message, "OK");
    }

    #[test]
    fn test_check_git_repo_with_branch() {
        // Arrange
        let branch = "main".to_string();

        // Act
        let (status, message) = GitRepoCheck::check_git_repo(Some(&branch));

        // Assert
        assert_eq!(status, HealthStatus::Pass);
        assert_eq!(message, "Branch: main");
    }

    #[test]
    fn test_check_git_repo_without_branch() {
        // Arrange & Act
        let (status, message) = GitRepoCheck::check_git_repo(None);

        // Assert
        assert_eq!(status, HealthStatus::Fail);
        assert_eq!(message, "Not a git repository");
    }

    #[tokio::test]
    async fn test_check_github_cli_result() {
        // Arrange
        let check = GitHubCliCheck;
        let context = HealthContext { git_branch: None };

        // Act
        let result = check.run(&context).await;

        // Assert — result depends on whether gh is installed
        assert!(
            result.status == HealthStatus::Pass || result.status == HealthStatus::Fail,
            "Expected Pass or Fail, got {:?}",
            result.status
        );
        assert!(!result.message.is_empty());
    }

    #[tokio::test]
    async fn test_check_cli_tool_nonexistent() {
        // Arrange & Act
        let (status, message) = GitHubCliCheck::check_cli_tool("nonexistent_tool_xyz_123").await;

        // Assert
        assert_eq!(status, HealthStatus::Fail);
        assert_eq!(message, "Not found in PATH");
    }

    #[tokio::test]
    async fn test_check_codex_cli_result() {
        // Arrange & Act
        let (status, message) = AgentCliCheck::check_agent_cli_tool(AgentKind::Codex).await;

        // Assert
        assert!(!message.is_empty());
        if status == HealthStatus::Fail {
            assert!(message == "Not found in PATH" || message.starts_with("Exit code error:"));
        }
    }

    #[test]
    fn test_normalize_agent_cli_version_for_codex() {
        // Arrange
        let raw_version = "codex-cli 0.1.0";

        // Act
        let version = AgentCliCheck::normalize_agent_cli_version(AgentKind::Codex, raw_version);

        // Assert
        assert_eq!(version, "0.1.0");
    }

    #[test]
    fn test_normalize_agent_cli_version_for_claude() {
        // Arrange
        let raw_version = "1.2.3 (Claude Code)";

        // Act
        let version = AgentCliCheck::normalize_agent_cli_version(AgentKind::Claude, raw_version);

        // Assert
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn test_normalize_agent_cli_version_for_gemini() {
        // Arrange
        let raw_version = "gemini 9.9.9";

        // Act
        let version = AgentCliCheck::normalize_agent_cli_version(AgentKind::Gemini, raw_version);

        // Assert
        assert_eq!(version, "gemini 9.9.9");
    }

    #[tokio::test]
    async fn test_check_github_auth_result() {
        // Arrange & Act
        let (status, message) = GitHubAuthCheck::check_github_auth().await;

        // Assert
        assert!(!message.is_empty());
        if status == HealthStatus::Fail {
            assert!(message == "Not authenticated" || message == "gh not found");
        }
    }

    #[test]
    fn test_parse_auth_field_with_dash_bullet() {
        // Arrange
        let output = "  - Active account: true\n  - Token scopes: 'repo', 'read:org'\n";

        // Act
        let account = GitHubAuthCheck::parse_auth_field(output, "Active account:");
        let scopes = GitHubAuthCheck::parse_auth_field(output, "Token scopes:");

        // Assert
        assert_eq!(account, Some("true"));
        assert_eq!(scopes, Some("'repo', 'read:org'"));
    }

    #[test]
    fn test_parse_auth_field_with_check_bullet() {
        // Arrange — matches actual `gh auth status` output
        let output =
            "  \u{2713} Logged in to github.com account user (keyring)\n  - Token scopes: 'repo'\n";

        // Act
        let account = GitHubAuthCheck::parse_auth_field(output, "account ")
            .and_then(|value| value.split_whitespace().next());
        let scopes = GitHubAuthCheck::parse_auth_field(output, "Token scopes:")
            .map(|value| value.replace('\'', ""));

        // Assert
        assert_eq!(account, Some("user"));
        assert_eq!(scopes, Some("repo".to_string()));
    }

    #[test]
    fn test_parse_auth_field_missing() {
        // Arrange
        let output = "  - Active account: true\n";

        // Act
        let result = GitHubAuthCheck::parse_auth_field(output, "Token scopes:");

        // Assert
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_run_health_checks_creates_entries() {
        // Arrange & Act
        let entries = run_health_checks(Some("main".to_string()));

        // Assert — entries are created immediately with Pending status
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock.len(), health_checks().len());
        for entry in lock.iter() {
            assert!(
                entry.status == HealthStatus::Pending || entry.status == HealthStatus::Running,
                "Expected Pending or Running, got {:?}",
                entry.status
            );
        }
    }

    #[tokio::test]
    async fn test_run_health_checks_completes() {
        // Arrange & Act
        let entries = run_health_checks(Some("main".to_string()));
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        // Assert — all checks should have completed
        let lock = entries.lock().expect("failed to lock");
        for entry in lock.iter() {
            assert!(
                entry.status != HealthStatus::Pending && entry.status != HealthStatus::Running,
                "Expected completed status for {:?}, got {:?}",
                entry.label,
                entry.status
            );
        }

        // Git should pass with branch
        assert_eq!(lock[0].status, HealthStatus::Pass);
        assert_eq!(lock[0].message, "Branch: main");
    }
}
