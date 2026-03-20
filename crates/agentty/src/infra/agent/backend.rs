use std::error::Error;
use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::domain::agent::ReasoningLevel;
use crate::infra::channel::{AgentRequestKind, TurnPromptAttachment};

/// Transport runtime used to execute turns for one backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTransport {
    /// Provider runs through persistent app-server sessions.
    AppServer,
    /// Provider runs as direct CLI subprocess commands.
    Cli,
}

impl AgentTransport {
    /// Returns whether this transport uses app-server sessions.
    pub fn uses_app_server(self) -> bool {
        matches!(self, Self::AppServer)
    }
}

/// Prompt delivery mode used by one provider backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentPromptTransport {
    /// Prompt is passed inline through argv.
    Argv,
    /// Prompt is streamed through stdin.
    Stdin,
}

/// App-server thought-stream classification policy for one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppServerThoughtPolicy {
    /// Provider does not expose dedicated thought phases.
    None,
    /// Provider uses phase labels to distinguish thought chunks.
    PhaseLabel,
}

/// Request payload used to build provider transport commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCommandRequest<'a> {
    /// Ordered local image attachments referenced from the prompt body.
    pub attachments: &'a [TurnPromptAttachment],
    /// Working directory where the command will run.
    pub folder: &'a Path,
    /// User prompt to send.
    pub prompt: &'a str,
    /// Canonical request kind that drives execution and protocol semantics.
    pub request_kind: &'a AgentRequestKind,
    /// Provider-specific model identifier.
    pub model: &'a str,
    /// Reasoning effort preference for this turn.
    ///
    /// Ignored by backends/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
}

/// Error type for backend setup and command construction failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentBackendError {
    /// One-time backend setup failure.
    Setup(String),
    /// Per-command build failure.
    CommandBuild(String),
}

impl fmt::Display for AgentBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(message) | Self::CommandBuild(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl Error for AgentBackendError {}

/// Builds and configures external agent CLI commands.
#[cfg_attr(test, mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// Performs one-time setup in an agent folder before first run.
    ///
    /// # Errors
    /// Returns an error when one-time backend setup cannot be completed.
    fn setup(&self, folder: &Path) -> Result<(), AgentBackendError>;

    /// Builds one provider transport command.
    ///
    /// CLI-backed providers return the per-turn subprocess command. App-server
    /// providers return the long-lived runtime command that owns later RPC
    /// turn execution.
    ///
    /// # Errors
    /// Returns an error when prompt rendering or provider argument
    /// construction fails.
    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError>;
}
