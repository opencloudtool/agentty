use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HealthCheckKind {
    AgentClaude,
    AgentGemini,
    Database,
    GitRepo,
}

impl HealthCheckKind {
    pub const ALL: &[HealthCheckKind] = &[
        HealthCheckKind::Database,
        HealthCheckKind::GitRepo,
        HealthCheckKind::AgentClaude,
        HealthCheckKind::AgentGemini,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HealthCheckKind::AgentClaude => "Claude CLI",
            HealthCheckKind::AgentGemini => "Gemini CLI",
            HealthCheckKind::Database => "Database",
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
    pub kind: HealthCheckKind,
    pub message: String,
    pub status: HealthStatus,
}

pub fn run_health_checks(
    pool: SqlitePool,
    git_branch: Option<String>,
) -> Arc<Mutex<Vec<HealthEntry>>> {
    let entries: Vec<HealthEntry> = HealthCheckKind::ALL
        .iter()
        .map(|&kind| HealthEntry {
            kind,
            message: String::new(),
            status: HealthStatus::Pending,
        })
        .collect();

    let shared = Arc::new(Mutex::new(entries));
    let shared_bg = Arc::clone(&shared);

    tokio::spawn(async move {
        for (index, &kind) in HealthCheckKind::ALL.iter().enumerate() {
            update_entry(&shared_bg, index, HealthStatus::Running, "Checking...");
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;

            let (status, message) = match kind {
                HealthCheckKind::Database => check_database(&pool).await,
                HealthCheckKind::GitRepo => check_git_repo(git_branch.as_ref()),
                HealthCheckKind::AgentClaude => check_cli_tool("claude").await,
                HealthCheckKind::AgentGemini => check_cli_tool("gemini").await,
            };

            update_entry(&shared_bg, index, status, &message);
        }
    });

    shared
}

fn update_entry(
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

async fn check_cli_tool(command: &str) -> (HealthStatus, String) {
    match tokio::process::Command::new(command)
        .arg("--version")
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (HealthStatus::Pass, version)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            (HealthStatus::Warn, format!("Exit code error: {stderr}"))
        }
        Err(_) => (HealthStatus::Warn, "Not found in PATH".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_check_kind_all_count() {
        // Arrange & Act & Assert
        assert_eq!(HealthCheckKind::ALL.len(), 4);
    }

    #[test]
    fn test_health_check_kind_label() {
        // Arrange & Act & Assert
        assert_eq!(HealthCheckKind::Database.label(), "Database");
        assert_eq!(HealthCheckKind::GitRepo.label(), "Git Repository");
        assert_eq!(HealthCheckKind::AgentClaude.label(), "Claude CLI");
        assert_eq!(HealthCheckKind::AgentGemini.label(), "Gemini CLI");
    }

    #[test]
    fn test_health_entry_defaults() {
        // Arrange & Act
        let entry = HealthEntry {
            kind: HealthCheckKind::Database,
            message: String::new(),
            status: HealthStatus::Pending,
        };

        // Assert
        assert_eq!(entry.kind, HealthCheckKind::Database);
        assert_eq!(entry.status, HealthStatus::Pending);
        assert!(entry.message.is_empty());
    }

    #[test]
    fn test_update_entry() {
        // Arrange
        let entries = Arc::new(Mutex::new(vec![HealthEntry {
            kind: HealthCheckKind::Database,
            message: String::new(),
            status: HealthStatus::Pending,
        }]));

        // Act
        update_entry(&entries, 0, HealthStatus::Pass, "OK");

        // Assert
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock[0].status, HealthStatus::Pass);
        assert_eq!(lock[0].message, "OK");
    }

    #[test]
    fn test_update_entry_out_of_bounds() {
        // Arrange
        let entries = Arc::new(Mutex::new(vec![HealthEntry {
            kind: HealthCheckKind::Database,
            message: String::new(),
            status: HealthStatus::Pending,
        }]));

        // Act — should not panic
        update_entry(&entries, 99, HealthStatus::Fail, "Error");

        // Assert — original entry unchanged
        let lock = entries.lock().expect("failed to lock");
        assert_eq!(lock[0].status, HealthStatus::Pending);
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
    async fn test_check_cli_tool_nonexistent() {
        // Arrange & Act
        let (status, message) = check_cli_tool("nonexistent_tool_xyz_123").await;

        // Assert
        assert_eq!(status, HealthStatus::Warn);
        assert_eq!(message, "Not found in PATH");
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
                entry.kind,
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
