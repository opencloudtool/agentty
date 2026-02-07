use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;

use crate::agent::AgentKind;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HealthCheckKind {
    AgentClaude,
    AgentCodex,
    AgentGemini,
    Database,
    GitHubCli,
    GitRepo,
}

impl HealthCheckKind {
    pub const ALL: &[HealthCheckKind] = &[
        HealthCheckKind::Database,
        HealthCheckKind::GitRepo,
        HealthCheckKind::GitHubCli,
        HealthCheckKind::AgentClaude,
        HealthCheckKind::AgentGemini,
        HealthCheckKind::AgentCodex,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HealthCheckKind::AgentClaude => "Claude Code",
            HealthCheckKind::AgentCodex => "Codex CLI",
            HealthCheckKind::AgentGemini => "Gemini CLI",
            HealthCheckKind::Database => "Database",
            HealthCheckKind::GitHubCli => "GitHub CLI",
            HealthCheckKind::GitRepo => "Git Repository",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HealthStatus {
    Fail,
    Pass,
    Pending,
    Running,
    Warn,
}

#[derive(Clone, Debug)]
pub struct HealthEntry {
    pub children: Vec<HealthEntry>,
    pub label: String,
    pub message: String,
    pub status: HealthStatus,
}

struct HealthResult {
    children: Vec<HealthEntry>,
    message: String,
    status: HealthStatus,
}

impl From<(HealthStatus, String)> for HealthResult {
    fn from((status, message): (HealthStatus, String)) -> Self {
        Self {
            children: Vec::new(),
            message,
            status,
        }
    }
}

pub fn run_health_checks(
    pool: SqlitePool,
    git_branch: Option<String>,
) -> Arc<Mutex<Vec<HealthEntry>>> {
    let entries: Vec<HealthEntry> = HealthCheckKind::ALL
        .iter()
        .map(|&kind| HealthEntry {
            children: Vec::new(),
            label: kind.label().to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        })
        .collect();

    let shared = Arc::new(Mutex::new(entries));
    let shared_bg = Arc::clone(&shared);

    tokio::spawn(async move {
        for (index, &kind) in HealthCheckKind::ALL.iter().enumerate() {
            update_status(&shared_bg, index, HealthStatus::Running, "Checking...");
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;

            let result: HealthResult = match kind {
                HealthCheckKind::Database => check_database(&pool).await.into(),
                HealthCheckKind::GitRepo => check_git_repo(git_branch.as_ref()).into(),
                HealthCheckKind::GitHubCli => check_github_cli().await,
                HealthCheckKind::AgentClaude => {
                    check_agent_cli_tool(AgentKind::Claude).await.into()
                }
                HealthCheckKind::AgentGemini => {
                    check_agent_cli_tool(AgentKind::Gemini).await.into()
                }
                HealthCheckKind::AgentCodex => check_agent_cli_tool(AgentKind::Codex).await.into(),
            };

            apply_result(&shared_bg, index, result);
        }
    });

    shared
}

fn update_status(
    entries: &Arc<Mutex<Vec<HealthEntry>>>,
    index: usize,
    status: HealthStatus,
    message: &str,
) {
    if let Ok(mut lock) = entries.lock() {
        if let Some(entry) = lock.get_mut(index) {
            entry.status = status;
            entry.message = message.to_string();
        }
    }
}

fn apply_result(entries: &Arc<Mutex<Vec<HealthEntry>>>, index: usize, result: HealthResult) {
    if let Ok(mut lock) = entries.lock() {
        if let Some(entry) = lock.get_mut(index) {
            entry.children = result.children;
            entry.message = result.message;
            entry.status = result.status;
        }
    }
}

async fn check_database(pool: &SqlitePool) -> (HealthStatus, String) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_) => (HealthStatus::Pass, "OK".to_string()),
        Err(err) => (HealthStatus::Fail, format!("{err}")),
    }
}

fn check_git_repo(git_branch: Option<&String>) -> (HealthStatus, String) {
    match git_branch {
        Some(branch) => (HealthStatus::Pass, format!("Branch: {branch}")),
        None => (HealthStatus::Warn, "Not a git repository".to_string()),
    }
}

async fn check_github_cli() -> HealthResult {
    let (status, message) = check_cli_tool("gh").await;
    if status != HealthStatus::Pass {
        return (status, message).into();
    }

    let auth_entry = check_github_auth().await;
    HealthResult {
        children: vec![auth_entry],
        message,
        status,
    }
}

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

            let version = normalize_agent_cli_version(agent_kind, &raw_version);

            (HealthStatus::Pass, version)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            (HealthStatus::Warn, format!("Exit code error: {stderr}"))
        }
        Err(_) => (HealthStatus::Warn, "Not found in PATH".to_string()),
    }
}

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
            (HealthStatus::Warn, format!("Exit code error: {stderr}"))
        }
        Err(_) => (HealthStatus::Warn, "Not found in PATH".to_string()),
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

