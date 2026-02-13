use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::acp::AcpSessionHandle;
use crate::app::{AgentEvent, App};
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

    /// Sends a prompt to an active ACP session handle and persists the result.
    ///
    /// Streaming output is written to the shared `output` buffer by the
    /// `AgenttyClient::session_notification` callback. When the prompt
    /// completes, the final output snapshot is persisted to the database.
    pub(super) fn spawn_acp_prompt_task(
        handle: Arc<AcpSessionHandle>,
        prompt: String,
        agent_tx: mpsc::UnboundedSender<AgentEvent>,
        id: String,
    ) {
        tokio::spawn(async move {
            match handle.prompt(&prompt).await {
                Ok(response) => {
                    let (input_tokens, output_tokens) = if let Some(usage) = response.usage {
                        (
                            Some(i64::try_from(usage.input_tokens).unwrap_or(0)),
                            Some(i64::try_from(usage.output_tokens).unwrap_or(0)),
                        )
                    } else {
                        (None, None)
                    };

                    let _ = agent_tx.send(AgentEvent::Finished {
                        session_id: id,
                        input_tokens,
                        output_tokens,
                    });
                }
                Err(error) => {
                    let _ = agent_tx.send(AgentEvent::Error {
                        session_id: id,
                        error,
                    });
                }
            }
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
