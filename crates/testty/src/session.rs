//! PTY session for driving a real TUI binary.
//!
//! [`PtySession`] spawns a compiled binary inside a pseudo-terminal using
//! `portable-pty`, sets deterministic rows and columns, and provides methods
//! to write input and read the raw ANSI byte stream. The session can capture
//! a [`TerminalFrame`] snapshot at any point for semantic inspection.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

use crate::frame::TerminalFrame;
use crate::step::Step;

/// Default number of terminal columns.
const DEFAULT_COLS: u16 = 80;

/// Default number of terminal rows.
const DEFAULT_ROWS: u16 = 24;

/// Default read timeout for draining PTY output.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(500);

/// A live PTY session driving a real TUI binary.
///
/// The session owns the PTY child process and provides methods to send
/// input, wait for output, and capture terminal frames for assertion.
pub struct PtySession {
    /// Path to the binary being driven.
    binary_path: PathBuf,
    /// Number of columns in the terminal grid.
    cols: u16,
    /// Number of rows in the terminal grid.
    rows: u16,
    /// Accumulated raw ANSI output bytes from the PTY.
    output_buffer: Vec<u8>,
    /// Writer end of the PTY for sending input.
    writer: Box<dyn Write + Send>,
    /// Receiver for output bytes read from the PTY in a background thread.
    output_receiver: mpsc::Receiver<Vec<u8>>,
    /// The child process handle, killed on drop.
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl PtySession {
    /// Spawn a binary in a new PTY session with default dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error if the PTY cannot be created or the binary cannot
    /// be spawned.
    pub fn spawn(binary_path: &Path) -> Result<Self, PtySessionError> {
        Self::spawn_with_size(binary_path, DEFAULT_COLS, DEFAULT_ROWS, &[], None)
    }

    /// Spawn a binary in a new PTY session with custom dimensions,
    /// environment variables, and an optional working directory.
    ///
    /// Each entry in `env_vars` is a `(key, value)` pair that will be set
    /// in the child process environment. When `workdir` is `Some`, the
    /// child process starts in that directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the PTY cannot be created or the binary cannot
    /// be spawned.
    pub fn spawn_with_size(
        binary_path: &Path,
        cols: u16,
        rows: u16,
        env_vars: &[(&str, &str)],
        workdir: Option<&Path>,
    ) -> Result<Self, PtySessionError> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| PtySessionError::PtyCreation(err.to_string()))?;

        let mut command = CommandBuilder::new(binary_path);
        if let Some(directory) = workdir {
            command.cwd(directory);
        }
        for (key, value) in env_vars {
            command.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| PtySessionError::SpawnFailed(err.to_string()))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| PtySessionError::PtyCreation(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| PtySessionError::PtyCreation(err.to_string()))?;

        let (output_sender, output_receiver) = mpsc::channel();
        thread::spawn(move || read_pty_output(reader, &output_sender));

