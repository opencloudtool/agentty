use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Output;

/// Boxed async result returned by [`TmuxClient`] methods.
pub type TmuxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Async tmux boundary used by app orchestration.
#[cfg_attr(test, mockall::automock)]
pub trait TmuxClient: Send + Sync {
    /// Opens one tmux window rooted at `session_folder`.
    ///
    /// Returns the tmux window id when creation succeeds.
    fn open_window_for_folder(&self, session_folder: PathBuf) -> TmuxFuture<Option<String>>;

    /// Sends `command` followed by Enter to the target tmux `window_id`.
    fn run_command_in_window(&self, window_id: String, command: String) -> TmuxFuture<()>;
}

/// Captured tmux subprocess result used by injected command runners.
#[derive(Debug, Eq, PartialEq)]
struct TmuxCommandOutput {
    status_success: bool,
    stdout: Vec<u8>,
}

impl TmuxCommandOutput {
    /// Converts one subprocess output into the reduced tmux result shape.
    fn from_process_output(output: Output) -> Self {
        Self {
            status_success: output.status.success(),
            stdout: output.stdout,
        }
    }
}

/// Async tmux command boundary used to test multi-command flows
/// deterministically.
#[cfg_attr(test, mockall::automock)]
trait TmuxCommandRunner: Send + Sync {
    /// Opens one tmux window rooted at `session_folder`.
    fn open_window(&self, session_folder: PathBuf) -> TmuxFuture<io::Result<TmuxCommandOutput>>;

    /// Sends literal `command` bytes to the target tmux `window_id`.
    fn send_literal_keys(
        &self,
        window_id: String,
        command: String,
    ) -> TmuxFuture<io::Result<TmuxCommandOutput>>;

    /// Sends Enter to the target tmux `window_id`.
    fn send_enter_key(&self, window_id: String) -> TmuxFuture<io::Result<TmuxCommandOutput>>;
}

/// Production tmux command runner backed by subprocess calls.
struct ProcessTmuxCommandRunner;

impl ProcessTmuxCommandRunner {
    /// Opens one tmux window in `session_folder`.
    async fn open_window_impl(session_folder: PathBuf) -> io::Result<TmuxCommandOutput> {
        let output = tokio::process::Command::new("tmux")
            .arg("new-window")
            .arg("-P")
            .arg("-F")
            .arg("#{window_id}")
            .arg("-c")
            .arg(session_folder)
            .output()
            .await?;

        Ok(TmuxCommandOutput::from_process_output(output))
    }

    /// Sends literal `command` bytes to one tmux `window_id`.
    async fn send_literal_keys_impl(
        window_id: String,
        command: String,
    ) -> io::Result<TmuxCommandOutput> {
        let output = tokio::process::Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(window_id)
            .arg("-l")
            .arg(command)
            .output()
            .await?;

        Ok(TmuxCommandOutput::from_process_output(output))
    }

    /// Sends Enter to one tmux `window_id`.
    async fn send_enter_key_impl(window_id: String) -> io::Result<TmuxCommandOutput> {
        let output = tokio::process::Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(window_id)
            .arg("C-m")
            .output()
            .await?;

        Ok(TmuxCommandOutput::from_process_output(output))
    }
}

impl TmuxCommandRunner for ProcessTmuxCommandRunner {
    fn open_window(&self, session_folder: PathBuf) -> TmuxFuture<io::Result<TmuxCommandOutput>> {
        Box::pin(async move { Self::open_window_impl(session_folder).await })
    }

    fn send_literal_keys(
        &self,
        window_id: String,
        command: String,
    ) -> TmuxFuture<io::Result<TmuxCommandOutput>> {
        Box::pin(async move { Self::send_literal_keys_impl(window_id, command).await })
    }

    fn send_enter_key(&self, window_id: String) -> TmuxFuture<io::Result<TmuxCommandOutput>> {
        Box::pin(async move { Self::send_enter_key_impl(window_id).await })
    }
}

