//! Pull-request lifecycle orchestration for session branches.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::agent::AgentKind;
use crate::app::session::session_branch;
use crate::app::task::TaskService;
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::db::Database;
use crate::git;
use crate::model::Status;

const PR_MERGE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Pull request runtime state shared by PR creation and polling tasks.
pub struct PrManager {
    pr_creation_in_flight: Arc<Mutex<HashSet<String>>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl PrManager {
    /// Creates an empty PR runtime state container.
    pub fn new() -> Self {
        Self {
            pr_creation_in_flight: Arc::new(Mutex::new(HashSet::new())),
            pr_poll_cancel: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns the shared set of session ids currently creating PRs.
    pub(crate) fn pr_creation_in_flight(&self) -> Arc<Mutex<HashSet<String>>> {
        Arc::clone(&self.pr_creation_in_flight)
    }

    /// Returns the shared PR polling cancel-token map.
    pub(crate) fn pr_poll_cancel(&self) -> Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> {
        Arc::clone(&self.pr_poll_cancel)
    }
}

impl Default for PrManager {
    fn default() -> Self {
        Self::new()
    }
}

struct PrPollTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    db: Database,
    folder: PathBuf,
    id: String,
    output: Arc<Mutex<String>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    repo_root: Option<PathBuf>,
    session_agent: AgentKind,
    session_model: String,
    source_branch: String,
    status: Arc<Mutex<Status>>,
}

struct CreatePrTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    db: Database,
    folder: PathBuf,
    id: String,
    output: Arc<Mutex<String>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    session_agent: AgentKind,
    session_model: String,
    source_branch: String,
    status: Arc<Mutex<Status>>,
    title: String,
}

struct PrPollShared<'a> {
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    db: &'a Database,
    id: &'a str,
    output: &'a Arc<Mutex<String>>,
    status: &'a Arc<Mutex<Status>>,
}

impl PrManager {
    /// Creates a pull request for a reviewed session branch.
    ///
    /// # Errors
    /// Returns an error if the session is not eligible for PR creation or git
    /// metadata for the worktree is unavailable.
    pub(super) async fn create_pr_session(
        &self,
        services: &AppServices,
        sessions: &SessionManager,
        session_id: &str,
    ) -> Result<(), String> {
        let (session, handles) = sessions.session_and_handles_or_err(session_id)?;

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
        let base_branch = services
            .db()
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
        let session_agent = session
            .agent
            .parse::<AgentKind>()
            .unwrap_or(AgentKind::Gemini);
        let session_model = session_agent
            .parse_model(&session.model)
            .unwrap_or_else(|| session_agent.default_model())
            .as_str()
            .to_string();

        self.mark_pr_creation_in_flight(&session.id)?;

        let status = Arc::clone(&handles.status);
        let output = Arc::clone(&handles.output);
        let folder = session.folder.clone();
        let db = services.db().clone();
        let id = session.id.clone();
        let app_event_tx = services.event_sender();
        if !TaskService::update_status(
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

        let pr_poll_cancel = self.pr_poll_cancel();

        Self::spawn_create_pr_task(CreatePrTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            id,
            output,
            pr_poll_cancel,
            session_agent,
            session_model,
            source_branch,
            status,
            title,
        });

        Ok(())
    }

    fn spawn_create_pr_task(input: CreatePrTaskInput) {
        tokio::spawn(async move {
            let result = Self::run_create_pr_command(&input).await;
            Self::finalize_create_pr_task(input, result).await;
        });
    }

    async fn run_create_pr_command(
        input: &CreatePrTaskInput,
    ) -> Result<Result<String, String>, tokio::task::JoinError> {
        let folder = input.folder.clone();
        let source_branch = input.source_branch.clone();
        let base_branch = input.base_branch.clone();
        let title = input.title.clone();

        tokio::task::spawn_blocking(move || {
            git::create_pr(&folder, &source_branch, &base_branch, &title)
        })
        .await
    }

    async fn finalize_create_pr_task(
        input: CreatePrTaskInput,
        result: Result<Result<String, String>, tokio::task::JoinError>,
    ) {
        let CreatePrTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            id,
            output,
            pr_poll_cancel,
            session_agent,
            session_model,
            source_branch,
            status,
            title: _,
        } = input;

