//! Forge CLI command boundary used by review-request adapters.

use std::io::ErrorKind;

use tokio::process::Command;

use super::ForgeFuture;

/// One forge CLI invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgeCommand {
    /// Environment variables applied to the spawned process.
    pub(crate) environment: Vec<(String, String)>,
    /// Executable name passed to the OS process launcher.
    pub(crate) executable: &'static str,
    /// Argument vector passed to the executable.
    pub(crate) arguments: Vec<String>,
}

impl ForgeCommand {
    /// Builds one forge CLI command with no extra environment.
    pub(crate) fn new(executable: &'static str, arguments: Vec<String>) -> Self {
        Self {
            environment: Vec::new(),
            executable,
            arguments,
        }
    }

    /// Adds one environment variable to the command.
    pub(crate) fn with_environment(mut self, key: &str, value: impl Into<String>) -> Self {
        self.environment.push((key.to_string(), value.into()));

        self
    }
}

/// Raw process output captured from one forge CLI invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgeCommandOutput {
    /// Process exit code, or `None` when the process terminated without one.
    pub(crate) exit_code: Option<i32>,
    /// Captured standard error text.
    pub(crate) stderr: String,
    /// Captured standard output text.
    pub(crate) stdout: String,
}

impl ForgeCommandOutput {
    /// Returns whether the command exited successfully.
    pub(crate) fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Spawn-time failures before a forge CLI command can complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ForgeCommandError {
    /// The requested executable was not found on the local machine.
    ExecutableNotFound { executable: String },
    /// The process could not be started for another reason.
    SpawnFailed {
        /// Executable name that failed to spawn.
        executable: String,
        /// Human-readable spawn error detail.
        message: String,
    },
}

/// Async command boundary used by forge adapters.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait ForgeCommandRunner: Send + Sync {
    /// Runs one forge CLI command and returns the captured output.
    fn run(
        &self,
        command: ForgeCommand,
    ) -> ForgeFuture<Result<ForgeCommandOutput, ForgeCommandError>>;
}

/// Production [`ForgeCommandRunner`] backed by `tokio::process::Command`.
pub(crate) struct RealForgeCommandRunner;

impl ForgeCommandRunner for RealForgeCommandRunner {
    fn run(
        &self,
        command: ForgeCommand,
    ) -> ForgeFuture<Result<ForgeCommandOutput, ForgeCommandError>> {
        Box::pin(async move { run_command(command).await })
    }
}

/// Runs one forge CLI command and captures stdout, stderr, and exit status.
async fn run_command(command: ForgeCommand) -> Result<ForgeCommandOutput, ForgeCommandError> {
    let mut process = Command::new(command.executable);
    process.args(&command.arguments);

    for (key, value) in &command.environment {
        process.env(key, value);
    }

    let output = process.output().await.map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            return ForgeCommandError::ExecutableNotFound {
                executable: command.executable.to_string(),
            };
        }

        ForgeCommandError::SpawnFailed {
            executable: command.executable.to_string(),
            message: error.to_string(),
        }
    })?;

    Ok(ForgeCommandOutput {
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// Extracts the best human-readable error detail from command output.
pub(crate) fn command_output_detail(output: &ForgeCommandOutput) -> String {
    let stderr_text = output.stderr.trim();
    if !stderr_text.is_empty() {
        return stderr_text.to_string();
    }

    let stdout_text = output.stdout.trim();
    if !stdout_text.is_empty() {
        return stdout_text.to_string();
    }

    "Unknown forge CLI error".to_string()
}