        Ok(Self {
            binary_path: binary_path.to_path_buf(),
            cols,
            rows,
            output_buffer: Vec::new(),
            writer,
            output_receiver,
            child,
        })
    }

    /// Write raw bytes to the PTY input.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the PTY fails.
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<(), PtySessionError> {
        self.writer
            .write_all(data)
            .map_err(|err| PtySessionError::WriteFailed(err.to_string()))?;
        self.writer
            .flush()
            .map_err(|err| PtySessionError::WriteFailed(err.to_string()))?;

        Ok(())
    }

    /// Write a text string to the PTY input.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the PTY fails.
    pub fn write_text(&mut self, text: &str) -> Result<(), PtySessionError> {
        self.write_bytes(text.as_bytes())
    }

    /// Send a special key press to the PTY.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the PTY fails.
    pub fn press_key(&mut self, key: &str) -> Result<(), PtySessionError> {
        let bytes = key_to_bytes(key);

        self.write_bytes(&bytes)
    }

    /// Drain all available output from the PTY into the internal buffer.
    ///
    /// Waits up to `timeout` for any pending output to arrive.
    pub fn drain_output(&mut self, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;

        while let Ok(chunk) = self.output_receiver.recv_timeout(
            deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::ZERO),
        ) {
            self.output_buffer.extend_from_slice(&chunk);
        }
    }

    /// Capture the current terminal state as a [`TerminalFrame`].
    ///
    /// Drains pending output first with the default timeout.
    pub fn capture_frame(&mut self) -> TerminalFrame {
        self.drain_output(DEFAULT_READ_TIMEOUT);

        TerminalFrame::new(self.cols, self.rows, &self.output_buffer)
    }

    /// Wait until the specified text appears in the terminal, or timeout.
    ///
    /// Polls the terminal at 100ms intervals.
    ///
    /// # Errors
    ///
    /// Returns an error if the text does not appear within the timeout.
    pub fn wait_for_text(
        &mut self,
        needle: &str,
        timeout: Duration,
    ) -> Result<TerminalFrame, PtySessionError> {
        let deadline = std::time::Instant::now() + timeout;

        loop {
            self.drain_output(Duration::from_millis(100));
            let frame = TerminalFrame::new(self.cols, self.rows, &self.output_buffer);

            if !frame.find_text(needle).is_empty() {
                return Ok(frame);
            }

            if std::time::Instant::now() >= deadline {
                return Err(PtySessionError::Timeout(format!(
                    "Text '{needle}' did not appear within {}ms",
                    timeout.as_millis()
                )));
            }
        }
    }

    /// Wait until the terminal frame stabilizes (no changes for
    /// `stable_duration`).
    ///
    /// The method requires at least one frame change from the initial empty
    /// state before the stability timer starts. This prevents returning an
    /// empty frame when the binary has not yet produced any output.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame does not stabilize within the timeout.
    pub fn wait_for_stable_frame(
        &mut self,
        stable_duration: Duration,
        timeout: Duration,
    ) -> Result<TerminalFrame, PtySessionError> {
        let deadline = std::time::Instant::now() + timeout;
        let mut previous_text = String::new();
        let mut stable_since = std::time::Instant::now();
        let mut seen_change = false;

        loop {
            self.drain_output(Duration::from_millis(100));
            let frame = TerminalFrame::new(self.cols, self.rows, &self.output_buffer);
            let current_text = frame.all_text();

            if current_text != previous_text {
                previous_text = current_text;
                stable_since = std::time::Instant::now();
                seen_change = true;
            } else if seen_change
                && std::time::Instant::now().duration_since(stable_since) >= stable_duration
            {
                return Ok(frame);
            }

            if std::time::Instant::now() >= deadline {
                return Err(PtySessionError::Timeout(
                    "Frame did not stabilize within timeout".to_string(),
                ));
            }
        }
    }

    /// Execute a sequence of [`Step`] actions against this session.
    ///
    /// Returns the final [`TerminalFrame`] after all steps have been
    /// executed. Capture steps record intermediate frames but the last
    /// frame is always returned.
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails (e.g., timeout, write failure).
    pub fn execute_steps(&mut self, steps: &[Step]) -> Result<TerminalFrame, PtySessionError> {
        let mut last_frame = None;

        for step in steps {
            match step {
                Step::WriteText(text) => {
                    self.write_text(text)?;
                }
                Step::PressKey(key) => {
                    self.press_key(key)?;
                }
                Step::Sleep(duration) => {
                    thread::sleep(*duration);
                }
                Step::WaitForText { needle, timeout_ms } => {
                    let timeout = Duration::from_millis(u64::from(*timeout_ms));
                    last_frame = Some(self.wait_for_text(needle, timeout)?);
                }
                Step::WaitForStableFrame {
                    stable_ms,
                    timeout_ms,
                } => {
                    let stable = Duration::from_millis(u64::from(*stable_ms));
                    let timeout = Duration::from_millis(u64::from(*timeout_ms));
                    last_frame = Some(self.wait_for_stable_frame(stable, timeout)?);
                }
                Step::Capture => {
                    last_frame = Some(self.capture_frame());
                }
            }
        }

        Ok(last_frame.unwrap_or_else(|| self.capture_frame()))
    }

    /// Return the configured number of columns.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Return the configured number of rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Return the path to the binary being driven.
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }
}