async fn check_github_auth() -> HealthEntry {
    let (status, message) = match tokio::process::Command::new("gh")
        .args(["auth", "status"])
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let account = parse_auth_field(&stdout, "account ")
                .and_then(|value| value.split_whitespace().next());
            let scopes =
                parse_auth_field(&stdout, "Token scopes:").map(|value| value.replace('\'', ""));
            let detail = match (account, scopes) {
                (Some(account), Some(scopes)) => format!("{account} ({scopes})"),
                (Some(account), None) => account.to_string(),
                _ => "Authenticated".to_string(),
            };
            (HealthStatus::Pass, detail)
        }
        Ok(_) => (HealthStatus::Warn, "Not authenticated".to_string()),
        Err(_) => (HealthStatus::Warn, "gh not found".to_string()),
    };

    HealthEntry {
        children: Vec::new(),
        label: "Auth Status".to_string(),
        message,
        status,
    }
}

fn parse_auth_field<'a>(output: &'a str, prefix: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let start = line.find(prefix)?;
        Some(line[start + prefix.len()..].trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_check_kind_all_count() {
        // Arrange & Act & Assert
        assert_eq!(HealthCheckKind::ALL.len(), 6);
    }

    #[test]
    fn test_health_check_kind_label() {
        // Arrange & Act & Assert
        assert_eq!(HealthCheckKind::Database.label(), "Database");
        assert_eq!(HealthCheckKind::GitRepo.label(), "Git Repository");
        assert_eq!(HealthCheckKind::GitHubCli.label(), "GitHub CLI");
        assert_eq!(HealthCheckKind::AgentClaude.label(), "Claude Code");
        assert_eq!(HealthCheckKind::AgentGemini.label(), "Gemini CLI");
        assert_eq!(HealthCheckKind::AgentCodex.label(), "Codex CLI");
    }

    #[test]
    fn test_health_entry_defaults() {
        // Arrange & Act
        let entry = HealthEntry {
            children: Vec::new(),
            label: "Database".to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        };

        // Assert
        assert_eq!(entry.label, "Database");
        assert_eq!(entry.status, HealthStatus::Pending);
        assert!(entry.message.is_empty());
        assert!(entry.children.is_empty());
    }

    #[test]
    fn test_update_status() {
        // Arrange
        let entries = Arc::new(Mutex::new(vec![HealthEntry {
            children: Vec::new(),
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
            children: Vec::new(),
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
            children: Vec::new(),
            label: "Database".to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        }]));

        // Act
        apply_result(&entries, 0, (HealthStatus::Pass, "OK".to_string()).into());

        // Assert
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock[0].status, HealthStatus::Pass);
        assert_eq!(lock[0].message, "OK");
        assert!(lock[0].children.is_empty());
    }

    #[test]
    fn test_apply_result_with_children() {
        // Arrange
        let entries = Arc::new(Mutex::new(vec![HealthEntry {
            children: Vec::new(),
            label: "GitHub CLI".to_string(),
            message: String::new(),
            status: HealthStatus::Pending,
        }]));
        let result = HealthResult {
            children: vec![HealthEntry {
                children: Vec::new(),
                label: "Auth Status".to_string(),
                message: "Authenticated".to_string(),
                status: HealthStatus::Pass,
            }],
            message: "gh version 2.0".to_string(),
            status: HealthStatus::Pass,
        };

        // Act
        apply_result(&entries, 0, result);

        // Assert
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock[0].status, HealthStatus::Pass);
        assert_eq!(lock[0].message, "gh version 2.0");
        assert_eq!(lock[0].children.len(), 1);
        assert_eq!(lock[0].children[0].label, "Auth Status");
        assert_eq!(lock[0].children[0].status, HealthStatus::Pass);
    }

    #[test]
    fn test_check_git_repo_with_branch() {
        // Arrange
        let branch = "main".to_string();

        // Act
        let (status, message) = check_git_repo(Some(&branch));

        // Assert
        assert_eq!(status, HealthStatus::Pass);
        assert_eq!(message, "Branch: main");
    }

    #[test]
    fn test_check_git_repo_without_branch() {
        // Arrange & Act
        let (status, message) = check_git_repo(None);

        // Assert
        assert_eq!(status, HealthStatus::Warn);
        assert_eq!(message, "Not a git repository");
    }

    #[tokio::test]
    async fn test_check_database_with_pool() {
        // Arrange
        let db = crate::db::Database::open_in_memory()
            .await
            .expect("failed to open db");
        let pool = db.pool().clone();

        // Act
        let (status, message) = check_database(&pool).await;

        // Assert
        assert_eq!(status, HealthStatus::Pass);
        assert_eq!(message, "OK");
    }

    #[tokio::test]
    async fn test_check_github_cli_result() {
        // Arrange & Act
        let result = check_github_cli().await;

        // Assert — result depends on whether gh is installed
        assert!(
            result.status == HealthStatus::Pass || result.status == HealthStatus::Warn,
            "Expected Pass or Warn, got {:?}",
            result.status
        );
        assert!(!result.message.is_empty());

        // If the binary was found, there should be an auth child
        if result.status == HealthStatus::Pass {
            assert_eq!(result.children.len(), 1);
            assert_eq!(result.children[0].label, "Auth Status");
        }
    }

    #[tokio::test]
    async fn test_check_cli_tool_nonexistent() {
        // Arrange & Act
        let (status, message) = check_cli_tool("nonexistent_tool_xyz_123").await;

        // Assert
        assert_eq!(status, HealthStatus::Warn);
        assert_eq!(message, "Not found in PATH");
    }

    #[tokio::test]
    async fn test_check_codex_cli_result() {
        // Arrange & Act
        let (status, message) = check_agent_cli_tool(AgentKind::Codex).await;

        // Assert
        assert!(
            status == HealthStatus::Pass || status == HealthStatus::Warn,
            "Expected Pass or Warn, got {status:?}"
        );
        assert!(!message.is_empty());
    }

    #[test]
    fn test_normalize_agent_cli_version_for_codex() {
        // Arrange
        let raw_version = "codex-cli 0.1.0";

        // Act
        let version = normalize_agent_cli_version(AgentKind::Codex, raw_version);

        // Assert
        assert_eq!(version, "0.1.0");
    }

    #[test]
    fn test_normalize_agent_cli_version_for_claude() {
        // Arrange
        let raw_version = "1.2.3 (Claude Code)";

        // Act
        let version = normalize_agent_cli_version(AgentKind::Claude, raw_version);

        // Assert
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn test_normalize_agent_cli_version_for_gemini() {
        // Arrange
        let raw_version = "gemini 9.9.9";

        // Act
        let version = normalize_agent_cli_version(AgentKind::Gemini, raw_version);

        // Assert
        assert_eq!(version, "gemini 9.9.9");
    }

    #[tokio::test]
    async fn test_check_github_auth_returns_entry() {
        // Arrange & Act
        let entry = check_github_auth().await;

        // Assert
        assert_eq!(entry.label, "Auth Status");
        assert!(
            entry.status == HealthStatus::Pass || entry.status == HealthStatus::Warn,
            "Expected Pass or Warn, got {:?}",
            entry.status
        );
        assert!(!entry.message.is_empty());
    }

    #[test]
    fn test_parse_auth_field_with_dash_bullet() {
        // Arrange
        let output = "  - Active account: true\n  - Token scopes: 'repo', 'read:org'\n";

        // Act
        let account = parse_auth_field(output, "Active account:");
        let scopes = parse_auth_field(output, "Token scopes:");

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
        let account =
            parse_auth_field(output, "account ").and_then(|value| value.split_whitespace().next());
        let scopes = parse_auth_field(output, "Token scopes:").map(|value| value.replace('\'', ""));

        // Assert
        assert_eq!(account, Some("user"));
        assert_eq!(scopes, Some("repo".to_string()));
    }

    #[test]
    fn test_parse_auth_field_missing() {
        // Arrange
        let output = "  - Active account: true\n";

        // Act
        let result = parse_auth_field(output, "Token scopes:");

        // Assert
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_run_health_checks_creates_entries() {
        // Arrange
        let db = crate::db::Database::open_in_memory()
            .await
            .expect("failed to open db");
        let pool = db.pool().clone();

        // Act
        let entries = run_health_checks(pool, Some("main".to_string()));

        // Assert — entries are created immediately with Pending status
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock.len(), HealthCheckKind::ALL.len());
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
        // Arrange
        let db = crate::db::Database::open_in_memory()
            .await
            .expect("failed to open db");
        let pool = db.pool().clone();

        // Act
        let entries = run_health_checks(pool, Some("main".to_string()));
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

        // Database should pass
        assert_eq!(lock[0].status, HealthStatus::Pass);
        assert_eq!(lock[0].message, "OK");

        // Git should pass with branch
        assert_eq!(lock[1].status, HealthStatus::Pass);
        assert_eq!(lock[1].message, "Branch: main");
    }
}
