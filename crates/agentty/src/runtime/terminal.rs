use std::cell::Cell;
use std::io;

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::runtime::TuiTerminal;

/// Abstraction over terminal transitions so setup/restore paths can be tested
/// without touching real terminal state.
#[cfg_attr(test, mockall::automock)]
trait TerminalOperation {
    /// Enables terminal raw mode before entering the alternate screen.
    fn enable_raw_mode(&self) -> io::Result<()>;

    /// Disables terminal raw mode during cleanup.
    fn disable_raw_mode(&self) -> io::Result<()>;

    /// Returns whether the active terminal supports keyboard enhancement
    /// flags for reporting modified keys like `Alt+Enter`.
    fn supports_keyboard_enhancement(&self) -> io::Result<bool>;

    /// Enters the alternate screen and enables bracketed paste, optionally
    /// enabling keyboard enhancement flags first.
    fn enter_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()>;

    /// Leaves the alternate screen, disables bracketed paste, and restores the
    /// terminal cursor, optionally popping keyboard enhancement flags first.
    fn leave_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()>;
}

/// Production terminal operations backed by `crossterm`.
struct CrosstermTerminalOperation;

impl TerminalOperation for CrosstermTerminalOperation {
    fn enable_raw_mode(&self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw_mode(&self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn supports_keyboard_enhancement(&self) -> io::Result<bool> {
        supports_keyboard_enhancement()
    }

    fn enter_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()> {
        if keyboard_enhancement_enabled {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(keyboard_enhancement_flags()),
                EnterAlternateScreen,
                EnableBracketedPaste
            )
        } else {
            execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)
        }
    }

    fn leave_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()> {
        if keyboard_enhancement_enabled {
            execute!(
                stdout,
                PopKeyboardEnhancementFlags,
                DisableBracketedPaste,
                LeaveAlternateScreen,
                Show
            )
        } else {
            execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen, Show)
        }
    }
}

/// Shared production terminal operation implementation.
static CROSSTERM_TERMINAL_OPERATION: CrosstermTerminalOperation = CrosstermTerminalOperation;

/// Returns the keyboard enhancement flag set used to disambiguate modified key
/// presses in terminals that support the kitty keyboard protocol without
/// requesting key release/repeat event streams.
fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
}

/// Restores terminal state on all exit paths after raw mode is enabled.
///
/// The app uses `?` extensively inside the event loop and setup flow. Without
/// this guard, any early return after entering raw mode and the alternate
/// screen can leave the user's shell in a broken state.
///
/// Keeping cleanup in `Drop` guarantees restore runs during normal exit,
/// runtime errors, and unwinding panics. The guard is intentionally
/// thread-affine: setup mutates its state before the runtime loop starts and
/// cleanup runs from the same task via `Drop`.
pub(crate) struct TerminalGuard {
    keyboard_enhancement_enabled: Cell<bool>,
}

impl TerminalGuard {
    /// Creates a guard that restores terminal state for the active TUI session.
    pub(crate) fn new() -> Self {
        Self {
            keyboard_enhancement_enabled: Cell::new(false),
        }
    }

    /// Records whether setup enabled keyboard enhancement flags so cleanup can
    /// pop them symmetrically.
    fn set_keyboard_enhancement_enabled(&self, enabled: bool) {
        self.keyboard_enhancement_enabled.set(enabled);
    }

    /// Returns whether cleanup must pop keyboard enhancement flags before
    /// leaving the alternate screen.
    fn keyboard_enhancement_enabled(&self) -> bool {
        self.keyboard_enhancement_enabled.get()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(
            &CROSSTERM_TERMINAL_OPERATION,
            self.keyboard_enhancement_enabled(),
        );
    }
}

/// Enables raw mode, enters the alternate screen, and turns on bracketed paste
/// so multiline clipboard content arrives as `Event::Paste`.
///
/// When supported, keyboard enhancement flags are also enabled so modified
/// Enter keys remain distinguishable over transports like SSH and `tmux`.
pub(crate) fn setup_terminal(guard: &TerminalGuard) -> io::Result<TuiTerminal> {
    setup_terminal_with_operation(&CROSSTERM_TERMINAL_OPERATION, guard)
}

/// Enables terminal modes with the supplied operation provider.
fn setup_terminal_with_operation(
    operation: &dyn TerminalOperation,
    guard: &TerminalGuard,
) -> io::Result<TuiTerminal> {
    operation.enable_raw_mode()?;

    let keyboard_enhancement_enabled =
        matches!(operation.supports_keyboard_enhancement(), Ok(true));
    guard.set_keyboard_enhancement_enabled(keyboard_enhancement_enabled);

    let mut stdout = io::stdout();
    operation.enter_alternate_screen(&mut stdout, keyboard_enhancement_enabled)?;
    let backend = CrosstermBackend::new(stdout);

    Terminal::new(backend)
}

