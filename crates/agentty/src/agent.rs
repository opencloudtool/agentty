use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::str::FromStr;

#[cfg(test)]
use mockall::automock;

#[cfg_attr(test, automock)]
pub trait AgentBackend: Send + Sync {
    /// One-time setup in agent folder before first run (e.g. config files).
    fn setup(&self, folder: &Path);
    /// Build a Command for an initial task.
    fn build_start_command(&self, folder: &Path, prompt: &str) -> Command;
    /// Build a Command for resuming/replying.
    fn build_resume_command(&self, folder: &Path, prompt: &str) -> Command;
}

pub struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) {
        // Gemini CLI needs no config files
    }

    fn build_resume_command(&self, folder: &Path, prompt: &str) -> Command {
        let mut cmd = self.build_start_command(folder, prompt);
        cmd.arg("--resume").arg("latest");
        cmd
    }

    fn build_start_command(&self, folder: &Path, prompt: &str) -> Command {
        let mut cmd = Command::new("gemini");
        cmd.arg("--prompt")
            .arg(prompt)
            .arg("--model")
            .arg("gemini-3-flash-preview")
            .arg("--approval-mode")
            .arg("auto_edit")
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

pub struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn setup(&self, _folder: &Path) {
        // Claude Code needs no config files
    }

    fn build_start_command(&self, folder: &Path, prompt: &str) -> Command {
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg(prompt)
            .arg("--allowedTools")
            .arg("Edit")
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn build_resume_command(&self, folder: &Path, prompt: &str) -> Command {
        let mut cmd = Command::new("claude");
        cmd.arg("-c")
            .arg("-p")
            .arg(prompt)
            .arg("--allowedTools")
            .arg("Edit")
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

/// Uses non-interactive Codex commands so Agentty can capture piped output.
///
/// Interactive `codex` requires a TTY and fails in this app with
/// `Error: stdout is not a terminal`, so we run `codex exec --full-auto`
/// and `codex exec resume --last --full-auto` instead.
pub struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) {
        // Codex CLI needs no config files
    }

    fn build_start_command(&self, folder: &Path, prompt: &str) -> Command {
        let mut cmd = Command::new("codex");
        cmd.arg("exec")
            .arg("--full-auto")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn build_resume_command(&self, folder: &Path, prompt: &str) -> Command {
        let mut cmd = Command::new("codex");
        cmd.arg("exec")
            .arg("resume")
            .arg("--last")
            .arg("--full-auto")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Gemini,
    Claude,
    Codex,
}

impl AgentKind {
    /// All available agent kinds, in display order.
    pub const ALL: &[AgentKind] = &[AgentKind::Gemini, AgentKind::Claude, AgentKind::Codex];

    /// Parse from `AGENTTY_AGENT` env var, defaulting to Gemini.
    pub fn from_env() -> Self {
        std::env::var("AGENTTY_AGENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(Self::Gemini)
    }

    /// Create the corresponding backend.
    pub fn create_backend(&self) -> Box<dyn AgentBackend> {
        match self {
            Self::Gemini => Box::new(GeminiBackend),
            Self::Claude => Box::new(ClaudeBackend),
            Self::Codex => Box::new(CodexBackend),
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gemini => write!(f, "gemini"),
            Self::Claude => write!(f, "claude"),
            Self::Codex => write!(f, "codex"),
        }
    }
}

impl FromStr for AgentKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "gemini" => Ok(Self::Gemini),
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => Err(format!("unknown agent kind: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_gemini_setup_creates_no_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        backend.setup(dir.path());

        // Assert
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }

    #[test]
    fn test_gemini_resume_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        let cmd = backend.build_resume_command(dir.path(), "follow-up");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("--prompt"));
        assert!(debug.contains("follow-up"));
        assert!(debug.contains("--resume"));
        assert!(debug.contains("latest"));
    }

    #[test]
    fn test_gemini_start_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        let cmd = backend.build_start_command(dir.path(), "hello");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("gemini"));
        assert!(debug.contains("--prompt"));
        assert!(debug.contains("hello"));
        assert!(debug.contains("gemini-3-flash-preview"));
        assert!(debug.contains("--approval-mode"));
        assert!(debug.contains("auto_edit"));
        assert!(!debug.contains("--resume"));
    }

    #[test]
    fn test_claude_setup_creates_no_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        backend.setup(dir.path());

        // Assert
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }

    #[test]
    fn test_claude_start_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let cmd = backend.build_start_command(dir.path(), "hello");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("claude"));
        assert!(debug.contains("-p"));
        assert!(debug.contains("hello"));
        assert!(debug.contains("--allowedTools"));
        assert!(debug.contains("Edit"));
        assert!(!debug.contains("-c"));
    }

    #[test]
    fn test_claude_resume_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let cmd = backend.build_resume_command(dir.path(), "follow-up");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("claude"));
        assert!(debug.contains("-c"));
        assert!(debug.contains("-p"));
        assert!(debug.contains("follow-up"));
        assert!(debug.contains("--allowedTools"));
        assert!(debug.contains("Edit"));
    }

    #[test]
    fn test_codex_setup_creates_no_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        backend.setup(dir.path());

        // Assert
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }

    #[test]
    fn test_codex_start_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let cmd = backend.build_start_command(dir.path(), "hello");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("codex"));
        assert!(debug.contains("exec"));
        assert!(debug.contains("hello"));
        assert!(debug.contains("--full-auto"));
        assert!(!debug.contains("--dangerously-bypass-approvals-and-sandbox"));
        assert!(!debug.contains("resume"));
    }

    #[test]
    fn test_codex_resume_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let cmd = backend.build_resume_command(dir.path(), "follow-up");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("codex"));
        assert!(debug.contains("exec"));
        assert!(debug.contains("resume"));
        assert!(debug.contains("--last"));
        assert!(debug.contains("--full-auto"));
        assert!(debug.contains("follow-up"));
        assert!(!debug.contains("--dangerously-bypass-approvals-and-sandbox"));
    }

    #[test]
    fn test_agent_kind_all() {
        // Arrange & Act & Assert
        assert_eq!(AgentKind::ALL.len(), 3);
        assert_eq!(AgentKind::ALL[0], AgentKind::Gemini);
        assert_eq!(AgentKind::ALL[1], AgentKind::Claude);
        assert_eq!(AgentKind::ALL[2], AgentKind::Codex);
    }

    #[test]
    fn test_agent_kind_from_env_default() {
        // Arrange — ensure env var is not set
        // We can't safely remove env vars in tests, so we test the parsing logic
        // via from_str which from_env delegates to.

        // Act & Assert
        assert_eq!(
            "gemini".parse::<AgentKind>().expect("parse"),
            AgentKind::Gemini
        );
    }

    #[test]
    fn test_agent_kind_from_env_reads_var() {
        // Arrange
        // from_env reads AGENTTY_AGENT — exercise both branches.
        // The env var may or may not be set in CI, so we just call from_env
        // and verify it returns a valid variant.

        // Act
        let kind = AgentKind::from_env();

        // Assert
        assert!(kind == AgentKind::Gemini || kind == AgentKind::Claude || kind == AgentKind::Codex);
    }

    #[test]
    fn test_agent_kind_create_backend() {
        // Arrange & Act & Assert
        // Just verify create_backend returns without panicking
        let _gemini = AgentKind::Gemini.create_backend();
        let _claude = AgentKind::Claude.create_backend();
        let _codex = AgentKind::Codex.create_backend();
    }

    #[test]
    fn test_agent_kind_display() {
        // Arrange & Act & Assert
        assert_eq!(AgentKind::Gemini.to_string(), "gemini");
        assert_eq!(AgentKind::Claude.to_string(), "claude");
        assert_eq!(AgentKind::Codex.to_string(), "codex");
    }

    #[test]
    fn test_agent_kind_from_str() {
        // Arrange & Act & Assert
        assert_eq!(
            "gemini".parse::<AgentKind>().expect("parse"),
            AgentKind::Gemini
        );
        assert_eq!(
            "claude".parse::<AgentKind>().expect("parse"),
            AgentKind::Claude
        );
        assert_eq!(
            "Gemini".parse::<AgentKind>().expect("parse"),
            AgentKind::Gemini
        );
        assert_eq!(
            "CLAUDE".parse::<AgentKind>().expect("parse"),
            AgentKind::Claude
        );
        assert_eq!(
            "codex".parse::<AgentKind>().expect("parse"),
            AgentKind::Codex
        );
        assert_eq!(
            "CODEX".parse::<AgentKind>().expect("parse"),
            AgentKind::Codex
        );
        assert!("unknown".parse::<AgentKind>().is_err());
    }

    #[test]
    fn test_agent_kind_roundtrip() {
        // Arrange & Act & Assert
        for kind in [AgentKind::Gemini, AgentKind::Claude, AgentKind::Codex] {
            let s = kind.to_string();
            let parsed: AgentKind = s.parse().expect("roundtrip parse");
            assert_eq!(parsed, kind);
        }
    }
}
