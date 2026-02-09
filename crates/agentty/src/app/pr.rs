use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::app::App;
use crate::db::Database;
use crate::git;
use crate::model::{Session, Status};

const PR_MERGE_POLL_INTERVAL: Duration = Duration::from_secs(30);

struct PrPollTaskInput {
    db: Database,
    folder: PathBuf,
    id: String,
    output: Arc<Mutex<String>>,
    status: Arc<Mutex<Status>>,
    source_branch: String,
    repo_root: Option<PathBuf>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl App {
    /// Creates a pull request for a reviewed session branch.
    ///
    /// # Errors
    /// Returns an error if the session is not eligible for PR creation or git
    /// metadata for the worktree is unavailable.
    pub async fn create_pr_session(&self, session_id: &str) -> Result<(), String> {
        let session = self
            .session_state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| "Session not found".to_string())?;

        if session.status() != Status::Review {
            return Err("Session must be in review to create a pull request".to_string());
        }

        if self.is_pr_creation_in_flight(&session.id) {
            return Err("Pull request creation is already in progress".to_string());
        }

        if self.is_pr_polling_active(&session.id) {
            return Err("Pull request is already being tracked".to_string());
        }

        // Read base branch from DB
        let base_branch = self
            .db
            .get_base_branch(&session.id)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Build source branch name
        let source_branch = format!("agentty/{}", session.id);

        // Build PR title from session prompt (first line only)
        let title = session
            .prompt
            .lines()
            .next()
            .unwrap_or("New Session")
            .to_string();

        self.mark_pr_creation_in_flight(&session.id)?;

        let status = Arc::clone(&session.status);
        let output = Arc::clone(&session.output);
        let folder = session.folder.clone();
        let db = self.db.clone();
        let name = session.id.clone();
        let repo_url = {
            let folder = folder.clone();
            match tokio::task::spawn_blocking(move || git::repo_url(&folder)).await {
                Ok(Ok(url)) => url,
                _ => "this repository".to_string(),
            }
        };

        Session::write_output(
            &output,
            &folder,
            &format!("\n[PR] Creating PR in {repo_url}\n"),
        );

        let pr_creation_in_flight = Arc::clone(&self.pr_creation_in_flight);
        let pr_poll_cancel = Arc::clone(&self.pr_poll_cancel);

        // Perform PR creation in background
        tokio::spawn(async move {
            let result = {
                let folder = folder.clone();
                let source_branch = source_branch.clone();
                tokio::task::spawn_blocking(move || {
                    git::create_pr(&folder, &source_branch, &base_branch, &title)
                })
                .await
            };

            match result {
                Ok(Ok(message)) => {
                    Session::write_output(&output, &folder, &format!("\n[PR] {message}\n"));
                    if Self::update_status(&status, &db, &name, Status::PullRequest).await {
                        let repo_root = Self::resolve_repo_root_from_worktree(&folder);
                        Self::spawn_pr_poll_task(PrPollTaskInput {
                            db,
                            folder,
                            id: name.clone(),
                            output,
                            status,
                            source_branch,
                            repo_root,
                            pr_poll_cancel,
                        });
                    } else {
                        Session::write_output(
                            &output,
                            &folder,
                            "\n[PR Error] Invalid status transition to PullRequest\n",
                        );
                    }
                }
                Ok(Err(error)) => {
                    Session::write_output(&output, &folder, &format!("\n[PR Error] {error}\n"));
                }
                Err(error) => {
                    Session::write_output(
                        &output,
                        &folder,
                        &format!("\n[PR Error] Join error: {error}\n"),
                    );
                }
            }

            if let Ok(mut in_flight) = pr_creation_in_flight.lock() {
                in_flight.remove(&name);
            }
        });

        Ok(())
    }

    pub(super) fn start_pr_polling_for_pull_request_sessions(&self) {
        for session in &self.session_state.sessions {
            if session.status() != Status::PullRequest {
                continue;
            }

            let source_branch = format!("agentty/{}", session.id);
            Self::spawn_pr_poll_task(PrPollTaskInput {
                db: self.db.clone(),
                folder: session.folder.clone(),
                id: session.id.clone(),
                output: Arc::clone(&session.output),
                status: Arc::clone(&session.status),
                source_branch,
                repo_root: Self::resolve_repo_root_from_worktree(&session.folder),
                pr_poll_cancel: Arc::clone(&self.pr_poll_cancel),
            });
        }
    }

    fn mark_pr_creation_in_flight(&self, id: &str) -> Result<(), String> {
        let mut in_flight = self
            .pr_creation_in_flight
            .lock()
            .map_err(|_| "Failed to lock PR creation state".to_string())?;

        if in_flight.contains(id) {
            return Err("Pull request creation is already in progress".to_string());
        }

        in_flight.insert(id.to_string());

        Ok(())
    }

    fn is_pr_creation_in_flight(&self, id: &str) -> bool {
        self.pr_creation_in_flight
            .lock()
            .is_ok_and(|in_flight| in_flight.contains(id))
    }

    fn is_pr_polling_active(&self, id: &str) -> bool {
        self.pr_poll_cancel
            .lock()
            .is_ok_and(|polling| polling.contains_key(id))
    }

    pub(super) fn clear_pr_creation_in_flight(&self, id: &str) {
        if let Ok(mut in_flight) = self.pr_creation_in_flight.lock() {
            in_flight.remove(id);
        }
    }

    pub(super) fn cancel_pr_polling_for_session(&self, id: &str) {
        if let Ok(mut polling) = self.pr_poll_cancel.lock()
            && let Some(cancel) = polling.remove(id)
        {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn spawn_pr_poll_task(input: PrPollTaskInput) {
        let PrPollTaskInput {
            db,
            folder,
            id,
            output,
            status,
            source_branch,
            repo_root,
            pr_poll_cancel,
        } = input;

        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut polling) = pr_poll_cancel.lock() {
            if polling.contains_key(&id) {
                return;
            }
            polling.insert(id.clone(), Arc::clone(&cancel));
        } else {
            return;
        }

        tokio::spawn(async move {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }

                let merged = {
                    let folder = folder.clone();
                    let source_branch = source_branch.clone();
                    tokio::task::spawn_blocking(move || git::is_pr_merged(&folder, &source_branch))
                        .await
                        .ok()
                        .and_then(std::result::Result::ok)
                };

                if merged == Some(true) {
                    Session::write_output(
                        &output,
                        &folder,
                        &format!("\n[PR] Pull request from `{source_branch}` was merged\n"),
                    );
                    if !Self::update_status(&status, &db, &id, Status::Done).await {
                        Session::write_output(
                            &output,
                            &folder,
                            "\n[PR Error] Invalid status transition to Done\n",
                        );
                    }
                    if let Err(error) = Self::cleanup_merged_session_worktree(
                        folder.clone(),
                        source_branch.clone(),
                        repo_root.clone(),
                    )
                    .await
                    {
                        Session::write_output(
                            &output,
                            &folder,
                            &format!("\n[PR Error] Failed to remove merged worktree: {error}\n"),
                        );
                    }

                    break;
                }

                for _ in 0..PR_MERGE_POLL_INTERVAL.as_secs() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }

            if let Ok(mut polling) = pr_poll_cancel.lock() {
                polling.remove(&id);
            }
        });
    }
}
