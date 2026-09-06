//! Shared raw subprocess execution for CLI-backed agent transports.

use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _};

use super::stdin;
use crate::agent::{self as agent, AgentBackend, AgentBackendError, BuildCommandRequest};
use crate::model::agent::AgentKind;

/// Observer for execution details that are meaningful to a transport adapter.
pub(crate) trait CliExecutionObserver: Send + Sync {
    /// Reports the active child PID and clears it after process termination.
    fn pid_updated(&self, child_pid: Option<u32>);

    /// Reports one complete stdout line while retaining the same line in the
    /// final raw output.
    fn stdout_line(&self, line: &str);
}

/// Observer used by callers that only need the collected raw output.
pub(crate) struct CollectingCliObserver;

impl CliExecutionObserver for CollectingCliObserver {
    fn pid_updated(&self, _child_pid: Option<u32>) {}

    fn stdout_line(&self, _line: &str) {}
}

/// Classified process termination state independent of caller policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CliExitStatus {
    /// Process exited unsuccessfully without a Unix signal.
    NonZero(Option<i32>),
    /// Process terminated after receiving a Unix signal.
    Signaled(i32),
    /// Process exited successfully.
    Success,
}

/// Raw subprocess output returned before provider-specific parsing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CliExecutionOutput {
    /// Classified process termination state.
    pub exit_status: CliExitStatus,
    /// Complete stderr captured from the child process.
    pub stderr: String,
    /// Complete stdout captured from the child process.
    pub stdout: String,
}

/// Infrastructure failure while preparing or running a CLI subprocess.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CliExecutionError {
    /// Provider backend could not construct the command.
    #[error("command build failed: {0}")]
    CommandBuild(AgentBackendError),
    /// A configured child pipe was unexpectedly unavailable.
    #[error("{0} pipe unavailable after spawn")]
    PipeUnavailable(&'static str),
    /// Child process could not be spawned.
    #[error("process spawn failed: {0}")]
    Spawn(io::Error),
    /// Stderr capture failed.
    #[error("stderr capture failed: {0}")]
    StderrRead(io::Error),
    /// Provider prompt could not be rendered for stdin delivery.
    #[error("stdin payload build failed: {0}")]
    StdinBuild(AgentBackendError),
    /// Stdin delivery failed after the child was spawned.
    #[error("stdin delivery failed: {0}")]
    StdinWrite(String),
    /// Stdout capture failed.
    #[error("stdout capture failed: {0}")]
    StdoutRead(io::Error),
    /// Child execution exceeded the caller-provided deadline.
    #[error("process timed out after {}s", .0.as_secs())]
    Timeout(Duration),
    /// Waiting for child termination failed.
    #[error("process wait failed: {0}")]
    Wait(io::Error),
}

