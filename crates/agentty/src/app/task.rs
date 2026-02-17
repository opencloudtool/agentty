//! Session task execution helpers for process running, output capture, and
//! status persistence.

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncRead};
use tokio::sync::mpsc;

use crate::agent::AgentKind;
use crate::app::{AppEvent, SessionManager};
use crate::db::Database;
use crate::git;
use crate::model::{PermissionMode, Status};

/// Stateless helpers for background tasks and session process output handling.
pub(super) struct TaskService;

/// Inputs needed to execute one session command.
pub(super) struct RunSessionTaskInput {
    pub(super) agent: AgentKind,
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    pub(super) cmd: Command,
    pub(super) commit_count: Arc<Mutex<i64>>,
    pub(super) db: Database,
    pub(super) folder: PathBuf,
    pub(super) id: String,
    pub(super) output: Arc<Mutex<String>>,
    pub(super) permission_mode: PermissionMode,
    pub(super) status: Arc<Mutex<Status>>,
}

impl TaskService {
    /// Spawns a background loop that periodically refreshes ahead/behind info.
    ///
    /// The task emits [`AppEvent::GitStatusUpdated`] snapshots instead of
    /// mutating app state directly.
    pub(super) fn spawn_git_status_task(
        working_dir: &Path,
        cancel: Arc<AtomicBool>,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
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
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = app_event_tx.send(AppEvent::GitStatusUpdated { status });
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
            app_event_tx,
            child_pid,
            cmd,
            commit_count,
            db,
            folder,
            id,
            output,
            permission_mode,
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
                if let Some(pid) = child.id()
                    && let Ok(mut guard) = child_pid.lock()
                {
                    *guard = Some(pid);
                }

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
                let exit_status = child.wait().await.ok();

                if let Ok(mut guard) = child_pid.lock() {
                    *guard = None;
                }

                let killed_by_signal = exit_status
                    .as_ref()
                    .is_some_and(|status| status.signal().is_some());

                if killed_by_signal {
                    let message = "\n[Stopped] Agent interrupted by user.\n";
                    Self::append_session_output(&output, &db, &app_event_tx, &id, message).await;
                } else {
                    let stdout_text = raw_stdout.lock().map(|buf| buf.clone()).unwrap_or_default();
                    let stderr_text = raw_stderr.lock().map(|buf| buf.clone()).unwrap_or_default();
                    let parsed = agent.parse_response(&stdout_text, &stderr_text, permission_mode);
                    Self::append_session_output(&output, &db, &app_event_tx, &id, &parsed.content)
                        .await;

                    let _ = db.update_session_stats(&id, &parsed.stats).await;

                    match SessionManager::commit_changes(&folder, &db, &id, &commit_count).await {
                        Ok(hash) => {
                            let message = format!("\n[Commit] committed with hash `{hash}`\n");
                            Self::append_session_output(&output, &db, &app_event_tx, &id, &message)
                                .await;
                        }
                        Err(commit_error) if commit_error.contains("Nothing to commit") => {}
                        Err(commit_error) => {
                            let message = format!("\n[Commit Error] {commit_error}\n");
                            Self::append_session_output(&output, &db, &app_event_tx, &id, &message)
                                .await;
                        }
                    }
                }
            }
            Err(spawn_error) => {
                let message = format!("Failed to spawn process: {spawn_error}\n");
                Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
                error = Some(message.trim().to_string());
            }
        }

        let _ = Self::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;

        if let Some(error) = error {
            return Err(error);
        }

        Ok(())
    }

    /// Applies a status transition to memory and database when valid.
    ///
    /// This emits [`AppEvent::SessionUpdated`] for targeted snapshot sync and
    /// emits [`AppEvent::RefreshSessions`] for transitions that require full
    /// list reload.
    pub(super) async fn update_status(
        status: &Mutex<Status>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
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
        let session_id = id.to_string();
        let _ = app_event_tx.send(AppEvent::SessionUpdated { session_id });
        if Self::status_requires_full_refresh(new) {
            let _ = app_event_tx.send(AppEvent::RefreshSessions);
        }

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
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        message: &str,
    ) {
        if let Ok(mut buf) = output.lock() {
            buf.push_str(message);
        }
        let _ = db.append_session_output(id, message).await;
        let _ = app_event_tx.send(AppEvent::SessionUpdated {
            session_id: id.to_string(),
        });
    }

    fn status_requires_full_refresh(status: Status) -> bool {
        matches!(
            status,
            Status::InProgress
                | Status::Review
                | Status::Merging
                | Status::CreatingPullRequest
                | Status::PullRequest
                | Status::Done
                | Status::Canceled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_requires_full_refresh_for_lifecycle_statuses() {
        // Arrange
        let refresh_statuses = [
            Status::InProgress,
            Status::Review,
            Status::Merging,
            Status::CreatingPullRequest,
            Status::PullRequest,
            Status::Done,
            Status::Canceled,
        ];

        // Act & Assert
        for status in refresh_statuses {
            assert!(TaskService::status_requires_full_refresh(status));
        }
        assert!(!TaskService::status_requires_full_refresh(Status::New));
    }
}