        match result {
            Ok(Ok(message)) => {
                let message = format!("\n[PR] {message}\n");
                TaskService::append_session_output(&output, &db, &app_event_tx, &id, &message)
                    .await;

                if TaskService::update_status(&status, &db, &app_event_tx, &id, Status::PullRequest)
                    .await
                {
                    let repo_root = SessionManager::resolve_repo_root_from_worktree(&folder);
                    Self::spawn_pr_poll_task(PrPollTaskInput {
                        app_event_tx: app_event_tx.clone(),
                        base_branch,
                        db,
                        folder,
                        id: id.clone(),
                        output,
                        pr_poll_cancel,
                        repo_root,
                        session_agent,
                        session_model,
                        source_branch,
                        status,
                    });
                } else {
                    TaskService::append_session_output(
                        &output,
                        &db,
                        &app_event_tx,
                        &id,
                        "\n[PR Error] Invalid status transition to PullRequest\n",
                    )
                    .await;
                    Self::set_review_status(&status, &db, &app_event_tx, &id).await;
                }
            }
            Ok(Err(error)) => {
                let message = format!("\n[PR Error] {error}\n");
                TaskService::append_session_output(&output, &db, &app_event_tx, &id, &message)
                    .await;
                Self::set_review_status(&status, &db, &app_event_tx, &id).await;
            }
            Err(error) => {
                let message = format!("\n[PR Error] Join error: {error}\n");
                TaskService::append_session_output(&output, &db, &app_event_tx, &id, &message)
                    .await;
                Self::set_review_status(&status, &db, &app_event_tx, &id).await;
            }
        }

