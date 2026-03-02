use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

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

/// Production [`TmuxClient`] implementation backed by tmux subprocess calls.
pub struct RealTmuxClient;

impl RealTmuxClient {
    /// Opens one tmux window in `session_folder` and returns its window id.
    async fn open_window_for_folder_impl(session_folder: PathBuf) -> Option<String> {
        let output = tokio::process::Command::new("tmux")
            .arg("new-window")
            .arg("-P")
            .arg("-F")
            .arg("#{window_id}")
            .arg("-c")
            .arg(session_folder)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        Self::parse_tmux_window_id(&output.stdout)
    }

    /// Sends `command` and Enter to one tmux `window_id`.
    async fn run_command_in_window_impl(window_id: String, command: String) {
        let send_literal_output = tokio::process::Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(&window_id)
            .arg("-l")
            .arg(command)
            .output()
            .await;

        let Ok(send_literal_output) = send_literal_output else {
            return;
        };
        if !send_literal_output.status.success() {
            return;
        }

        let _ = tokio::process::Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(window_id)
            .arg("C-m")
            .output()
            .await;
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
    use super::*;

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
}
