use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncRead};

use crate::agent::AgentKind;
use crate::app::App;
use crate::db::Database;
use crate::git;
use crate::model::{Session, Status};

impl App {
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

    pub(super) fn spawn_session_task(
        folder: PathBuf,
        cmd: Command,
        output: Arc<Mutex<String>>,
        status: Arc<Mutex<Status>>,
        db: Database,
        id: String,
        agent: AgentKind,
    ) {
        let mut tokio_cmd = tokio::process::Command::from(cmd);
        // Prevent the child process from inheriting the TUI's terminal on
        // stdin.  On macOS the child can otherwise disturb crossterm's raw-mode
        // settings, causing the event reader to stall and the UI to freeze.
        tokio_cmd.stdin(std::process::Stdio::null());
        tokio::spawn(async move {
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
                    Self::append_session_output(&output, &folder, &db, &id, &parsed).await;
                }
                Err(e) => {
                    let message = format!("Failed to spawn process: {e}\n");
                    Self::append_session_output(&output, &folder, &db, &id, &message).await;
                }
            }

            Self::update_status(&status, &db, &id, Status::Review).await;
        });
    }

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

    pub(super) async fn append_session_output(
        output: &Arc<Mutex<String>>,
        folder: &Path,
        db: &Database,
        id: &str,
        message: &str,
    ) {
        Session::write_output(output, folder, message);
        let _ = db.append_session_output(id, message).await;
    }
}
