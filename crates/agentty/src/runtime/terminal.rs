use std::io;
use std::path::Path;
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
    let pause_guard = EventReaderPauseGuard::new(event_reader_pause);
    suspend_for_external_editor(terminal)?;

    let editor_status_result = tokio::process::Command::new(editor_binary)
        .args(args)
        .current_dir(working_dir)
        .status()
        .await;
    let resume_result = resume_after_external_editor(terminal);

    drop(pause_guard);
    resume_result?;

    let editor_status = editor_status_result?;
    if !editor_status.success() {
        return Err(io::Error::other(format!(
            "`{editor_binary}` exited with status {editor_status}"
        )));
    }

    Ok(())
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
