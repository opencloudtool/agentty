use std::error::Error;
use std::fmt;
use std::path::Path;
use std::process::Command;

use ag_protocol::TurnPromptAttachment;

use crate::channel::AgentRequestKind;
use crate::model::agent::ReasoningLevel;
use crate::model::permission::PermissionMode;
use crate::model::session::SpeedMode;

/// Maximum concurrent subagents requested from providers with a native limit.
///
/// Excludes the parent agent. Provider exceptions still apply; this is not a
/// host-wide CPU or memory limit.
pub(super) const MAX_CONCURRENT_SUBAGENTS: usize = 2;

/// Transport runtime used to execute turns for one backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTransport {
    /// Provider runs through a managed session runtime.
    AppServer,
    /// Provider runs as direct CLI subprocess commands.
    Cli,
}

impl AgentTransport {
    /// Returns whether this transport uses managed runtime sessions.
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

/// Managed-runtime thought-stream classification policy for one provider.
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
    /// Main repository checkout that must remain read-only during the turn,
    /// when Agentty can resolve it.
    pub main_checkout_root: Option<&'a Path>,
    /// Provider-specific model identifier.
    pub model: &'a str,
    /// Filesystem and command permission policy for this turn.
    pub permission_mode: PermissionMode,
    /// Current personality body included during a full prompt bootstrap.
    pub personality_prompt: Option<&'a str>,
    /// User prompt to send.
    pub prompt: &'a str,
    /// Reasoning effort preference for this turn.
    ///
    /// Ignored by backends/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
    /// Canonical request kind that drives execution and protocol semantics.
    pub request_kind: &'a AgentRequestKind,
    /// Replayable transcript text captured when the turn was queued.
    pub replay_transcript: Option<&'a str>,
    /// Response-speed preference for this turn.
    pub speed_mode: SpeedMode,
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
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// Performs one-time setup in an agent folder before first run.
    ///
    /// # Errors
    /// Returns an error when one-time backend setup cannot be completed.
    fn setup(&self, folder: &Path) -> Result<(), AgentBackendError>;

    /// Builds one provider transport command.
    ///
    /// CLI-backed providers return the per-turn subprocess command. Managed
    /// providers return the long-lived runtime command that owns later turn
    /// execution over its native protocol.
    ///
    /// # Errors
    /// Returns an error when prompt rendering or provider argument
    /// construction fails.
    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_transport_app_server_uses_app_server() {
        // Arrange
        let transport = AgentTransport::AppServer;

        // Act
        let result = transport.uses_app_server();

        // Assert
        assert!(result);
    }

    #[test]
    fn test_agent_transport_cli_does_not_use_app_server() {
        // Arrange
        let transport = AgentTransport::Cli;

        // Act
        let result = transport.uses_app_server();

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_agent_backend_error_setup_displays_message() {
        // Arrange
        let error = AgentBackendError::Setup("setup failed".to_string());

        // Act
        let display = format!("{error}");

        // Assert
        assert_eq!(display, "setup failed");
    }

    #[test]
    fn test_agent_backend_error_command_build_displays_message() {
        // Arrange
        let error = AgentBackendError::CommandBuild("build failed".to_string());

        // Act
        let display = format!("{error}");

        // Assert
        assert_eq!(display, "build failed");
    }

    #[test]
    fn test_agent_backend_error_implements_std_error() {
        // Arrange
        let error = AgentBackendError::Setup("test error".to_string());

        // Act
        let std_error: &dyn Error = &error;

        // Assert
        assert_eq!(std_error.to_string(), "test error");
        assert!(std_error.source().is_none());
    }

    #[test]
    fn test_agent_backend_error_setup_and_command_build_are_distinct() {
        // Arrange
        let setup_error = AgentBackendError::Setup("failure".to_string());
        let build_error = AgentBackendError::CommandBuild("failure".to_string());

        // Act / Assert
        assert_ne!(setup_error, build_error);
    }
}