        let _ = app_event_tx.send(AppEvent::PrCreationCleared { session_id: id });
    }

    async fn set_review_status(
        status: &Arc<Mutex<Status>>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
    ) {
        let _ = TaskService::update_status(status, db, app_event_tx, id, Status::Review).await;
    }

    /// Ensures merge polling tasks are running for sessions in
    /// `PullRequest` status.
    pub(super) fn start_pr_polling_for_pull_request_sessions(
        &self,
        services: &AppServices,
        sessions: &SessionManager,
    ) {
        for session in &sessions.sessions {
            if session.status != Status::PullRequest {
                continue;
            }

            let Some(handles) = sessions.handles.get(&session.id) else {
                continue;
            };

            let source_branch = session_branch(&session.id);
            let session_agent = session
                .agent
                .parse::<AgentKind>()
                .unwrap_or(AgentKind::Gemini);
            let session_model = session_agent
                .parse_model(&session.model)
                .unwrap_or_else(|| session_agent.default_model())
                .as_str()
                .to_string();
            Self::spawn_pr_poll_task(PrPollTaskInput {
                app_event_tx: services.event_sender(),
                base_branch: session.base_branch.clone(),
                db: services.db().clone(),
                folder: session.folder.clone(),
                id: session.id.clone(),
                output: Arc::clone(&handles.output),
                pr_poll_cancel: self.pr_poll_cancel(),
                repo_root: SessionManager::resolve_repo_root_from_worktree(&session.folder),
                session_agent,
                session_model,
                source_branch,
                status: Arc::clone(&handles.status),
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

    /// Removes a session identifier from the in-flight PR creation set.
    pub(super) fn clear_pr_creation_in_flight(&self, id: &str) {
        if let Ok(mut in_flight) = self.pr_creation_in_flight.lock() {
            in_flight.remove(id);
        }
    }

    /// Requests cancellation of PR polling for a single session.
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
            base_branch,
            db,
            folder,
            id,
            output,
            pr_poll_cancel,
            repo_root,
            session_agent,
            session_model,
            source_branch,
            status,
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

                let shared = PrPollShared {
                    app_event_tx: &app_event_tx,
                    db: &db,
                    id: &id,
                    output: &output,
                    status: &status,
                };

                if Self::poll_is_merged(&folder, &source_branch).await == Some(true) {
                    Self::handle_merged_pr(
                        &shared,
                        &folder,
                        &source_branch,
                        repo_root.as_deref(),
                        &base_branch,
                        session_agent,
                        &session_model,
                    )
                    .await;
                    break;
                }

                if Self::poll_is_closed(&folder, &source_branch).await == Some(true) {
                    Self::handle_closed_pr(
                        &shared,
                        &folder,
                        &source_branch,
                        &base_branch,
                        session_agent,
                        &session_model,
                    )
                    .await;
                    break;
                }

                Self::wait_for_next_poll(&cancel).await;
            }

            let _ = app_event_tx.send(AppEvent::PrPollingStopped { session_id: id });
        });
    }

    async fn poll_is_merged(folder: &Path, source_branch: &str) -> Option<bool> {
        let folder = folder.to_path_buf();
        let source_branch = source_branch.to_string();
        tokio::task::spawn_blocking(move || git::is_pr_merged(&folder, &source_branch))
            .await
            .ok()
            .and_then(std::result::Result::ok)
    }

    async fn poll_is_closed(folder: &Path, source_branch: &str) -> Option<bool> {
        let folder = folder.to_path_buf();
        let source_branch = source_branch.to_string();
        tokio::task::spawn_blocking(move || git::is_pr_closed(&folder, &source_branch))
            .await
            .ok()
            .and_then(std::result::Result::ok)
    }

    async fn handle_merged_pr(
        shared: &PrPollShared<'_>,
        folder: &Path,
        source_branch: &str,
        repo_root: Option<&Path>,
        base_branch: &str,
        session_agent: AgentKind,
        session_model: &str,
    ) {
        let merged_message = format!("\n[PR] Pull request from `{source_branch}` was merged\n");
        TaskService::append_session_output(
            shared.output,
            shared.db,
            shared.app_event_tx,
            shared.id,
            &merged_message,
        )
        .await;
        let summary_diff = SessionManager::session_diff_for_summary(folder, base_branch).await;
        let moved_to_done = TaskService::update_status(
            shared.status,
            shared.db,
            shared.app_event_tx,
            shared.id,
            Status::Done,
        )
        .await;

        if !moved_to_done {
            TaskService::append_session_output(
                shared.output,
                shared.db,
                shared.app_event_tx,
                shared.id,
                "\n[PR Error] Invalid status transition to Done\n",
            )
            .await;
        }

        let summary_folder = repo_root.map_or_else(|| folder.to_path_buf(), Path::to_path_buf);
        if let Err(error) = SessionManager::cleanup_merged_session_worktree(
            folder.to_path_buf(),
            source_branch.to_string(),
            repo_root.map(Path::to_path_buf),
        )
        .await
        {
            let message = format!("\n[PR Error] Failed to remove merged worktree: {error}\n");
            TaskService::append_session_output(
                shared.output,
                shared.db,
                shared.app_event_tx,
                shared.id,
                &message,
            )
            .await;
        }

        if moved_to_done {
            SessionManager::update_terminal_session_summary_from_diff(
                shared.db,
                shared.id,
                &summary_folder,
                &summary_diff,
                session_agent,
                session_model,
                Status::Done,
            )
            .await;
        }
    }

    async fn handle_closed_pr(
        shared: &PrPollShared<'_>,
        folder: &Path,
        source_branch: &str,
        base_branch: &str,
        session_agent: AgentKind,
        session_model: &str,
    ) {
        let closed_message = format!("\n[PR] Pull request from `{source_branch}` was closed\n");
        TaskService::append_session_output(
            shared.output,
            shared.db,
            shared.app_event_tx,
            shared.id,
            &closed_message,
        )
        .await;
        let moved_to_canceled = TaskService::update_status(
            shared.status,
            shared.db,
            shared.app_event_tx,
            shared.id,
            Status::Canceled,
        )
        .await;

        if moved_to_canceled {
            SessionManager::update_terminal_session_summary(
                shared.db,
                shared.id,
                folder,
                base_branch,
                session_agent,
                session_model,
                Status::Canceled,
            )
            .await;
        } else {
            TaskService::append_session_output(
                shared.output,
                shared.db,
                shared.app_event_tx,
                shared.id,
                "\n[PR Error] Invalid status transition to Canceled\n",
            )
            .await;
        }
    }

    async fn wait_for_next_poll(cancel: &AtomicBool) {
        for _ in 0..PR_MERGE_POLL_INTERVAL.as_secs() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