/// Production [`TmuxClient`] implementation backed by tmux subprocess calls.
pub struct RealTmuxClient;

impl RealTmuxClient {
    /// Opens one tmux window in `session_folder` and returns its window id.
    async fn open_window_for_folder_impl(session_folder: PathBuf) -> Option<String> {
        let command_runner = ProcessTmuxCommandRunner;

        Self::open_window_for_folder_with_runner(&command_runner, session_folder).await
    }

    /// Sends `command` and Enter to one tmux `window_id`.
    async fn run_command_in_window_impl(window_id: String, command: String) {
        let command_runner = ProcessTmuxCommandRunner;

        Self::run_command_in_window_with_runner(&command_runner, window_id, command).await;
    }

    /// Opens one tmux window using the provided command runner.
    async fn open_window_for_folder_with_runner(
        command_runner: &dyn TmuxCommandRunner,
        session_folder: PathBuf,
    ) -> Option<String> {
        let output = command_runner.open_window(session_folder).await.ok()?;
        if !output.status_success {
            return None;
        }

        Self::parse_tmux_window_id(&output.stdout)
    }

    /// Sends `command` and Enter using the provided command runner.
    async fn run_command_in_window_with_runner(
        command_runner: &dyn TmuxCommandRunner,
        window_id: String,
        command: String,
    ) {
        let send_literal_output = command_runner
            .send_literal_keys(window_id.clone(), command)
            .await;

        let Ok(send_literal_output) = send_literal_output else {
            return;
        };
        if !send_literal_output.status_success {
            return;
        }

        let _ = command_runner.send_enter_key(window_id).await;
    }

    /// Parses a tmux window id from command output bytes.
    fn parse_tmux_window_id(stdout: &[u8]) -> Option<String> {
        let window_id = std::str::from_utf8(stdout).ok()?.trim();
        if window_id.is_empty() {
            return None;
        }

        Some(window_id.to_string())
    }
}

impl TmuxClient for RealTmuxClient {
    fn open_window_for_folder(&self, session_folder: PathBuf) -> TmuxFuture<Option<String>> {
        Box::pin(async move { Self::open_window_for_folder_impl(session_folder).await })
    }