/// Terminates the PTY child process on drop to prevent orphan leaks.
///
/// Errors from `kill()` and `wait()` are intentionally discarded because the
/// child may have already exited or been reaped.
impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Errors that can occur during PTY session operations.
#[derive(Debug, thiserror::Error)]
pub enum PtySessionError {
    /// Failed to create the PTY.
    #[error("PTY creation failed: {0}")]
    PtyCreation(String),

    /// Failed to spawn the child process.
    #[error("Failed to spawn binary: {0}")]
    SpawnFailed(String),

    /// Failed to write to the PTY.
    #[error("Write failed: {0}")]
    WriteFailed(String),

    /// A wait operation timed out.
    #[error("Timeout: {0}")]
    Timeout(String),
}

/// Background thread function that reads PTY output and sends chunks
/// over the channel.
fn read_pty_output(mut reader: Box<dyn Read + Send>, sender: &mpsc::Sender<Vec<u8>>) {
    let mut buffer = [0u8; 4096];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(bytes_read) => {
                if sender.send(buffer[..bytes_read].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

/// Convert a human-readable key name to terminal escape bytes.
fn key_to_bytes(key: &str) -> Vec<u8> {
    match key.to_lowercase().as_str() {
        "enter" | "return" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "escape" | "esc" => vec![0x1b],
        "backspace" => vec![0x7f],
        "up" => vec![0x1b, b'[', b'A'],
        "down" => vec![0x1b, b'[', b'B'],
        "right" => vec![0x1b, b'[', b'C'],
        "left" => vec![0x1b, b'[', b'D'],
        "home" => vec![0x1b, b'[', b'H'],
        "end" => vec![0x1b, b'[', b'F'],
        "delete" => vec![0x1b, b'[', b'3', b'~'],
        "pageup" => vec![0x1b, b'[', b'5', b'~'],
        "pagedown" => vec![0x1b, b'[', b'6', b'~'],
        "space" => vec![b' '],
        other => {
            // Check for ctrl+ combinations (exactly one a–z letter).
            if let Some(character) = other.strip_prefix("ctrl+")
                && character.len() == 1
                && let Some(byte) = character.bytes().next()
                && byte.to_ascii_lowercase().is_ascii_lowercase()
            {
                // Ctrl+A = 0x01, Ctrl+Z = 0x1a.
                let ctrl_byte = byte.to_ascii_lowercase() - b'a' + 1;

                return vec![ctrl_byte];
            }

            // Fall through: send the raw string bytes.
            other.as_bytes().to_vec()
        }
    }
}

/// Builder for configuring a [`PtySession`] before spawning.
#[must_use]
pub struct PtySessionBuilder {
    /// Path to the binary to spawn.
    binary_path: PathBuf,
    /// Terminal columns.
    cols: u16,
    /// Terminal rows.
    rows: u16,
    /// Environment variables for the child process.
    env_vars: Vec<(String, String)>,
    /// Optional working directory for the child process.
    workdir: Option<PathBuf>,
}

impl PtySessionBuilder {
    /// Create a new builder for the given binary path.
    pub fn new(binary_path: impl Into<PathBuf>) -> Self {
        Self {
            binary_path: binary_path.into(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            env_vars: Vec::new(),
            workdir: None,
        }
    }

    /// Set the terminal dimensions.
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;

        self
    }

    /// Add an environment variable for the child process.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.push((key.into(), value.into()));

        self
    }

    /// Set the working directory for the child process.
    pub fn workdir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.workdir = Some(directory.into());

        self
    }

    /// Spawn the PTY session with the configured settings.
    ///
    /// # Errors
    ///
    /// Returns an error if the PTY cannot be created or the binary cannot
    /// be spawned.
    pub fn spawn(self) -> Result<PtySession, PtySessionError> {
        let env_refs: Vec<(&str, &str)> = self
            .env_vars
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();

        PtySession::spawn_with_size(
            &self.binary_path,
            self.cols,
            self.rows,
            &env_refs,
            self.workdir.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_to_bytes_returns_ctrl_a() {
        // Arrange / Act
        let bytes = key_to_bytes("ctrl+a");

        // Assert — Ctrl+A = 0x01.
        assert_eq!(bytes, vec![0x01]);
    }

    #[test]
    fn key_to_bytes_returns_ctrl_z() {
        // Arrange / Act
        let bytes = key_to_bytes("ctrl+z");

        // Assert — Ctrl+Z = 0x1a.
        assert_eq!(bytes, vec![0x1a]);
    }

    #[test]
    fn key_to_bytes_ctrl_multi_char_falls_through() {
        // Arrange / Act — "ctrl+ab" must not silently resolve to Ctrl+A.
        let bytes = key_to_bytes("ctrl+ab");

        // Assert
        assert_eq!(bytes, "ctrl+ab".as_bytes());
    }

    #[test]
    fn key_to_bytes_ctrl_non_alpha_falls_through() {
        // Arrange / Act — ctrl+[ is not a valid ctrl+letter combination.
        let bytes = key_to_bytes("ctrl+[");

        // Assert — falls through to raw bytes instead of panicking.
        assert_eq!(bytes, "ctrl+[".as_bytes());
    }

    #[test]
    fn key_to_bytes_known_keys() {
        // Arrange / Act / Assert
        assert_eq!(key_to_bytes("enter"), vec![b'\r']);
        assert_eq!(key_to_bytes("tab"), vec![b'\t']);
        assert_eq!(key_to_bytes("escape"), vec![0x1b]);
        assert_eq!(key_to_bytes("backspace"), vec![0x7f]);
        assert_eq!(key_to_bytes("space"), vec![b' ']);
    }

    #[test]
    fn key_to_bytes_unknown_key_returns_raw_bytes() {
        // Arrange / Act
        let bytes = key_to_bytes("x");

        // Assert
        assert_eq!(bytes, vec![b'x']);
    }

    /// Verifies that `wait_for_stable_frame` times out when the spawned
    /// binary produces no terminal output, instead of returning an empty
    /// frame immediately.
    #[test]
    fn wait_for_stable_frame_times_out_when_no_output() {
        // Arrange — script stays alive but produces nothing.
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let script_path = temp_dir.path().join("silent.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 60\n").expect("failed to write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .expect("failed to set permissions");
        }

        let mut session = PtySession::spawn(&script_path).expect("failed to spawn silent script");

        // Act
        let result =
            session.wait_for_stable_frame(Duration::from_millis(200), Duration::from_millis(800));

        // Assert
        assert!(
            matches!(result, Err(PtySessionError::Timeout(_))),
            "should timeout when no frame change is observed"
        );
    }

    /// Verifies that `wait_for_stable_frame` returns a non-empty frame once
    /// the binary has rendered output and the frame stops changing.
    #[test]
    fn wait_for_stable_frame_returns_after_content_stabilizes() {
        // Arrange — script writes visible text and stays alive so the PTY
        // does not close.
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let script_path = temp_dir.path().join("greet.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho hello\nsleep 60\n")
            .expect("failed to write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .expect("failed to set permissions");
        }

        let mut session = PtySession::spawn(&script_path).expect("failed to spawn greet script");

        // Act
        let frame = session
            .wait_for_stable_frame(Duration::from_millis(300), Duration::from_secs(5))
            .expect("frame should stabilize");

        // Assert
        let text = frame.all_text();
        assert!(
            text.contains("hello"),
            "stable frame should contain echoed output, got: '{text}'"
        );
    }
}
