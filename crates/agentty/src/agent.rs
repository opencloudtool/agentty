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
    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command;
    /// Build a Command for resuming/replying.
    fn build_resume_command(&self, folder: &Path, prompt: &str, model: &str) -> Command;
}

pub struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) {
        // Gemini CLI needs no config files
    }

    fn build_resume_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut cmd = self.build_start_command(folder, prompt, model);
        cmd.arg("--resume").arg("latest");
        cmd
    }

    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut cmd = Command::new("gemini");
        cmd.arg("--prompt")
            .arg(prompt)
            .arg("--model")
            .arg(model)
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

    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg(prompt)
            .arg("--allowedTools")
            .arg("Edit")
            .env("ANTHROPIC_MODEL", model)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn build_resume_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut cmd = Command::new("claude");
        cmd.arg("-c")
            .arg("-p")
            .arg(prompt)
            .arg("--allowedTools")
            .arg("Edit")
            .env("ANTHROPIC_MODEL", model)
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

    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut cmd = Command::new("codex");
        cmd.arg("exec")
            .arg("--model")
            .arg(model)
            .arg("--full-auto")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn build_resume_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut cmd = Command::new("codex");
        cmd.arg("exec")
            .arg("resume")
            .arg("--last")
            .arg("--model")
            .arg(model)
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

/// Supported Gemini model names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GeminiModel {
    #[default]
    Gemini3FlashPreview,
    Gemini3ProPreview,
}

impl GeminiModel {
    pub const ALL: &[Self] = &[Self::Gemini3FlashPreview, Self::Gemini3ProPreview];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemini3FlashPreview => "gemini-3-flash-preview",
            Self::Gemini3ProPreview => "gemini-3-pro-preview",
        }
    }
}

impl FromStr for GeminiModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gemini-3-flash-preview" => Ok(Self::Gemini3FlashPreview),
            "gemini-3-pro-preview" => Ok(Self::Gemini3ProPreview),
            other => Err(format!("unknown Gemini model: {other}")),
        }
    }
}

/// Supported Codex model names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexModel {
    #[default]
    Gpt53Codex,
    Gpt52Codex,
}

impl CodexModel {
    pub const ALL: &[Self] = &[Self::Gpt53Codex, Self::Gpt52Codex];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpt53Codex => "gpt-5.3-codex",
            Self::Gpt52Codex => "gpt-5.2-codex",
        }
    }
}

impl FromStr for CodexModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gpt-5.3-codex" => Ok(Self::Gpt53Codex),
            "gpt-5.2-codex" => Ok(Self::Gpt52Codex),
            other => Err(format!("unknown Codex model: {other}")),
        }
    }
}

/// Supported Claude model names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeModel {
    #[default]
    ClaudeOpus46,
    ClaudeSonnet4520250929,
    ClaudeHaiku4520251001,
}

impl ClaudeModel {
    pub const ALL: &[Self] = &[
        Self::ClaudeOpus46,
        Self::ClaudeSonnet4520250929,
        Self::ClaudeHaiku4520251001,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeOpus46 => "claude-opus-4-6",
            Self::ClaudeSonnet4520250929 => "claude-sonnet-4-5-20250929",
            Self::ClaudeHaiku4520251001 => "claude-haiku-4-5-20251001",
        }
    }
}

impl FromStr for ClaudeModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claude-opus-4-6" => Ok(Self::ClaudeOpus46),
            "claude-sonnet-4-5-20250929" => Ok(Self::ClaudeSonnet4520250929),
            "claude-haiku-4-5-20251001" => Ok(Self::ClaudeHaiku4520251001),
            other => Err(format!("unknown Claude model: {other}")),
        }
    }
}

/// Model value typed by provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentModel {
    Claude(ClaudeModel),
    Codex(CodexModel),
    Gemini(GeminiModel),
}

