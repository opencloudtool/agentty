use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncRead};

use crate::app::App;
use crate::db::Database;
use crate::git;
use crate::model::{SESSION_DATA_DIR, Status};

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
    ) {
        let mut tokio_cmd = tokio::process::Command::from(cmd);
        // Prevent the child process from inheriting the TUI's terminal on
        // stdin.  On macOS the child can otherwise disturb crossterm's raw-mode
        // settings, causing the event reader to stall and the UI to freeze.
        tokio_cmd.stdin(std::process::Stdio::null());
        tokio::spawn(async move {
            let file = std::fs::OpenOptions::new()
                .append(true)
                .open(folder.join(SESSION_DATA_DIR).join("output.txt"))
                .ok()
                .map(std::io::BufWriter::new);
            let file = Arc::new(Mutex::new(file));

            match tokio_cmd.spawn() {
                Ok(mut child) => {
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();

                    let mut handles = Vec::new();

                    if let Some(stdout) = stdout {
                        let output = Arc::clone(&output);
                        let file = Arc::clone(&file);
                        handles.push(tokio::spawn(async move {
                            Self::process_output(stdout, &file, &output).await;
                        }));
                    }
                    if let Some(stderr) = stderr {
                        let output = Arc::clone(&output);
                        let file = Arc::clone(&file);
                        handles.push(tokio::spawn(async move {
                            Self::process_output(stderr, &file, &output).await;
                        }));
                    }

                    for handle in handles {
                        let _ = handle.await;
                    }
                    let _ = child.wait().await;
                }
                Err(e) => {
                    if let Ok(mut buf) = output.lock() {
                        let _ = writeln!(buf, "Failed to spawn process: {e}");
                    }
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

    pub(super) async fn process_output<R: AsyncRead + Unpin>(
        source: R,
        file: &Arc<Mutex<Option<std::io::BufWriter<std::fs::File>>>>,
        output: &Arc<Mutex<String>>,
    ) {
        let mut reader = tokio::io::BufReader::new(source).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(mut f_guard) = file.lock()
                && let Some(f) = f_guard.as_mut()
            {
                let _ = writeln!(f, "{line}");
            }
            if let Ok(mut buf) = output.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    }
}