/// Builds and executes one CLI backend command, returning unparsed output.
///
/// Command construction, prompt stdin rendering, pipe setup, concurrent stream
/// capture, PID lifetime, timeout enforcement, and exit classification all
/// live here. Callers remain responsible for interpreting output and applying
/// response-repair policy.
///
/// # Errors
/// Returns [`CliExecutionError`] when command preparation or subprocess I/O
/// fails, or when `timeout` expires.
pub(crate) async fn execute_cli_command(
    backend: &dyn AgentBackend,
    kind: AgentKind,
    build_request: BuildCommandRequest<'_>,
    observer: &dyn CliExecutionObserver,
    timeout: Option<Duration>,
) -> Result<CliExecutionOutput, CliExecutionError> {
    let command = backend
        .build_command(build_request)
        .map_err(CliExecutionError::CommandBuild)?;
    let stdin_payload = agent::build_command_stdin_payload(kind, build_request)
        .map_err(CliExecutionError::StdinBuild)?;
    let mut tokio_command = tokio::process::Command::from(command);
    // Agent tools must not refresh Git's index during read-only inspection.
    tokio_command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(if stdin_payload.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = tokio_command.spawn().map_err(CliExecutionError::Spawn)?;
    let _pid_guard = ChildPidObserverGuard::new(observer, child.id());
    let stdout = require_pipe(child.stdout.take(), "stdout")?;
    let stderr = require_pipe(child.stderr.take(), "stderr")?;
    let mut stdin_write_task = stdin::spawn_optional_stdin_write(
        child.stdin.take(),
        stdin_payload,
        "stdin pipe unavailable after spawn",
        CliExecutionError::StdinWrite,
    );

    let execution = async {
        let wait = async { child.wait().await.map_err(CliExecutionError::Wait) };
        let stdout_capture = capture_stdout(tokio::io::BufReader::new(stdout), observer);
        let stderr_capture = capture_stderr(stderr);
        let (exit_status, stdout, stderr) = tokio::try_join!(wait, stdout_capture, stderr_capture)?;

        Ok((exit_status, stdout, stderr))
    };
    let execution_result = match timeout {
        Some(timeout) => {
            let Ok(execution_result) = tokio::time::timeout(timeout, execution).await else {
                abort_stdin_write_task(&mut stdin_write_task).await;

                return Err(CliExecutionError::Timeout(timeout));
            };

            execution_result
        }
        None => execution.await,
    };
    let (exit_status, stdout, stderr) =
        finish_cli_execution(stdin_write_task, execution_result).await?;

    Ok(CliExecutionOutput {
        exit_status: classify_exit_status(exit_status),
        stderr,
        stdout,
    })
}

/// Finalizes stdin delivery without leaving its task detached after failure.
async fn finish_cli_execution(
    mut stdin_write_task: Option<tokio::task::JoinHandle<Result<(), CliExecutionError>>>,
    execution_result: Result<(ExitStatus, String, String), CliExecutionError>,
) -> Result<(ExitStatus, String, String), CliExecutionError> {
    if execution_result.is_err() {
        abort_stdin_write_task(&mut stdin_write_task).await;
    }

    stdin::await_optional_stdin_write(
        stdin_write_task,
        "stdin write task failed",
        CliExecutionError::StdinWrite,
    )
    .await?;

    execution_result
}

/// Cancels and joins one active stdin writer before an execution error exits.
async fn abort_stdin_write_task(
    stdin_write_task: &mut Option<tokio::task::JoinHandle<Result<(), CliExecutionError>>>,
) {
    let Some(stdin_write_task) = stdin_write_task.take() else {
        return;
    };

    stdin_write_task.abort();
    let _ = stdin_write_task.await;
}

/// Reports a spawned PID and guarantees a matching clear notification.
struct ChildPidObserverGuard<'a> {
    observer: &'a dyn CliExecutionObserver,
}

impl<'a> ChildPidObserverGuard<'a> {
    /// Creates a PID guard and reports the current child identifier.
    fn new(observer: &'a dyn CliExecutionObserver, child_pid: Option<u32>) -> Self {
        observer.pid_updated(child_pid);

        Self { observer }
    }
}

impl Drop for ChildPidObserverGuard<'_> {
    fn drop(&mut self) {
        self.observer.pid_updated(None);
    }
}

/// Returns one required child pipe or a typed infrastructure error.
fn require_pipe<Pipe>(
    pipe: Option<Pipe>,
    pipe_name: &'static str,
) -> Result<Pipe, CliExecutionError> {
    pipe.ok_or(CliExecutionError::PipeUnavailable(pipe_name))
}

/// Captures stdout exactly while reporting completed lines to the observer.
async fn capture_stdout<Reader>(
    mut reader: Reader,
    observer: &dyn CliExecutionObserver,
) -> Result<String, CliExecutionError>
where
    Reader: AsyncBufRead + Unpin,
{
    let mut output = Vec::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(CliExecutionError::StdoutRead)?;
        if bytes_read == 0 {
            break;
        }

        output.extend_from_slice(&line);
        let line_text = String::from_utf8_lossy(&line);
        observer.stdout_line(line_text.trim_end_matches(['\r', '\n']));
    }

    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// Captures the complete stderr byte stream.
async fn capture_stderr<Reader>(mut reader: Reader) -> Result<String, CliExecutionError>
where
    Reader: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    reader
        .read_to_end(&mut output)
        .await
        .map_err(CliExecutionError::StderrRead)?;

    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// Converts an OS exit status into the transport-neutral classification.
