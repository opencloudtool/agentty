use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::runtime::TuiTerminal;

/// Boxed async result returned by [`EditorLauncher`] methods.
type EditorLaunchFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Async boundary for launching one external editor process.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait EditorLauncher: Send + Sync {
    /// Launches `editor_binary` with `args` in `working_dir`.
    fn launch_editor(
        &self,
        working_dir: PathBuf,
        editor_binary: String,
        args: Vec<String>,
    ) -> EditorLaunchFuture<io::Result<()>>;
}

/// Production [`EditorLauncher`] that shells out to external editors.
pub(crate) struct ProcessEditorLauncher;

impl ProcessEditorLauncher {
    /// Launches one editor process and validates successful exit status.
    async fn launch_editor_impl(
        working_dir: PathBuf,
        editor_binary: String,
        args: Vec<String>,
    ) -> io::Result<()> {
        let editor_status = tokio::process::Command::new(&editor_binary)
            .args(&args)
            .current_dir(working_dir)
            .status()
            .await?;
        if !editor_status.success() {
            return Err(io::Error::other(format!(
                "`{editor_binary}` exited with status {editor_status}"
            )));
        }

        Ok(())
    }
}

impl EditorLauncher for ProcessEditorLauncher {
    fn launch_editor(
        &self,
        working_dir: PathBuf,
        editor_binary: String,
        args: Vec<String>,
    ) -> EditorLaunchFuture<io::Result<()>> {
        Box::pin(async move { Self::launch_editor_impl(working_dir, editor_binary, args).await })
    }
}

/// Restores terminal state on all exit paths after raw mode is enabled.
///
/// The app uses `?` extensively inside the event loop and setup flow. Without
/// this guard, any early return after entering raw mode and the alternate
/// screen can leave the user's shell in a broken state.
///
/// Keeping cleanup in `Drop` guarantees restore runs during normal exit,
/// runtime errors, and unwinding panics.
pub(crate) struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = disable_raw_mode();
        let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen, Show);
    }
}

/// Enables raw mode, enters the alternate screen, and turns on bracketed paste
/// so multiline clipboard content arrives as `Event::Paste`.
pub(crate) fn setup_terminal() -> io::Result<TuiTerminal> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);

    Terminal::new(backend)
}

/// Opens `nvim` in read-only mode (`-R`) in `working_dir` while temporarily
/// suspending the ratatui terminal state.
///
/// This follows the ratatui spawn-editor recipe by leaving the alternate
/// screen and disabling raw mode before launching the editor, then restoring
/// the TUI state after the process exits.
pub(crate) async fn open_nvim(
    terminal: &mut TuiTerminal,
    event_reader_pause: &AtomicBool,
    working_dir: &Path,
) -> io::Result<()> {
    open_external_editor(terminal, event_reader_pause, working_dir, "nvim", &["-R"]).await
}

/// Opens one external full-screen editor process from the provided directory
/// and restores ratatui terminal mode when it exits.
pub(crate) async fn open_external_editor(
    terminal: &mut TuiTerminal,
    event_reader_pause: &AtomicBool,
    working_dir: &Path,
    editor_binary: &str,
    args: &[&str],
) -> io::Result<()> {
    let launcher = ProcessEditorLauncher;

    open_external_editor_with_launcher(
        terminal,
        event_reader_pause,
        &launcher,
        working_dir,
        editor_binary,
        args,
    )
    .await
}

/// Opens one external editor process using an injected launcher and restores
/// ratatui terminal mode when it exits.
async fn open_external_editor_with_launcher(
    terminal: &mut TuiTerminal,
    event_reader_pause: &AtomicBool,
    editor_launcher: &dyn EditorLauncher,
    working_dir: &Path,
    editor_binary: &str,
    args: &[&str],
) -> io::Result<()> {
    let pause_guard = EventReaderPauseGuard::new(event_reader_pause);
    suspend_for_external_editor(terminal)?;

    let launch_result =
        launch_external_editor(editor_launcher, working_dir, editor_binary, args).await;
    let resume_result = resume_after_external_editor(terminal);

    drop(pause_guard);
    resume_result?;

    launch_result
}

/// Launches one external editor using the provided launcher boundary.
async fn launch_external_editor(
    editor_launcher: &dyn EditorLauncher,
    working_dir: &Path,
    editor_binary: &str,
    args: &[&str],
) -> io::Result<()> {
    let editor_arguments: Vec<String> = args
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();

    editor_launcher
        .launch_editor(
            working_dir.to_path_buf(),
            editor_binary.to_string(),
            editor_arguments,
        )
        .await
}

/// Leaves alternate-screen raw terminal mode so an external process can own
/// the user's TTY.
fn suspend_for_external_editor(terminal: &mut TuiTerminal) -> io::Result<()> {
    terminal.show_cursor()?;
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    )?;

    Ok(())
}

/// Re-enters alternate-screen raw terminal mode after an external process
/// exits and clears stale content from the next frame.
fn resume_after_external_editor(terminal: &mut TuiTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        Hide
    )?;
    terminal.clear()?;

    Ok(())
}

/// Temporarily pauses the background event-reader thread while an external
/// interactive process owns the terminal.
struct EventReaderPauseGuard<'a> {
    event_reader_pause: &'a AtomicBool,
    was_paused: bool,
}

impl<'a> EventReaderPauseGuard<'a> {
    /// Marks the shared event-reader pause flag as active and stores the
    /// previous value for restoration.
    fn new(event_reader_pause: &'a AtomicBool) -> Self {
        let was_paused = event_reader_pause.swap(true, Ordering::Relaxed);

        Self {
            event_reader_pause,
            was_paused,
        }
    }
}

impl Drop for EventReaderPauseGuard<'_> {
    fn drop(&mut self) {
        self.event_reader_pause
            .store(self.was_paused, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mockall::predicate::eq;

    use super::*;

    #[tokio::test]
    async fn launch_external_editor_passes_working_directory_binary_and_args() {
        // Arrange
        let working_dir = PathBuf::from("/tmp/project");
        let mut mock_editor_launcher = MockEditorLauncher::new();
        mock_editor_launcher
            .expect_launch_editor()
            .with(
                eq(working_dir.clone()),
                eq("nvim".to_string()),
                eq(vec!["-R".to_string(), "README.md".to_string()]),
            )
            .times(1)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        // Act
        let launch_result = launch_external_editor(
            &mock_editor_launcher,
            &working_dir,
            "nvim",
            &["-R", "README.md"],
        )
        .await;

        // Assert
        assert!(launch_result.is_ok());
    }

    #[tokio::test]
    async fn launch_external_editor_returns_launcher_error() {
        // Arrange
        let working_dir = PathBuf::from("/tmp/project");
        let mut mock_editor_launcher = MockEditorLauncher::new();
        mock_editor_launcher
            .expect_launch_editor()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Err(io::Error::other("launch failed")) }));

        // Act
        let launch_result =
            launch_external_editor(&mock_editor_launcher, &working_dir, "nvim", &["-R"]).await;

        // Assert
        assert_eq!(
            launch_result.expect_err("launcher should fail").to_string(),
            "launch failed"
        );
    }
}
