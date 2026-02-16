use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::app::session::session_branch;
use crate::app::{App, AppEvent};
use crate::db::Database;
use crate::git;
use crate::model::Status;

const PR_MERGE_POLL_INTERVAL: Duration = Duration::from_secs(30);

struct PrPollTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    db: Database,
    folder: PathBuf,
    id: String,
    output: Arc<Mutex<String>>,
    status: Arc<Mutex<Status>>,
    source_branch: String,
    repo_root: Option<PathBuf>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

struct CreatePrTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    db: Database,
    folder: PathBuf,
    id: String,
    output: Arc<Mutex<String>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    source_branch: String,
    status: Arc<Mutex<Status>>,
    title: String,
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

        if session.status != Status::Review {
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
            .get_session_base_branch(&session.id)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Build source branch name
        let source_branch = session_branch(&session.id);

        // Build PR title from session prompt (first line only)
        let title = session
            .prompt
            .lines()
            .next()
            .unwrap_or("New Session")
            .to_string();

        let handles = self
            .session_state
            .handles
            .get(&session.id)
            .ok_or_else(|| "Session handles not found".to_string())?;

        self.mark_pr_creation_in_flight(&session.id)?;

        let status = Arc::clone(&handles.status);
        let output = Arc::clone(&handles.output);
        let folder = session.folder.clone();
        let db = self.db.clone();
        let id = session.id.clone();
        let app_event_tx = self.app_event_sender();
        if !Self::update_status(
            &status,
            &db,
            &app_event_tx,
            &id,
            Status::CreatingPullRequest,
        )
        .await
        {
            self.clear_pr_creation_in_flight(&id);

            return Err("Invalid status transition to CreatingPullRequest".to_string());
        }

        let pr_poll_cancel = Arc::clone(&self.pr_poll_cancel);

        Self::spawn_create_pr_task(CreatePrTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            id,
            output,
            pr_poll_cancel,
            source_branch,
            status,
            title,
        });

        Ok(())
    }

    fn spawn_create_pr_task(input: CreatePrTaskInput) {
        let CreatePrTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            id,
            output,
            pr_poll_cancel,
            source_branch,
            status,
            title,
        } = input;

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
                    let message = format!("\n[PR] {message}\n");
                    Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
                    if Self::update_status(&status, &db, &app_event_tx, &id, Status::PullRequest)
                        .await
                    {
                        let repo_root = Self::resolve_repo_root_from_worktree(&folder);
                        Self::spawn_pr_poll_task(PrPollTaskInput {
                            app_event_tx: app_event_tx.clone(),
                            db,
                            folder,
                            id: id.clone(),
                            output,
                            status,
                            source_branch,
                            repo_root,
                            pr_poll_cancel,
                        });
                    } else {
                        Self::append_session_output(
                            &output,
                            &db,
                            &app_event_tx,
                            &id,
                            "\n[PR Error] Invalid status transition to PullRequest\n",
                        )
                        .await;
                        let _ =
                            Self::update_status(&status, &db, &app_event_tx, &id, Status::Review)
                                .await;
                    }
                }
                Ok(Err(error)) => {
                    let message = format!("\n[PR Error] {error}\n");
                    Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
                    let _ =
                        Self::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;
                }
                Err(error) => {
                    let message = format!("\n[PR Error] Join error: {error}\n");
                    Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
                    let _ =
                        Self::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;
                }
            }

            let _ = app_event_tx.send(AppEvent::PrCreationCleared { session_id: id });
        });
    }

    pub(super) fn start_pr_polling_for_pull_request_sessions(&self) {
        for session in &self.session_state.sessions {
            if session.status != Status::PullRequest {
                continue;
            }

            let Some(handles) = self.session_state.handles.get(&session.id) else {
                continue;
            };

            let source_branch = session_branch(&session.id);
            Self::spawn_pr_poll_task(PrPollTaskInput {
                app_event_tx: self.app_event_sender(),
                db: self.db.clone(),
                folder: session.folder.clone(),
                id: session.id.clone(),
                output: Arc::clone(&handles.output),
                status: Arc::clone(&handles.status),
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
            app_event_tx,
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
                    let merged_message =
                        format!("\n[PR] Pull request from `{source_branch}` was merged\n");
                    Self::append_session_output(&output, &db, &app_event_tx, &id, &merged_message)
                        .await;
                    if !Self::update_status(&status, &db, &app_event_tx, &id, Status::Done).await {
                        Self::append_session_output(
                            &output,
                            &db,
                            &app_event_tx,
                            &id,
                            "\n[PR Error] Invalid status transition to Done\n",
                        )
                        .await;
                    }
                    if let Err(error) = Self::cleanup_merged_session_worktree(
                        folder.clone(),
                        source_branch.clone(),
                        repo_root.clone(),
                    )
                    .await
                    {
                        let message =
                            format!("\n[PR Error] Failed to remove merged worktree: {error}\n");
                        Self::append_session_output(&output, &db, &app_event_tx, &id, &message)
                            .await;
                    }

                    break;
                }

                let closed = {
                    let folder = folder.clone();
                    let source_branch = source_branch.clone();
                    tokio::task::spawn_blocking(move || git::is_pr_closed(&folder, &source_branch))
                        .await
                        .ok()
                        .and_then(std::result::Result::ok)
                };

                if closed == Some(true) {
                    let closed_message =
                        format!("\n[PR] Pull request from `{source_branch}` was closed\n");
                    Self::append_session_output(&output, &db, &app_event_tx, &id, &closed_message)
                        .await;
                    if !Self::update_status(&status, &db, &app_event_tx, &id, Status::Canceled)
                        .await
                    {
                        Self::append_session_output(
                            &output,
                            &db,
                            &app_event_tx,
                            &id,
                            "\n[PR Error] Invalid status transition to Canceled\n",
                        )
                        .await;
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

            let _ = app_event_tx.send(AppEvent::PrPollingStopped { session_id: id });
        });
    }
}