/// Restores terminal modes and ignores failures so drop paths do not panic.
fn restore_terminal_state(operation: &dyn TerminalOperation, keyboard_enhancement_enabled: bool) {
    let mut stdout = io::stdout();
    let _ = operation.disable_raw_mode();
    let _ = operation.leave_alternate_screen(&mut stdout, keyboard_enhancement_enabled);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Verifies setup returns raw-mode failures directly.
    #[test]
    fn setup_terminal_returns_error_when_enable_raw_mode_fails() {
        // Arrange
        let mut operation = MockTerminalOperation::new();
        let guard = TerminalGuard::new();
        operation
            .expect_enable_raw_mode()
            .once()
            .returning(|| Err(io::Error::other("enable failed")));
        operation.expect_enter_alternate_screen().times(0);
        operation.expect_supports_keyboard_enhancement().times(0);

        // Act
        let result = setup_terminal_with_operation(&operation, &guard);

        // Assert
        let error = result.expect_err("setup should fail when raw mode fails");
        assert_eq!(error.to_string(), "enable failed");
    }

    /// Verifies setup returns alternate-screen failures directly.
    #[test]
    fn setup_terminal_returns_error_when_enter_alternate_screen_fails() {
        // Arrange
        let mut operation = MockTerminalOperation::new();
        let guard = TerminalGuard::new();
        operation
            .expect_enable_raw_mode()
            .once()
            .returning(|| Ok(()));
        operation
            .expect_supports_keyboard_enhancement()
            .once()
            .returning(|| Ok(false));
        operation
            .expect_enter_alternate_screen()
            .once()
            .withf(|_, keyboard_enhancement_enabled| !keyboard_enhancement_enabled)
            .returning(|_, _| Err(io::Error::other("enter failed")));

        // Act
        let result = setup_terminal_with_operation(&operation, &guard);

        // Assert
        let error = result.expect_err("setup should fail when alternate screen fails");
        assert_eq!(error.to_string(), "enter failed");
    }

    /// Verifies setup enables keyboard enhancement when the terminal reports
    /// support for the protocol.
    #[test]
    fn setup_terminal_enables_keyboard_enhancement_when_supported() {
        // Arrange
        let mut operation = MockTerminalOperation::new();
        let guard = TerminalGuard::new();
        operation
            .expect_enable_raw_mode()
            .once()
            .returning(|| Ok(()));
        operation
            .expect_supports_keyboard_enhancement()
            .once()
            .returning(|| Ok(true));
        operation
            .expect_enter_alternate_screen()
            .once()
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = setup_terminal_with_operation(&operation, &guard);

        // Assert
        let _terminal =
            result.expect("setup should succeed when keyboard enhancement is supported");
        assert!(guard.keyboard_enhancement_enabled());
    }

    /// Verifies support-query failures fall back to the legacy key mode so TUI
    /// startup still succeeds.
    #[test]
    fn setup_terminal_ignores_keyboard_enhancement_query_failures() {
        // Arrange
        let mut operation = MockTerminalOperation::new();
        let guard = TerminalGuard::new();
        operation
            .expect_enable_raw_mode()
            .once()
            .returning(|| Ok(()));
        operation
            .expect_supports_keyboard_enhancement()
            .once()
            .returning(|| Err(io::Error::other("unsupported")));
        operation
            .expect_enter_alternate_screen()
            .once()
            .withf(|_, keyboard_enhancement_enabled| !keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = setup_terminal_with_operation(&operation, &guard);

        // Assert
        let _terminal = result.expect("setup should fall back when support query fails");
        assert!(!guard.keyboard_enhancement_enabled());
    }

    /// Verifies restore still attempts alternate-screen cleanup when raw-mode
    /// cleanup fails.
    #[test]
    fn restore_terminal_state_attempts_leave_even_when_disable_fails() {
        // Arrange
        let mut operation = MockTerminalOperation::new();
        let leave_calls = Arc::new(AtomicUsize::new(0));
        let leave_calls_for_expectation = leave_calls.clone();
        operation
            .expect_disable_raw_mode()
            .once()
            .returning(|| Err(io::Error::other("disable failed")));
        operation
            .expect_leave_alternate_screen()
            .once()
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(move |_, _| {
                leave_calls_for_expectation.fetch_add(1, Ordering::Relaxed);
                Ok(())
            });

        // Act
        restore_terminal_state(&operation, true);

        // Assert
        assert_eq!(leave_calls.load(Ordering::Relaxed), 1);
    }
}