    fn run_command_in_window(&self, window_id: String, command: String) -> TmuxFuture<()> {
        Box::pin(async move { Self::run_command_in_window_impl(window_id, command).await })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mockall::Sequence;
    use mockall::predicate::eq;

    use super::*;

    #[tokio::test]
    async fn open_window_for_folder_with_runner_returns_window_id_on_success() {
        // Arrange
        let mut command_runner = MockTmuxCommandRunner::new();
        let session_folder = PathBuf::from("/tmp/agentty-session");

        command_runner
            .expect_open_window()
            .with(eq(session_folder.clone()))
            .times(1)
            .return_once(|_| Box::pin(async { Ok(successful_tmux_output(b"@42\n")) }));

        // Act
        let window_id =
            RealTmuxClient::open_window_for_folder_with_runner(&command_runner, session_folder)
                .await;

        // Assert
        assert_eq!(window_id, Some("@42".to_string()));
    }

    #[tokio::test]
    async fn open_window_for_folder_with_runner_returns_none_when_command_fails() {
        // Arrange
        let mut command_runner = MockTmuxCommandRunner::new();
        let session_folder = PathBuf::from("/tmp/agentty-session");

        command_runner
            .expect_open_window()
            .with(eq(session_folder.clone()))
            .times(1)
            .return_once(|_| Box::pin(async { Err(io::Error::other("tmux unavailable")) }));

        // Act
        let window_id =
            RealTmuxClient::open_window_for_folder_with_runner(&command_runner, session_folder)
                .await;

        // Assert
        assert_eq!(window_id, None);
    }

    #[tokio::test]
    async fn open_window_for_folder_with_runner_returns_none_when_tmux_exits_unsuccessfully() {
        // Arrange
        let mut command_runner = MockTmuxCommandRunner::new();
        let session_folder = PathBuf::from("/tmp/agentty-session");

        command_runner
            .expect_open_window()
            .with(eq(session_folder.clone()))
            .times(1)
            .return_once(|_| Box::pin(async { Ok(failed_tmux_output()) }));

        // Act
        let window_id =
            RealTmuxClient::open_window_for_folder_with_runner(&command_runner, session_folder)
                .await;

        // Assert
        assert_eq!(window_id, None);
    }

    #[tokio::test]
    async fn run_command_in_window_with_runner_sends_enter_after_literal_keys() {
        // Arrange
        let mut command_runner = MockTmuxCommandRunner::new();
        let mut sequence = Sequence::new();

        command_runner
            .expect_send_literal_keys()
            .with(eq("@42".to_string()), eq("git status".to_string()))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(|_, _| Box::pin(async { Ok(successful_tmux_output(b"")) }));
        command_runner
            .expect_send_enter_key()
            .with(eq("@42".to_string()))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(|_| Box::pin(async { Ok(successful_tmux_output(b"")) }));

        // Act
        RealTmuxClient::run_command_in_window_with_runner(
            &command_runner,
            "@42".to_string(),
            "git status".to_string(),
        )
        .await;

        // Assert
        // `mockall` verifies the expected literal-send and Enter sequence.
    }

    #[tokio::test]
    async fn run_command_in_window_with_runner_stops_when_literal_send_fails() {
        // Arrange
        let mut command_runner = MockTmuxCommandRunner::new();

        command_runner
            .expect_send_literal_keys()
            .with(eq("@42".to_string()), eq("git status".to_string()))
            .times(1)
            .return_once(|_, _| Box::pin(async { Err(io::Error::other("tmux send-keys failed")) }));
        command_runner.expect_send_enter_key().times(0);

        // Act
        RealTmuxClient::run_command_in_window_with_runner(
            &command_runner,
            "@42".to_string(),
            "git status".to_string(),
        )
        .await;

        // Assert
        // `mockall` verifies that Enter is not sent after a command error.
    }

    #[tokio::test]
    async fn run_command_in_window_with_runner_stops_when_literal_send_exits_unsuccessfully() {
        // Arrange
        let mut command_runner = MockTmuxCommandRunner::new();

        command_runner
            .expect_send_literal_keys()
            .with(eq("@42".to_string()), eq("git status".to_string()))
            .times(1)
            .return_once(|_, _| Box::pin(async { Ok(failed_tmux_output()) }));
        command_runner.expect_send_enter_key().times(0);

        // Act
        RealTmuxClient::run_command_in_window_with_runner(
            &command_runner,
            "@42".to_string(),
            "git status".to_string(),
        )
        .await;

        // Assert
        // `mockall` verifies that Enter is not sent after a failed exit status.
    }

    #[test]
    fn parse_tmux_window_id_returns_none_for_invalid_utf8() {
        // Arrange
        let stdout = [0x80];

        // Act
        let window_id = RealTmuxClient::parse_tmux_window_id(&stdout);

        // Assert
        assert_eq!(window_id, None);
    }

    #[test]
    fn parse_tmux_window_id_trims_newline_and_returns_window_id() {
        // Arrange
        let stdout = b"@42\n";

        // Act
        let window_id = RealTmuxClient::parse_tmux_window_id(stdout);

        // Assert
        assert_eq!(window_id, Some("@42".to_string()));
    }

    fn successful_tmux_output(stdout: &[u8]) -> TmuxCommandOutput {
        TmuxCommandOutput {
            status_success: true,
            stdout: stdout.to_vec(),
        }
    }

    fn failed_tmux_output() -> TmuxCommandOutput {
        TmuxCommandOutput {
            status_success: false,
            stdout: vec![],
        }
    }
}