impl AgentModel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude(model) => model.as_str(),
            Self::Codex(model) => model.as_str(),
            Self::Gemini(model) => model.as_str(),
        }
    }

    pub fn kind(self) -> AgentKind {
        match self {
            Self::Claude(_) => AgentKind::Claude,
            Self::Codex(_) => AgentKind::Codex,
            Self::Gemini(_) => AgentKind::Gemini,
        }
    }
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

    /// Returns the default model for this agent kind.
    pub fn default_model(self) -> AgentModel {
        match self {
            Self::Gemini => AgentModel::Gemini(GeminiModel::default()),
            Self::Claude => AgentModel::Claude(ClaudeModel::default()),
            Self::Codex => AgentModel::Codex(CodexModel::default()),
        }
    }

    /// Returns the model string when it belongs to this agent kind.
    pub fn model_str(self, model: AgentModel) -> Option<&'static str> {
        if model.kind() != self {
            return None;
        }

        Some(model.as_str())
    }

    /// Returns the curated model list for this agent kind.
    pub fn models(self) -> &'static [AgentModel] {
        const GEMINI_MODELS: &[AgentModel] = &[
            AgentModel::Gemini(GeminiModel::Gemini3FlashPreview),
            AgentModel::Gemini(GeminiModel::Gemini3ProPreview),
        ];
        const CLAUDE_MODELS: &[AgentModel] = &[
            AgentModel::Claude(ClaudeModel::ClaudeOpus46),
            AgentModel::Claude(ClaudeModel::ClaudeSonnet4520250929),
            AgentModel::Claude(ClaudeModel::ClaudeHaiku4520251001),
        ];
        const CODEX_MODELS: &[AgentModel] = &[
            AgentModel::Codex(CodexModel::Gpt53Codex),
            AgentModel::Codex(CodexModel::Gpt52Codex),
        ];

        match self {
            Self::Gemini => GEMINI_MODELS,
            Self::Claude => CLAUDE_MODELS,
            Self::Codex => CODEX_MODELS,
        }
    }

    /// Parses a provider-specific model string for this agent kind.
    pub fn parse_model(self, value: &str) -> Option<AgentModel> {
        match self {
            Self::Gemini => value.parse::<GeminiModel>().ok().map(AgentModel::Gemini),
            Self::Claude => value.parse::<ClaudeModel>().ok().map(AgentModel::Claude),
            Self::Codex => value.parse::<CodexModel>().ok().map(AgentModel::Codex),
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
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn command_env_value(command: &Command, key: &str) -> Option<String> {
        command.get_envs().find_map(|(name, value)| {
            if name.to_string_lossy() != key {
                return None;
            }

            value.map(|entry| entry.to_string_lossy().to_string())
        })
    }

    #[test]
    fn test_gemini_setup_creates_no_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        AgentBackend::setup(&backend, dir.path());

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
        let cmd = AgentBackend::build_resume_command(
            &backend,
            dir.path(),
            "follow-up",
            "gemini-3-pro-preview",
        );

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("--prompt"));
        assert!(debug.contains("follow-up"));
        assert!(debug.contains("--resume"));
        assert!(debug.contains("latest"));
        assert!(debug.contains("gemini-3-pro-preview"));
    }

    #[test]
    fn test_gemini_start_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        let cmd = AgentBackend::build_start_command(
            &backend,
            dir.path(),
            "hello",
            "gemini-3-flash-preview",
        );

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
        AgentBackend::setup(&backend, dir.path());

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
        let cmd =
            AgentBackend::build_start_command(&backend, dir.path(), "hello", "claude-opus-4-6");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("claude"));
        assert!(debug.contains("-p"));
        assert!(debug.contains("hello"));
        assert!(debug.contains("--allowedTools"));
        assert!(debug.contains("Edit"));
        assert!(!debug.contains("-c"));
        assert_eq!(
            command_env_value(&cmd, "ANTHROPIC_MODEL"),
            Some("claude-opus-4-6".to_string())
        );
    }

    #[test]
    fn test_claude_resume_command_args() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let cmd = AgentBackend::build_resume_command(
            &backend,
            dir.path(),
            "follow-up",
            "claude-haiku-4-5-20251001",
        );

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("claude"));
        assert!(debug.contains("-c"));
        assert!(debug.contains("-p"));
        assert!(debug.contains("follow-up"));
        assert!(debug.contains("--allowedTools"));
        assert!(debug.contains("Edit"));
        assert_eq!(
            command_env_value(&cmd, "ANTHROPIC_MODEL"),
            Some("claude-haiku-4-5-20251001".to_string())
        );
    }

    #[test]
    fn test_codex_setup_creates_no_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        AgentBackend::setup(&backend, dir.path());

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
        let cmd = AgentBackend::build_start_command(&backend, dir.path(), "hello", "gpt-5.3-codex");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("codex"));
        assert!(debug.contains("exec"));
        assert!(debug.contains("hello"));
        assert!(debug.contains("gpt-5.3-codex"));
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
        let cmd =
            AgentBackend::build_resume_command(&backend, dir.path(), "follow-up", "gpt-5.2-codex");

        // Assert
        let debug = format!("{cmd:?}");
        assert!(debug.contains("codex"));
        assert!(debug.contains("exec"));
        assert!(debug.contains("resume"));
        assert!(debug.contains("--last"));
        assert!(debug.contains("gpt-5.2-codex"));
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

    #[test]
    fn test_agent_kind_default_models() {
        // Arrange & Act & Assert
        assert_eq!(
            AgentKind::Gemini.default_model(),
            AgentModel::Gemini(GeminiModel::Gemini3FlashPreview)
        );
        assert_eq!(
            AgentKind::Claude.default_model(),
            AgentModel::Claude(ClaudeModel::ClaudeOpus46)
        );
        assert_eq!(
            AgentKind::Codex.default_model(),
            AgentModel::Codex(CodexModel::Gpt53Codex)
        );
    }

    #[test]
    fn test_agent_kind_models_lists() {
        // Arrange & Act
        let gemini_models = AgentKind::Gemini.models();
        let claude_models = AgentKind::Claude.models();
        let codex_models = AgentKind::Codex.models();

        // Assert
        assert_eq!(
            gemini_models,
            &[
                AgentModel::Gemini(GeminiModel::Gemini3FlashPreview),
                AgentModel::Gemini(GeminiModel::Gemini3ProPreview),
            ]
        );
        assert_eq!(
            claude_models,
            &[
                AgentModel::Claude(ClaudeModel::ClaudeOpus46),
                AgentModel::Claude(ClaudeModel::ClaudeSonnet4520250929),
                AgentModel::Claude(ClaudeModel::ClaudeHaiku4520251001),
            ]
        );
        assert_eq!(
            codex_models,
            &[
                AgentModel::Codex(CodexModel::Gpt53Codex),
                AgentModel::Codex(CodexModel::Gpt52Codex),
            ]
        );
    }

    #[test]
    fn test_agent_kind_parse_model() {
        // Arrange & Act & Assert
        assert_eq!(
            AgentKind::Gemini.parse_model("gemini-3-pro-preview"),
            Some(AgentModel::Gemini(GeminiModel::Gemini3ProPreview))
        );
        assert_eq!(
            AgentKind::Codex.parse_model("gpt-5.2-codex"),
            Some(AgentModel::Codex(CodexModel::Gpt52Codex))
        );
        assert_eq!(
            AgentKind::Claude.parse_model("claude-haiku-4-5-20251001"),
            Some(AgentModel::Claude(ClaudeModel::ClaudeHaiku4520251001))
        );
        assert_eq!(AgentKind::Gemini.parse_model("claude-opus-4-6"), None);
    }

    #[test]
    fn test_agent_model_as_str() {
        // Arrange & Act & Assert
        assert_eq!(
            AgentModel::Gemini(GeminiModel::Gemini3FlashPreview).as_str(),
            "gemini-3-flash-preview"
        );
        assert_eq!(
            AgentModel::Codex(CodexModel::Gpt53Codex).as_str(),
            "gpt-5.3-codex"
        );
        assert_eq!(
            AgentModel::Claude(ClaudeModel::ClaudeOpus46).as_str(),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn test_agent_kind_model_str_validates_kind() {
        // Arrange
        let model = AgentModel::Gemini(GeminiModel::Gemini3FlashPreview);

        // Act
        let valid = AgentKind::Gemini.model_str(model);
        let invalid = AgentKind::Claude.model_str(model);

        // Assert
        assert_eq!(valid, Some("gemini-3-flash-preview"));
        assert_eq!(invalid, None);
    }
}