fn classify_exit_status(exit_status: ExitStatus) -> CliExitStatus {
    if let Some(signal) = exit_status.signal() {
        return CliExitStatus::Signaled(signal);
    }
    if exit_status.success() {
        return CliExitStatus::Success;
    }

    CliExitStatus::NonZero(exit_status.code())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;
    use std::task::{Context, Poll};

    use ag_protocol::TurnPromptAttachment;
    use tempfile::tempdir;
    use tokio::io::{AsyncRead, ReadBuf};

    use super::*;
    use crate::MockAgentBackend;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::ReasoningLevel;

    /// Observer that records all streaming callbacks for assertions.
    struct RecordingObserver {
        lines: Mutex<Vec<String>>,
        pid_updates: Mutex<Vec<Option<u32>>>,
    }

    impl RecordingObserver {
        /// Creates an empty callback recorder.
        fn new() -> Self {
            Self {
                lines: Mutex::new(Vec::new()),
                pid_updates: Mutex::new(Vec::new()),
            }
        }
    }

    impl CliExecutionObserver for RecordingObserver {
        fn pid_updated(&self, child_pid: Option<u32>) {
            self.pid_updates
                .lock()
                .expect("PID update lock should be available")
                .push(child_pid);
        }

        fn stdout_line(&self, line: &str) {
            self.lines
                .lock()
                .expect("line lock should be available")
                .push(line.to_string());
        }
    }

    /// Async reader that deterministically fails every read.
    struct FailingReader;

    impl AsyncRead for FailingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("read failed")))
        }
    }

    /// Builds one borrowed command request for executor tests.
    fn build_request<'a>(
        attachments: &'a [TurnPromptAttachment],
        folder: &'a Path,
        prompt: &'a str,
        request_kind: &'a AgentRequestKind,
    ) -> BuildCommandRequest<'a> {
        BuildCommandRequest {
            attachments,
            folder,
            main_checkout_root: None,
            model: "test-model",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt,
            reasoning_level: ReasoningLevel::default(),
            request_kind,
            speed_mode: crate::model::session::SpeedMode::default(),
            replay_transcript: None,
        }
    }

    /// Builds a shell command for deterministic subprocess output.
    fn shell_command(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);

        command
    }

    #[tokio::test]
    async fn test_execute_cli_command_returns_command_build_failure() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            Err(AgentBackendError::CommandBuild(
                "command unavailable".to_string(),
            ))
        });

        // Act
        let error = execute_cli_command(
            &backend,
            AgentKind::Codex,
            build_request(&[], folder.path(), "prompt", &request_kind),
            &CollectingCliObserver,
            None,
        )
        .await
        .expect_err("command construction should fail");

        // Assert
        assert!(matches!(
            error,
            CliExecutionError::CommandBuild(AgentBackendError::CommandBuild(message))
                if message == "command unavailable"
        ));
    }

    #[tokio::test]
    async fn test_execute_cli_command_returns_stdin_build_failure() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let attachments = vec![TurnPromptAttachment {
            local_image_path: PathBuf::from(OsString::from_vec(b"/tmp/image-\xff.png".to_vec())),
            placeholder: "[Image #1]".to_string(),
        }];
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .returning(|_| Ok(shell_command("exit 0")));

        // Act
        let error = execute_cli_command(
            &backend,
            AgentKind::Claude,
            build_request(
                &attachments,
                folder.path(),
                "Review [Image #1]",
                &request_kind,
            ),
            &CollectingCliObserver,
            None,
        )
        .await
        .expect_err("stdin rendering should fail");

        // Assert
        assert!(matches!(error, CliExecutionError::StdinBuild(_)));
    }

    #[tokio::test]
    async fn test_execute_cli_command_returns_spawn_failure() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .returning(|_| Ok(Command::new("/no-such-binary-agentty-cli-execution-test")));

        // Act
        let error = execute_cli_command(
            &backend,
            AgentKind::Codex,
            build_request(&[], folder.path(), "prompt", &request_kind),
            &CollectingCliObserver,
            None,
        )
        .await
        .expect_err("process spawning should fail");

        // Assert
        assert!(matches!(error, CliExecutionError::Spawn(_)));
    }

    #[tokio::test]
    async fn test_execute_cli_command_collects_output_and_streams_lines() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().returning(|_| {
            Ok(shell_command(
                "printf 'first\\nsecond'; printf 'warning' >&2",
            ))
        });
        let observer = RecordingObserver::new();

        // Act
        let output = execute_cli_command(
            &backend,
            AgentKind::Codex,
            build_request(&[], folder.path(), "prompt", &request_kind),
            &observer,
            None,
        )
        .await
        .expect("process should complete");

        // Assert
        assert_eq!(output.exit_status, CliExitStatus::Success);
        assert_eq!(output.stdout, "first\nsecond");
        assert_eq!(output.stderr, "warning");
        assert_eq!(
            *observer
                .lines
                .lock()
                .expect("line lock should be available"),
            vec!["first".to_string(), "second".to_string()]
        );
        let pid_updates = observer
            .pid_updates
            .lock()
            .expect("PID update lock should be available");
        assert!(pid_updates.first().is_some_and(Option::is_some));
        assert_eq!(pid_updates.last(), Some(&None));
    }

    #[tokio::test]
    async fn test_execute_cli_command_disables_optional_git_locks_for_tools() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().once().returning(|_| {
            let mut command = shell_command("sh -c 'printf %s \"$GIT_OPTIONAL_LOCKS\"'");
            command.env("GIT_OPTIONAL_LOCKS", "1");

            Ok(command)
        });

        // Act
        let output = execute_cli_command(
            &backend,
            AgentKind::Codex,
            build_request(&[], folder.path(), "prompt", &request_kind),
            &CollectingCliObserver,
            None,
        )
        .await
        .expect("process should complete");

        // Assert
        assert_eq!(output.exit_status, CliExitStatus::Success);
        assert_eq!(output.stdout, "0");
    }

    #[tokio::test]
    async fn test_execute_cli_command_classifies_non_zero_exit() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .returning(|_| Ok(shell_command("exit 7")));

        // Act
        let output = execute_cli_command(
            &backend,
            AgentKind::Codex,
            build_request(&[], folder.path(), "prompt", &request_kind),
            &CollectingCliObserver,
            None,
        )
        .await
        .expect("non-zero termination should still return raw output");

        // Assert
        assert_eq!(output.exit_status, CliExitStatus::NonZero(Some(7)));
    }

    #[tokio::test]
    async fn test_execute_cli_command_classifies_signal_termination() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .returning(|_| Ok(shell_command("kill -9 $$")));

        // Act
        let output = execute_cli_command(
            &backend,
            AgentKind::Codex,
            build_request(&[], folder.path(), "prompt", &request_kind),
            &CollectingCliObserver,
            None,
        )
        .await
        .expect("signal termination should still return raw output");

        // Assert
        assert_eq!(output.exit_status, CliExitStatus::Signaled(9));
    }

    #[tokio::test]
    async fn test_execute_cli_command_enforces_timeout() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .returning(|_| Ok(shell_command("while :; do :; done")));

        // Act
        let error = execute_cli_command(
            &backend,
            AgentKind::Codex,
            build_request(&[], folder.path(), "prompt", &request_kind),
            &CollectingCliObserver,
            Some(Duration::from_millis(20)),
        )
        .await
        .expect_err("process should time out");

        // Assert
        assert!(matches!(error, CliExecutionError::Timeout(_)));
    }

    #[tokio::test]
    async fn test_finish_cli_execution_aborts_stdin_writer_after_execution_error() {
        // Arrange
        let stdin_write_task = tokio::spawn(std::future::pending());
        let execution_result = Err(CliExecutionError::StdoutRead(io::Error::other(
            "read failed",
        )));

        // Act
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            finish_cli_execution(Some(stdin_write_task), execution_result),
        )
        .await
        .expect("stdin writer cleanup should not hang")
        .expect_err("execution failure should be preserved");

        // Assert
        assert!(matches!(error, CliExecutionError::StdoutRead(_)));
    }

    #[test]
    fn test_require_pipe_returns_unavailable_error() {
        // Arrange / Act
        let error = require_pipe::<u8>(None, "stdout").expect_err("pipe should be required");

        // Assert
        assert!(matches!(
            error,
            CliExecutionError::PipeUnavailable("stdout")
        ));
    }

    #[tokio::test]
    async fn test_capture_stdout_returns_read_error() {
        // Arrange
        let observer = CollectingCliObserver;

        // Act
        let error = capture_stdout(tokio::io::BufReader::new(FailingReader), &observer)
            .await
            .expect_err("stdout read should fail");

        // Assert
        assert!(matches!(error, CliExecutionError::StdoutRead(_)));
    }

    #[tokio::test]
    async fn test_capture_stderr_returns_read_error() {
        // Arrange / Act
        let error = capture_stderr(FailingReader)
            .await
            .expect_err("stderr read should fail");

        // Assert
        assert!(matches!(error, CliExecutionError::StderrRead(_)));
    }
}
