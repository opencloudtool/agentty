//! Session task execution helpers for process running, output capture, and
//! status persistence.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncRead};

use crate::agent::AgentKind;
use crate::app::App;
use crate::db::Database;
use crate::git;
use crate::model::Status;

/// Inputs needed to execute one session command.
pub(super) struct RunSessionTaskInput {
    pub(super) agent: AgentKind,
    pub(super) cmd: Command,
    pub(super) commit_count: Arc<Mutex<i64>>,
    pub(super) db: Database,
    pub(super) folder: PathBuf,
    pub(super) id: String,
    pub(super) output: Arc<Mutex<String>>,
    pub(super) status: Arc<Mutex<Status>>,
}

impl App {
    /// Spawns a background loop that periodically refreshes ahead/behind info.
    pub(super) fn spawn_git_status_task(
        working_dir: &Path,
        git_status: Arc<Mutex<Option<(u32, u32)>>>,
        cancel: Arc<AtomicBool>,
    ) {
        let dir = working_dir.to_path_buf();
        tokio::spawn(async move {
            let repo_root = git::find_git_repo_root(&dir).unwrap_or(dir);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                {
                    let root = repo_root.clone();
                    let _ = tokio::task::spawn_blocking(move || git::fetch_remote(&root)).await;
                }
                let status = {
                    let root = repo_root.clone();
                    tokio::task::spawn_blocking(move || git::get_ahead_behind(&root))
                        .await
                        .ok()
                        .and_then(std::result::Result::ok)
                };
                if let Ok(mut lock) = git_status.lock() {
                    *lock = status;
                }
                for _ in 0..30 {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        });
    }

    /// Executes one agent command, captures output, persists stats, and
    /// commits.
    ///
    /// # Errors
    /// Returns an error when process spawning fails.
    pub(super) async fn run_session_task(input: RunSessionTaskInput) -> Result<(), String> {
        let RunSessionTaskInput {
            agent,
            cmd,
            commit_count,
            db,
            folder,
            id,
            output,
            status,
        } = input;

        let mut tokio_cmd = tokio::process::Command::from(cmd);
        // Prevent the child process from inheriting the TUI's terminal on
        // stdin. On macOS the child can otherwise disturb crossterm's raw-mode
        // settings, causing the event reader to stall and the UI to freeze.
        tokio_cmd.stdin(std::process::Stdio::null());

        let mut error: Option<String> = None;

        match tokio_cmd.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();

                let raw_stdout = Arc::new(Mutex::new(String::new()));
                let raw_stderr = Arc::new(Mutex::new(String::new()));
                let mut handles = Vec::new();

                if let Some(stdout) = stdout {
                    let buffer = Arc::clone(&raw_stdout);
                    handles.push(tokio::spawn(async move {
                        Self::capture_raw_output(stdout, &buffer).await;
                    }));
                }

                if let Some(stderr) = stderr {
                    let buffer = Arc::clone(&raw_stderr);
                    handles.push(tokio::spawn(async move {
                        Self::capture_raw_output(stderr, &buffer).await;
                    }));
                }

                for handle in handles {
                    let _ = handle.await;
                }
                let _ = child.wait().await;

                let stdout_text = raw_stdout.lock().map(|buf| buf.clone()).unwrap_or_default();
                let stderr_text = raw_stderr.lock().map(|buf| buf.clone()).unwrap_or_default();
                let parsed = agent.parse_response(&stdout_text, &stderr_text);
                Self::append_session_output(&output, &db, &id, &parsed.content).await;

                let _ = db.update_session_stats(&id, &parsed.stats).await;

                match Self::commit_changes(&folder, &db, &id, &commit_count).await {
                    Ok(hash) => {
                        let message = format!("\n[Commit] committed with hash `{hash}`\n");
                        Self::append_session_output(&output, &db, &id, &message).await;
                    }
                    Err(commit_error) if commit_error.contains("Nothing to commit") => {}
                    Err(commit_error) => {
                        let message = format!("\n[Commit Error] {commit_error}\n");
                        Self::append_session_output(&output, &db, &id, &message).await;
                    }
                }
            }
            Err(spawn_error) => {
                let message = format!("Failed to spawn process: {spawn_error}\n");
                Self::append_session_output(&output, &db, &id, &message).await;
                error = Some(message.trim().to_string());
            }
        }

        let _ = Self::update_status(&status, &db, &id, Status::Review).await;

        if let Some(error) = error {
            return Err(error);
        }

        Ok(())
    }

    /// Applies a status transition to memory and database when valid.
    pub(super) async fn update_status(
        status: &Mutex<Status>,
        db: &Database,
        id: &str,
        new: Status,
    ) -> bool {
        let should_update = if let Ok(mut current) = status.lock() {
            if (*current).can_transition_to(new) {
                *current = new;
                true
            } else {
                false
            }
        } else {
            false
        };
        if !should_update {
            return false;
        }
        let _ = db.update_session_status(id, &new.to_string()).await;

        true
    }

    /// Captures raw output from a stream into an in-memory buffer.
    pub(super) async fn capture_raw_output<R: AsyncRead + Unpin>(
        source: R,
        buffer: &Arc<Mutex<String>>,
    ) {
        let mut reader = tokio::io::BufReader::new(source).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(mut buf) = buffer.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    }

    /// Appends output to the in-memory handle buffer and database.
    pub(super) async fn append_session_output(
        output: &Arc<Mutex<String>>,
        db: &Database,
        id: &str,
        message: &str,
    ) {
        if let Ok(mut buf) = output.lock() {
            buf.push_str(message);
        }
        let _ = db.append_session_output(id, message).await;
    }
}
