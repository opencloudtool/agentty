use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::widgets::TableState;
use tokio::io::{AsyncBufReadExt as _, AsyncRead};
use uuid::Uuid;

use crate::agent::{AgentBackend, AgentKind, AgentModel};
use crate::db::Database;
use crate::git;
use crate::health::{self, HealthEntry};
use crate::model::{AppMode, Project, SESSION_DATA_DIR, Session, Status, Tab};

pub const AGENTTY_WORKSPACE: &str = "/var/tmp/.agentty";
const PR_MERGE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
type SessionHandles = (Arc<Mutex<String>>, Arc<Mutex<Status>>);

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

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    refresh_deadline: Instant,
    row_count: i64,
    updated_at_max: i64,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    pub fn new(
        sessions: Vec<Session>,
        table_state: TableState,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        Self {
            sessions,
            table_state,
            refresh_deadline: Instant::now() + SESSION_REFRESH_INTERVAL,
            row_count,
            updated_at_max,
        }
    }
}

pub struct App {
    pub current_tab: Tab,
    pub mode: AppMode,
    pub projects: Vec<Project>,
    pub session_state: SessionState,
    active_project_id: i64,
    base_path: PathBuf,
    db: Database,
    git_branch: Option<String>,
    git_status: Arc<Mutex<Option<(u32, u32)>>>,
    git_status_cancel: Arc<AtomicBool>,
    health_checks: Arc<Mutex<Vec<HealthEntry>>>,
    pr_creation_in_flight: Arc<Mutex<HashSet<String>>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    working_dir: PathBuf,
}

impl App {
    pub async fn new(
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
    ) -> Self {
        let active_project_id = db
            .upsert_project(&working_dir.to_string_lossy(), git_branch.as_deref())
            .await
            .unwrap_or(0);

        let _ = db.backfill_sessions_project(active_project_id).await;

        Self::discover_sibling_projects(&working_dir, &db).await;

        let projects = Self::load_projects_from_db(&db).await;

        let mut table_state = TableState::default();
        let sessions = Self::load_sessions(&base_path, &db, &projects, &[]).await;
        let (sessions_row_count, sessions_updated_at_max) =
            db.load_sessions_metadata().await.unwrap_or((0, 0));
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }

        let git_status = Arc::new(Mutex::new(None));
        let git_status_cancel = Arc::new(AtomicBool::new(false));
        let pr_creation_in_flight = Arc::new(Mutex::new(HashSet::new()));
        let pr_poll_cancel = Arc::new(Mutex::new(HashMap::new()));

        if git_branch.is_some() {
            Self::spawn_git_status_task(
                &working_dir,
                Arc::clone(&git_status),
                Arc::clone(&git_status_cancel),
            );
        }

        let app = Self {
            current_tab: Tab::Sessions,
            mode: AppMode::List,
            session_state: SessionState::new(
                sessions,
                table_state,
                sessions_row_count,
                sessions_updated_at_max,
            ),
            active_project_id,
            base_path,
            db,
            git_branch,
            git_status,
            git_status_cancel,
            health_checks: Arc::new(Mutex::new(Vec::new())),
            pr_creation_in_flight,
            pr_poll_cancel,
            projects,
            working_dir,
        };

        app.start_pr_polling_for_pull_request_sessions();
        app
    }

    pub fn active_project_id(&self) -> i64 {
        self.active_project_id
    }

    pub fn working_dir(&self) -> &PathBuf {
        &self.working_dir
    }

    pub fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.git_status.lock().ok().and_then(|s| *s)
    }

    pub fn health_checks(&self) -> &Arc<Mutex<Vec<HealthEntry>>> {
        &self.health_checks
    }

    pub fn start_health_checks(&mut self) {
        self.health_checks = health::run_health_checks(self.git_branch.clone());
    }

    /// Switches the active project context and reloads project sessions.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or session state cannot
    /// be reloaded from persisted storage.
    pub async fn switch_project(&mut self, project_id: i64) -> Result<(), String> {
        let project = self
            .db
            .get_project(project_id)
            .await?
            .ok_or_else(|| "Project not found".to_string())?;

        // Cancel existing git status task
        self.git_status_cancel.store(true, Ordering::Relaxed);

        // Update working dir and git info
        self.working_dir = PathBuf::from(&project.path);
        self.git_branch.clone_from(&project.git_branch);
        self.active_project_id = project_id;

        // Reset git status
        if let Ok(mut status) = self.git_status.lock() {
            *status = None;
        }

        // Start new git status task
        let new_cancel = Arc::new(AtomicBool::new(false));
        self.git_status_cancel = new_cancel.clone();
        if self.git_branch.is_some() {
            Self::spawn_git_status_task(
                &self.working_dir,
                Arc::clone(&self.git_status),
                new_cancel,
            );
        }

        // Refresh project list and reload all sessions
        self.projects = Self::load_projects_from_db(&self.db).await;
        let existing_sessions = std::mem::take(&mut self.session_state.sessions);
        self.session_state.sessions = Self::load_sessions(
            &self.base_path,
            &self.db,
            &self.projects,
            &existing_sessions,
        )
        .await;
        self.start_pr_polling_for_pull_request_sessions();
        if self.session_state.sessions.is_empty() {
            self.session_state.table_state.select(None);
        } else {
            self.session_state.table_state.select(Some(0));
        }
        self.update_sessions_metadata_cache().await;

        Ok(())
    }

    /// Refreshes in-memory sessions from the database when `session` metadata
    /// changes.
    pub async fn refresh_sessions_if_needed(&mut self) {
        if !self.is_session_refresh_due() {
            return;
        }

        self.session_state.refresh_deadline = Instant::now() + SESSION_REFRESH_INTERVAL;

        let Ok((sessions_row_count, sessions_updated_at_max)) =
            self.db.load_sessions_metadata().await
        else {
            return;
        };
        if sessions_row_count == self.session_state.row_count
            && sessions_updated_at_max == self.session_state.updated_at_max
        {
            return;
        }

        let selected_index = self.session_state.table_state.selected();
        let selected_session_name = selected_index
            .and_then(|index| self.session_state.sessions.get(index))
            .map(|session| session.id.clone());

        let existing_sessions = std::mem::take(&mut self.session_state.sessions);
        self.session_state.sessions = Self::load_sessions(
            &self.base_path,
            &self.db,
            &self.projects,
            &existing_sessions,
        )
        .await;
        self.start_pr_polling_for_pull_request_sessions();
        self.restore_table_selection(selected_session_name.as_deref(), selected_index);
        self.ensure_mode_session_exists();

        self.session_state.row_count = sessions_row_count;
        self.session_state.updated_at_max = sessions_updated_at_max;
    }

    pub fn next_tab(&mut self) {
        self.current_tab = self.current_tab.next();
    }

    pub fn next(&mut self) {
        if self.session_state.sessions.is_empty() {
            return;
        }
        let i = match self.session_state.table_state.selected() {
            Some(i) => {
                if i >= self.session_state.sessions.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.session_state.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.session_state.sessions.is_empty() {
            return;
        }
        let i = match self.session_state.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.session_state.sessions.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.session_state.table_state.select(Some(i));
    }

    /// Creates a blank session with an empty prompt and output.
    ///
    /// Returns the identifier of the newly created session.
    /// The session is created with `New` status and no agent is started —
    /// call [`start_session`] to submit a prompt and launch the agent.
    ///
    /// # Errors
    /// Returns an error if the worktree, session files, or database record
    /// cannot be created.
    pub async fn create_session(&mut self) -> Result<String, String> {
        let base_branch = self
            .git_branch
            .as_deref()
            .ok_or_else(|| "Git branch is required to create a session".to_string())?;

        let session_id = Uuid::new_v4().to_string();
        let folder = self.base_path.join(&session_id);
        if folder.exists() {
            return Err(format!("Session folder {session_id} already exists"));
        }

        // Create git worktree
        let worktree_branch = format!("agentty/{session_id}");
        let repo_root = git::find_git_repo_root(&self.working_dir)
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        let folder_bg = folder.clone();
        let repo_root_bg = repo_root.clone();
        let branch_bg = worktree_branch.clone();
        let base_bg = base_branch.to_string();
        tokio::task::spawn_blocking(move || {
            git::create_worktree(&repo_root_bg, &folder_bg, &branch_bg, &base_bg)
        })
        .await
        .map_err(|e| format!("Join error: {e}"))?
        .map_err(|err| format!("Failed to create git worktree: {err}"))?;

        let data_dir = folder.join(SESSION_DATA_DIR);
        if let Err(err) = std::fs::create_dir_all(&data_dir) {
            self.rollback_failed_session_creation(
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!(
                "Failed to create session metadata directory: {err}"
            ));
        }

        if let Err(err) = std::fs::write(data_dir.join("prompt.txt"), "") {
            self.rollback_failed_session_creation(
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!("Failed to write session prompt: {err}"));
        }

        if let Err(err) = std::fs::write(data_dir.join("output.txt"), "") {
            self.rollback_failed_session_creation(
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!("Failed to write session output: {err}"));
        }

        if let Err(err) = self
            .db
            .insert_session(
                &session_id,
                &AgentKind::Gemini.to_string(),
                AgentKind::Gemini.default_model().as_str(),
                base_branch,
                &Status::New.to_string(),
                self.active_project_id,
            )
            .await
        {
            self.rollback_failed_session_creation(
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!("Failed to save session metadata: {err}"));
        }

        AgentKind::Gemini.create_backend().setup(&folder);

        let existing_sessions = std::mem::take(&mut self.session_state.sessions);
        self.session_state.sessions = Self::load_sessions(
            &self.base_path,
            &self.db,
            &self.projects,
            &existing_sessions,
        )
        .await;
        self.update_sessions_metadata_cache().await;

        let index = self
            .session_state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .unwrap_or(0);
        self.session_state.table_state.select(Some(index));

        Ok(session_id)
    }

    /// Submits the first prompt for a blank session and starts the agent.
    ///
    /// # Errors
    /// Returns an error if the session is missing or prompt persistence fails.
    pub async fn start_session(&mut self, session_id: &str, prompt: String) -> Result<(), String> {
        let session_index = self
            .session_index_for_id(session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        let session = self
            .session_state
            .sessions
            .get_mut(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        session.prompt.clone_from(&prompt);

        let title = Self::summarize_title(&prompt);
        session.title = Some(title.clone());
        let _ = self.db.update_session_title(&session.id, &title).await;

        let data_dir = session.folder.join(SESSION_DATA_DIR);
        std::fs::write(data_dir.join("prompt.txt"), &prompt)
            .map_err(|err| format!("Failed to write session prompt: {err}"))?;

        let initial_output = format!(" › {prompt}\n\n");
        session.append_output(&initial_output);

        let folder = session.folder.clone();
        let output = Arc::clone(&session.output);
        let status = Arc::clone(&session.status);
        let name = session.id.clone();
        let db = self.db.clone();
        let (session_agent, session_model) = Self::resolve_session_agent_and_model(session);

        let _ = Self::update_status(&status, &db, &name, Status::InProgress).await;

        let cmd = session_agent.create_backend().build_start_command(
            &folder,
            &prompt,
            session_model.as_str(),
        );
        Self::spawn_session_task(folder, cmd, output, status, db, name);

        Ok(())
    }

    /// Submits a follow-up prompt to an existing session.
    pub fn reply(&mut self, session_id: &str, prompt: &str) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.session_state.sessions.get(session_index) else {
            return;
        };
        let (session_agent, session_model) = Self::resolve_session_agent_and_model(session);
        let backend = session_agent.create_backend();
        self.reply_with_backend(session_id, prompt, backend.as_ref(), session_model.as_str());
    }

    /// Updates and persists the agent/model pair for a single session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or the model does not belong
    /// to the selected agent.
    pub fn set_session_agent_and_model(
        &mut self,
        session_id: &str,
        session_agent: AgentKind,
        session_model: AgentModel,
    ) -> Result<(), String> {
        if session_model.kind() != session_agent {
            return Err("Model does not belong to selected agent".to_string());
        }

        let Some(session_index) = self.session_index_for_id(session_id) else {
            return Err("Session not found".to_string());
        };
        let Some(session) = self.session_state.sessions.get_mut(session_index) else {
            return Err("Session not found".to_string());
        };

        let agent = session_agent.to_string();
        let model = session_model.as_str().to_string();

        session.agent.clone_from(&agent);
        session.model.clone_from(&model);

        let db = self.db.clone();
        let id = session_id.to_string();
        tokio::spawn(async move {
            let _ = db.update_session_agent_and_model(&id, &agent, &model).await;
        });

        Ok(())
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.session_state
            .table_state
            .selected()
            .and_then(|i| self.session_state.sessions.get(i))
    }

    /// Returns the session identifier for the given list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<String> {
        self.session_state
            .sessions
            .get(session_index)
            .map(|session| session.id.clone())
    }

    /// Resolves a stable session identifier to the current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.session_state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
    }

    pub async fn delete_selected_session(&mut self) {
        let Some(i) = self.session_state.table_state.selected() else {
            return;
        };
        if i >= self.session_state.sessions.len() {
            return;
        }
        let session = self.session_state.sessions.remove(i);

        let _ = self.db.delete_session(&session.id).await;
        self.cancel_pr_polling_for_session(&session.id);
        self.clear_pr_creation_in_flight(&session.id);

        // Remove git worktree and branch if in a git repo
        if self.git_branch.is_some() {
            let branch_name = format!("agentty/{}", session.id);

            // Find repo root for branch deletion
            if let Some(repo_root) = git::find_git_repo_root(&self.working_dir) {
                let folder = session.folder.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = git::remove_worktree(&folder);
                    let _ = git::delete_branch(&repo_root, &branch_name);
                })
                .await;
            } else {
                let folder = session.folder.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = git::remove_worktree(&folder);
                })
                .await;
            }
        }

        let _ = std::fs::remove_dir_all(&session.folder);
        if self.session_state.sessions.is_empty() {
            self.session_state.table_state.select(None);
        } else if i >= self.session_state.sessions.len() {
            self.session_state
                .table_state
                .select(Some(self.session_state.sessions.len() - 1));
        }
        self.update_sessions_metadata_cache().await;
    }

    /// Commits all changes in a session worktree.
    ///
    /// # Errors
    /// Returns an error if the session has no worktree or the git commit
    /// operation fails.
    pub async fn commit_session(&self, session_id: &str) -> Result<String, String> {
        let session = self
            .session_state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| "Session not found".to_string())?;

        // Verify this session has a git worktree via DB
        if self.db.get_base_branch(&session.id).await?.is_none() {
            return Err("No git worktree for this session".to_string());
        }

        // Commit all changes in the worktree
        let folder = session.folder.clone();
        tokio::task::spawn_blocking(move || git::commit_all(&folder, "TEST COMMIT FROM AGENT"))
            .await
            .map_err(|e| format!("Join error: {e}"))?
            .map_err(|err| format!("Failed to commit: {err}"))?;

        Ok("Successfully committed changes".to_string())
    }

    /// Squash-merges a reviewed session branch into its base branch.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for merge, required git
    /// metadata is missing, or the merge/cleanup steps fail.
    pub async fn merge_session(&self, session_id: &str) -> Result<String, String> {
        let session = self
            .session_state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        if !matches!(session.status(), Status::Review | Status::PullRequest) {
            return Err("Session must be in review or pull request status".to_string());
        }

        // Read base branch from DB
        let base_branch = self
            .db
            .get_base_branch(&session.id)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Find repo root
        let repo_root = git::find_git_repo_root(&self.working_dir)
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        // Build source branch name
        let source_branch = format!("agentty/{}", session.id);

        // Build commit message from session prompt
        let commit_message = format!("Merge session: {}", session.prompt);

        // Perform squash merge
        {
            let repo_root = repo_root.clone();
            let source_branch = source_branch.clone();
            let base_branch = base_branch.clone();
            let commit_message = commit_message.clone();
            tokio::task::spawn_blocking(move || {
                git::squash_merge(&repo_root, &source_branch, &base_branch, &commit_message)
            })
            .await
            .map_err(|e| format!("Join error: {e}"))?
            .map_err(|err| format!("Failed to merge: {err}"))?;
        }

        if !Self::update_status(&session.status, &self.db, &session.id, Status::Done).await {
            return Err("Invalid status transition to Done".to_string());
        }

        self.cancel_pr_polling_for_session(&session.id);
        self.clear_pr_creation_in_flight(&session.id);
        Self::cleanup_merged_session_worktree(
            session.folder.clone(),
            source_branch.clone(),
            Some(repo_root),
        )
        .await
        .map_err(|error| format!("Merged successfully but failed to remove worktree: {error}"))?;

        Ok(format!(
            "Successfully merged {source_branch} into {base_branch}"
        ))
    }

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

    fn start_pr_polling_for_pull_request_sessions(&self) {
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

    fn clear_pr_creation_in_flight(&self, id: &str) {
        if let Ok(mut in_flight) = self.pr_creation_in_flight.lock() {
            in_flight.remove(id);
        }
    }

    fn cancel_pr_polling_for_session(&self, id: &str) {
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

    async fn cleanup_merged_session_worktree(
        folder: PathBuf,
        source_branch: String,
        repo_root: Option<PathBuf>,
    ) -> Result<(), String> {
        tokio::task::spawn_blocking(move || {
            let repo_root = repo_root.or_else(|| Self::resolve_repo_root_from_worktree(&folder));

            git::remove_worktree(&folder)?;

            if let Some(repo_root) = repo_root {
                git::delete_branch(&repo_root, &source_branch)?;
            }

            let _ = std::fs::remove_dir_all(&folder);

            Ok(())
        })
        .await
        .map_err(|error| format!("Join error: {error}"))?
    }

    fn resolve_repo_root_from_worktree(worktree_path: &Path) -> Option<PathBuf> {
        let git_path = worktree_path.join(".git");
        if git_path.is_dir() {
            return Some(worktree_path.to_path_buf());
        }

        if !git_path.is_file() {
            return None;
        }

        let git_file = std::fs::read_to_string(git_path).ok()?;
        let git_dir_line = git_file.lines().find(|line| line.starts_with("gitdir:"))?;
        let git_dir = PathBuf::from(git_dir_line.trim_start_matches("gitdir:").trim());
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            worktree_path.join(git_dir)
        };

        git_dir.parent()?.parent()?.parent().map(Path::to_path_buf)
    }

    fn reply_with_backend(
        &mut self,
        session_id: &str,
        prompt: &str,
        backend: &dyn AgentBackend,
        model: &str,
    ) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.session_state.sessions.get_mut(session_index) else {
            return;
        };

        // If the session was persisted with a blank prompt (e.g. app closed
        // before first message), treat the first reply as the initial start.
        let is_first_message = session.prompt.is_empty();
        let allowed = session.status() == Status::Review
            || (is_first_message && session.status() == Status::New);
        if !allowed {
            session.append_output("\n[Reply Error] Session must be in review status\n");

            return;
        }
        if is_first_message {
            session.prompt = prompt.to_string();
            let title = Self::summarize_title(prompt);
            session.title = Some(title.clone());
            let data_dir = session.folder.join(SESSION_DATA_DIR);
            let _ = std::fs::write(data_dir.join("prompt.txt"), prompt);
            let db = self.db.clone();
            let name = session.id.clone();
            tokio::spawn(async move {
                let _ = db.update_session_title(&name, &title).await;
            });
        }

        let reply_line = format!("\n › {prompt}\n\n");
        session.append_output(&reply_line);

        let folder = session.folder.clone();
        let output = Arc::clone(&session.output);
        let status = Arc::clone(&session.status);
        let name = session.id.clone();
        let db = self.db.clone();

        {
            let status = Arc::clone(&status);
            let db = db.clone();
            let name = name.clone();
            tokio::spawn(async move {
                Self::update_status(&status, &db, &name, Status::InProgress).await;
            });
        }

        let cmd = if is_first_message {
            backend.build_start_command(&folder, prompt, model)
        } else {
            backend.build_resume_command(&folder, prompt, model)
        };
        Self::spawn_session_task(folder, cmd, output, status, db, name);
    }

    fn resolve_session_agent_and_model(session: &Session) -> (AgentKind, AgentModel) {
        let session_agent = session
            .agent
            .parse::<AgentKind>()
            .unwrap_or(AgentKind::Gemini);
        let session_model = session_agent
            .parse_model(&session.model)
            .unwrap_or_else(|| session_agent.default_model());

        (session_agent, session_model)
    }

    fn summarize_title(prompt: &str) -> String {
        let first_line = prompt.lines().next().unwrap_or(prompt).trim();
        if first_line.len() <= 30 {
            return first_line.to_string();
        }

        let truncated = &first_line[..30];
        if let Some(last_space) = truncated.rfind(' ') {
            format!("{}…", &first_line[..last_space])
        } else {
            format!("{truncated}…")
        }
    }

    async fn rollback_failed_session_creation(
        &self,
        folder: &Path,
        repo_root: &Path,
        session_name: &str,
        worktree_branch: &str,
        session_saved: bool,
    ) {
        if session_saved {
            let _ = self.db.delete_session(session_name).await;
        }

        {
            let folder = folder.to_path_buf();
            let repo_root = repo_root.to_path_buf();
            let worktree_branch = worktree_branch.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                let _ = git::remove_worktree(&folder);
                let _ = git::delete_branch(&repo_root, &worktree_branch);
            })
            .await;
        }

        let _ = std::fs::remove_dir_all(folder);
    }

    fn is_session_refresh_due(&self) -> bool {
        Instant::now() >= self.session_state.refresh_deadline
    }

    fn restore_table_selection(
        &mut self,
        selected_session_name: Option<&str>,
        selected_index: Option<usize>,
    ) {
        if self.session_state.sessions.is_empty() {
            self.session_state.table_state.select(None);

            return;
        }

        if let Some(session_name) = selected_session_name
            && let Some(index) = self
                .session_state
                .sessions
                .iter()
                .position(|session| session.id == session_name)
        {
            self.session_state.table_state.select(Some(index));

            return;
        }

        let restored_index =
            selected_index.map(|index| index.min(self.session_state.sessions.len() - 1));
        self.session_state.table_state.select(restored_index);
    }

    fn ensure_mode_session_exists(&mut self) {
        let mode_session_id = match &self.mode {
            AppMode::Prompt { session_id, .. }
            | AppMode::View { session_id, .. }
            | AppMode::Diff { session_id, .. } => Some(session_id),
            _ => None,
        };
        let Some(session_id) = mode_session_id else {
            return;
        };
        if self.session_index_for_id(session_id).is_none() {
            self.mode = AppMode::List;
        }
    }

    async fn update_sessions_metadata_cache(&mut self) {
        if let Ok((sessions_row_count, sessions_updated_at_max)) =
            self.db.load_sessions_metadata().await
        {
            self.session_state.row_count = sessions_row_count;
            self.session_state.updated_at_max = sessions_updated_at_max;
        }
    }

    async fn load_sessions(
        base: &Path,
        db: &Database,
        projects: &[Project],
        existing_sessions: &[Session],
    ) -> Vec<Session> {
        let project_names: HashMap<i64, String> = projects
            .iter()
            .filter_map(|project| {
                let name = project.path.file_name()?.to_string_lossy().to_string();
                Some((project.id, name))
            })
            .collect();
        let existing_sessions_by_name: HashMap<String, SessionHandles> = existing_sessions
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    (Arc::clone(&session.output), Arc::clone(&session.status)),
                )
            })
            .collect();

        let db_rows = db.load_sessions().await.unwrap_or_default();
        let sessions: Vec<Session> = db_rows
            .into_iter()
            .filter_map(|row| {
                let folder = base.join(&row.id);
                if !folder.is_dir() {
                    return None;
                }
                let data_dir = folder.join(SESSION_DATA_DIR);
                let prompt = std::fs::read_to_string(data_dir.join("prompt.txt")).ok()?;
                let output_text =
                    std::fs::read_to_string(data_dir.join("output.txt")).unwrap_or_default();
                let status = row.status.parse::<Status>().unwrap_or(Status::Done);
                let session_agent = row.agent.parse::<AgentKind>().unwrap_or(AgentKind::Gemini);
                let session_model = session_agent
                    .parse_model(&row.model)
                    .unwrap_or_else(|| session_agent.default_model())
                    .as_str()
                    .to_string();
                let project_name = row
                    .project_id
                    .and_then(|id| project_names.get(&id))
                    .cloned()
                    .unwrap_or_default();
                let (output, status) = if let Some((existing_output, existing_status)) =
                    existing_sessions_by_name.get(&row.id)
                {
                    if let Ok(mut output_buffer) = existing_output.lock() {
                        *output_buffer = output_text;
                    }
                    if let Ok(mut status_value) = existing_status.lock() {
                        *status_value = status;
                    }
                    (Arc::clone(existing_output), Arc::clone(existing_status))
                } else {
                    (
                        Arc::new(Mutex::new(output_text)),
                        Arc::new(Mutex::new(status)),
                    )
                };
                Some(Session {
                    agent: row.agent,
                    folder,
                    id: row.id,
                    model: session_model,
                    output,
                    project_name,
                    prompt,
                    status,
                    title: row.title,
                })
            })
            .collect();

        sessions
    }

    async fn discover_sibling_projects(working_dir: &Path, db: &Database) {
        let Some(parent) = working_dir.parent() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || path == working_dir {
                continue;
            }
            if path.join(".git").exists() {
                let branch = git::detect_git_info(&path);
                let _ = db
                    .upsert_project(&path.to_string_lossy(), branch.as_deref())
                    .await;
            }
        }
    }

    async fn load_projects_from_db(db: &Database) -> Vec<Project> {
        db.load_projects()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|row| Project {
                git_branch: row.git_branch,
                id: row.id,
                path: PathBuf::from(row.path),
            })
            .collect()
    }

    fn spawn_git_status_task(
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

    fn spawn_session_task(
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

    async fn update_status(status: &Mutex<Status>, db: &Database, id: &str, new: Status) -> bool {
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

    async fn process_output<R: AsyncRead + Unpin>(
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::*;
    use crate::agent::MockAgentBackend;

    fn create_mock_backend() -> MockAgentBackend {
        let mut mock = MockAgentBackend::new();
        mock.expect_build_start_command().returning(|folder, _, _| {
            let mut cmd = Command::new("echo");
            cmd.arg("mock-start")
                .current_dir(folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            cmd
        });
        mock
    }

    async fn new_test_app(path: PathBuf) -> App {
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        App::new(path, working_dir, None, db).await
    }

    fn setup_test_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        std::fs::write(path.join("README.md"), "test").expect("write failed");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("git commit failed");
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(path)
            .output()
            .expect("git branch failed");
    }

    async fn new_test_app_with_git(path: &Path) -> App {
        setup_test_git_repo(path);
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        App::new(
            path.to_path_buf(),
            path.to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await
    }

    fn add_manual_session(app: &mut App, base_path: &Path, id: &str, prompt: &str) {
        let folder = base_path.join(id);
        let data_dir = folder.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        std::fs::write(data_dir.join("prompt.txt"), prompt).expect("failed to write prompt");
        std::fs::write(data_dir.join("output.txt"), "").expect("failed to write output");
        app.session_state.sessions.push(Session {
            agent: "gemini".to_string(),
            folder,
            id: id.to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: String::new(),
            prompt: prompt.to_string(),
            status: Arc::new(Mutex::new(Status::Review)),
            title: Some(App::summarize_title(prompt)),
        });
        if app.session_state.table_state.selected().is_none() {
            app.session_state.table_state.select(Some(0));
        }
    }

    /// Helper: creates a session and starts it with the given prompt (two-step
    /// flow).
    async fn create_and_start_session(app: &mut App, prompt: &str) {
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let start_backend = create_mock_backend();
        app.reply_with_backend(
            &session_id,
            prompt,
            &start_backend,
            "gemini-3-flash-preview",
        );
    }

    async fn wait_for_status(session: &Session, expected: Status) {
        for _ in 0..40 {
            if session.status() == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        panic!("session did not reach status {expected}");
    }

    #[tokio::test]
    async fn test_new_app_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
        assert_eq!(app.session_state.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_working_dir_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let working_dir = app.working_dir();

        // Assert
        assert_eq!(working_dir, &PathBuf::from("/tmp/test"));
    }

    #[tokio::test]
    async fn test_git_branch_getter_with_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            dir.path().to_path_buf(),
            working_dir,
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let branch = app.git_branch();

        // Assert
        assert_eq!(branch, Some("main"));
    }

    #[tokio::test]
    async fn test_git_branch_getter_without_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let branch = app.git_branch();

        // Assert
        assert_eq!(branch, None);
    }

    #[tokio::test]
    async fn test_navigation() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        create_and_start_session(&mut app, "B").await;

        // Act & Assert (Next)
        app.session_state.table_state.select(Some(0));
        app.next();
        assert_eq!(app.session_state.table_state.selected(), Some(1));
        app.next();
        assert_eq!(app.session_state.table_state.selected(), Some(0)); // Loop back

        // Act & Assert (Previous)
        app.previous();
        assert_eq!(app.session_state.table_state.selected(), Some(1)); // Loop back
        app.previous();
        assert_eq!(app.session_state.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_navigation_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        app.next();
        assert_eq!(app.session_state.table_state.selected(), None);

        app.previous();
        assert_eq!(app.session_state.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_navigation_recovery() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;

        // Act & Assert — next recovers from None
        app.session_state.table_state.select(None);
        app.next();
        assert_eq!(app.session_state.table_state.selected(), Some(0));

        // Act & Assert — previous recovers from None
        app.session_state.table_state.select(None);
        app.previous();
        assert_eq!(app.session_state.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_create_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;

        // Act
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Assert — blank session
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(session_id, app.session_state.sessions[0].id);
        assert!(app.session_state.sessions[0].prompt.is_empty());
        assert_eq!(app.session_state.sessions[0].title, None);
        assert_eq!(app.session_state.sessions[0].display_title(), "No title");
        assert_eq!(app.session_state.sessions[0].status(), Status::New);
        assert_eq!(app.session_state.table_state.selected(), Some(0));
        assert_eq!(app.session_state.sessions[0].agent, "gemini");

        // Check filesystem
        let session_dir = &app.session_state.sessions[0].folder;
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        assert!(session_dir.exists());
        assert!(data_dir.join("prompt.txt").exists());
        assert!(data_dir.join("output.txt").exists());
        let prompt_content =
            std::fs::read_to_string(data_dir.join("prompt.txt")).expect("failed to read prompt");
        assert!(prompt_content.is_empty());
        let output_content =
            std::fs::read_to_string(data_dir.join("output.txt")).expect("failed to read output");
        assert!(output_content.is_empty());

        // Check DB
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert_eq!(db_sessions.len(), 1);
        assert_eq!(db_sessions[0].agent, "gemini");
        assert_eq!(db_sessions[0].base_branch, "main");
        assert_eq!(db_sessions[0].status, "New");
    }

    #[tokio::test]
    async fn test_start_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Act
        app.start_session(&session_id, "Hello".to_string())
            .await
            .expect("failed to start session");

        // Assert
        assert_eq!(app.session_state.sessions[0].prompt, "Hello");
        assert_eq!(
            app.session_state.sessions[0].title,
            Some("Hello".to_string())
        );
        let output = app.session_state.sessions[0]
            .output
            .lock()
            .expect("failed to lock output")
            .clone();
        assert!(output.contains("Hello"));

        // Check filesystem
        let data_dir = app.session_state.sessions[0].folder.join(SESSION_DATA_DIR);
        let prompt_content =
            std::fs::read_to_string(data_dir.join("prompt.txt")).expect("failed to read prompt");
        assert_eq!(prompt_content, "Hello");
    }

    #[tokio::test]
    async fn test_esc_deletes_blank_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing session index");
        let session_folder = app.session_state.sessions[session_index].folder.clone();
        assert!(session_folder.exists());

        // Act — simulate Esc: delete the blank session
        app.delete_selected_session().await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
        assert!(!session_folder.exists());
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert!(db_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_reply() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Initial").await;
        let session_id = app.session_state.sessions[0].id.clone();

        // Act
        app.reply(&session_id, "Reply");

        // Assert
        let session = &app.session_state.sessions[0];
        let output = session.output.lock().expect("failed to lock output");
        assert!(output.contains("Reply"));
    }

    #[tokio::test]
    async fn test_selected_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Test").await;

        // Act & Assert
        assert!(app.selected_session().is_some());

        app.session_state.table_state.select(None);
        assert!(app.selected_session().is_none());
    }

    #[tokio::test]
    async fn test_delete_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        let session_folder = app.session_state.sessions[0].folder.clone();

        // Act
        app.delete_selected_session().await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
        assert_eq!(app.session_state.table_state.selected(), None);
        assert!(!session_folder.exists());
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert!(db_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_delete_selected_session_edge_cases() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "1").await;
        create_and_start_session(&mut app, "2").await;

        // Act & Assert — index out of bounds
        app.session_state.table_state.select(Some(99));
        app.delete_selected_session().await;
        assert_eq!(app.session_state.sessions.len(), 2);

        // Act & Assert — None selected
        app.session_state.table_state.select(None);
        app.delete_selected_session().await;
        assert_eq!(app.session_state.sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_last_session_update_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "1").await;
        create_and_start_session(&mut app, "2").await;

        // Act & Assert — delete last item
        app.session_state.table_state.select(Some(1));
        app.delete_selected_session().await;
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(app.session_state.table_state.selected(), Some(0));

        // Act & Assert — delete remaining item
        app.delete_selected_session().await;
        assert!(app.session_state.sessions.is_empty());
        assert_eq!(app.session_state.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_load_existing_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "12345678",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        let session_dir = dir.path().join("12345678");
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir(&session_dir).expect("failed to create session dir");
        std::fs::create_dir(&data_dir).expect("failed to create data dir");
        std::fs::write(data_dir.join("prompt.txt"), "Existing").expect("failed to write prompt");
        std::fs::write(data_dir.join("output.txt"), "Output").expect("failed to write output");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(app.session_state.sessions[0].id, "12345678");
        assert_eq!(app.session_state.sessions[0].prompt, "Existing");
        assert_eq!(app.session_state.sessions[0].agent, "claude");
        assert_eq!(app.session_state.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_load_existing_sessions_ordered_by_updated_at_desc() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert alpha");
        db.insert_session(
            "beta",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta");

        sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(1_i64)
        .bind("alpha")
        .execute(db.pool())
        .await
        .expect("failed to update alpha timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(2_i64)
        .bind("beta")
        .execute(db.pool())
        .await
        .expect("failed to update beta timestamp");

        for session_name in ["alpha", "beta"] {
            let session_dir = dir.path().join(session_name);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
            std::fs::write(data_dir.join("prompt.txt"), session_name)
                .expect("failed to write prompt");
            std::fs::write(data_dir.join("output.txt"), "output").expect("failed to write output");
        }

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert
        let session_names: Vec<&str> = app
            .session_state
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        assert_eq!(session_names, vec!["beta", "alpha"]);
    }

    #[tokio::test]
    async fn test_refresh_sessions_if_needed_reloads_and_preserves_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha");
        db.insert_session(
            "beta",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 1
WHERE id = 'alpha'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 2
WHERE id = 'beta'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta timestamp");
        for session_name in ["alpha", "beta"] {
            let session_dir = dir.path().join(session_name);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
            std::fs::write(data_dir.join("prompt.txt"), session_name)
                .expect("failed to write prompt");
            std::fs::write(data_dir.join("output.txt"), "output").expect("failed to write output");
        }
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;
        app.session_state.table_state.select(Some(1));

        // Act
        tokio::time::sleep(Duration::from_secs(1)).await;
        app.db
            .update_session_status("alpha", "Done")
            .await
            .expect("failed to update session status");
        tokio::time::sleep(Duration::from_millis(600)).await;
        app.refresh_sessions_if_needed().await;

        // Assert
        assert_eq!(app.session_state.sessions[0].id, "alpha");
        let selected_index = app
            .session_state
            .table_state
            .selected()
            .expect("missing selection");
        assert_eq!(app.session_state.sessions[selected_index].id, "alpha");
    }

    #[tokio::test]
    async fn test_refresh_sessions_if_needed_remaps_view_mode_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha");
        db.insert_session(
            "beta",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 1
WHERE id = 'alpha'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 2
WHERE id = 'beta'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta timestamp");
        for session_name in ["alpha", "beta"] {
            let session_dir = dir.path().join(session_name);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
            std::fs::write(data_dir.join("prompt.txt"), session_name)
                .expect("failed to write prompt");
            std::fs::write(data_dir.join("output.txt"), "output").expect("failed to write output");
        }
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;
        let selected_session_id = app.session_state.sessions[1].id.clone();
        app.mode = AppMode::View {
            session_id: selected_session_id.clone(),
            scroll_offset: None,
        };

        // Act
        tokio::time::sleep(Duration::from_secs(1)).await;
        app.db
            .update_session_status("alpha", "Done")
            .await
            .expect("failed to update session status");
        tokio::time::sleep(Duration::from_millis(600)).await;
        app.refresh_sessions_if_needed().await;

        // Assert
        assert_eq!(app.session_state.sessions[0].id, "alpha");
        match app.mode {
            AppMode::View { session_id, .. } => assert_eq!(session_id, selected_session_id),
            _ => panic!("expected view mode"),
        }
    }

    #[tokio::test]
    async fn test_load_sessions_invalid_path() {
        // Arrange
        let path = PathBuf::from("/invalid/path/that/does/not/exist");

        // Act
        let app = new_test_app(path).await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_load_session_without_folder_skipped() {
        // Arrange — DB has a row but no matching folder on disk
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "missing01",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert — session is skipped because folder doesn't exist
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_spawn_integration() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut mock = MockAgentBackend::new();
        mock.expect_build_start_command()
            .returning(|folder, prompt, _| {
                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("--model")
                    .arg("gemini-3-flash-preview")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act — create and start session (start command)
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.reply_with_backend(&session_id, "SpawnInit", &mock, "gemini-3-flash-preview");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Assert
        {
            let session = &app.session_state.sessions[0];
            let output = session
                .output
                .lock()
                .expect("failed to lock output")
                .clone();
            assert!(output.contains("--prompt"));
            assert!(output.contains("SpawnInit"));
            assert!(!output.contains("--resume"));
            assert_eq!(session.status(), Status::Review);
        }

        // Act — reply (resume command)
        let mut resume_mock = MockAgentBackend::new();
        resume_mock
            .expect_build_resume_command()
            .returning(|folder, prompt, _| {
                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("--model")
                    .arg("gemini-3-flash-preview")
                    .arg("--resume")
                    .arg("latest")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        let session_id = app.session_state.sessions[0].id.clone();
        app.reply_with_backend(
            &session_id,
            "SpawnReply",
            &resume_mock,
            "gemini-3-flash-preview",
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Assert
        {
            let session = &app.session_state.sessions[0];
            let output = session
                .output
                .lock()
                .expect("failed to lock output")
                .clone();
            assert!(output.contains("SpawnReply"));
            assert!(output.contains("--resume"));
            assert!(output.contains("latest"));
            assert_eq!(session.status(), Status::Review);
        }
    }

    #[tokio::test]
    async fn test_process_output() {
        // Arrange
        let output = Arc::new(Mutex::new(String::new()));
        let file: Arc<Mutex<Option<std::io::BufWriter<std::fs::File>>>> =
            Arc::new(Mutex::new(None));
        let source = "Line 1\nLine 2".as_bytes();

        // Act
        App::process_output(source, &file, &output).await;

        // Assert
        let out = output.lock().expect("failed to lock output").clone();
        assert!(out.contains("Line 1"));
        assert!(out.contains("Line 2"));

        // Arrange — with file
        let dir = tempdir().expect("failed to create temp dir");
        let file_path = dir.path().join("out.txt");
        let f = std::fs::File::create(&file_path).expect("failed to create file");
        let file = Arc::new(Mutex::new(Some(std::io::BufWriter::new(f))));
        let source_file = "File Line".as_bytes();

        // Act
        App::process_output(source_file, &file, &output).await;

        // Assert
        drop(file);
        let content = std::fs::read_to_string(file_path).expect("failed to read file");
        assert!(content.contains("File Line"));
    }

    #[tokio::test]
    async fn test_next_tab() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert_eq!(app.current_tab, Tab::Sessions);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Roadmap);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Sessions);
    }

    #[tokio::test]
    async fn test_create_session_without_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.create_session().await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Git branch is required")
        );
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_create_session_with_git_no_actual_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let result = app.create_session().await;

        // Assert - should fail because git repo doesn't actually exist
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("git repository root")
        );
    }

    #[tokio::test]
    async fn test_create_session_cleans_up_on_error() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let result = app.create_session().await;

        // Assert - session should not be created
        assert!(result.is_err());
        assert_eq!(app.session_state.sessions.len(), 0);

        // Verify no session folder was left behind
        let entries = std::fs::read_dir(dir.path())
            .expect("failed to read dir")
            .count();
        assert_eq!(entries, 0, "Session folder should be cleaned up on error");
    }

    #[tokio::test]
    async fn test_delete_session_without_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        app.delete_selected_session().await;

        // Assert
        assert_eq!(app.session_state.sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_commit_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.commit_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_commit_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.commit_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_merge_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.merge_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_merge_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.merge_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_merge_session_removes_worktree_and_branch_after_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Local merge cleanup").await;
        wait_for_status(&app.session_state.sessions[0], Status::Review).await;
        let session_id = app.session_state.sessions[0].id.clone();
        app.commit_session(&session_id)
            .await
            .expect("failed to commit session");
        let session_folder = app.session_state.sessions[0].folder.clone();
        let session_name = app.session_state.sessions[0].id.clone();
        let branch_name = format!("agentty/{session_name}");

        // Act
        let result = app.merge_session(&session_id).await;

        // Assert
        assert!(result.is_ok(), "merge should succeed: {:?}", result.err());
        assert_eq!(app.session_state.sessions[0].status(), Status::Done);
        assert!(!session_folder.exists(), "worktree should be removed");

        let branch_output = Command::new("git")
            .args(["branch", "--list", &branch_name])
            .current_dir(dir.path())
            .output()
            .expect("failed to list branches");
        let branches = String::from_utf8_lossy(&branch_output.stdout);
        assert!(
            branches.trim().is_empty(),
            "branch should be removed after merge"
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.create_pr_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_requires_review_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");
        if let Ok(mut status) = app.session_state.sessions[0].status.lock() {
            *status = Status::Done;
        }

        // Act
        let result = app.create_pr_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("must be in review")
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.create_pr_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_cleanup_merged_session_worktree_without_repo_hint() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let worktree_folder = dir.path().join("merged-worktree");
        let branch_name = "agentty/cleanup123";
        git::create_worktree(dir.path(), &worktree_folder, branch_name, "main")
            .expect("failed to create worktree");
        assert!(
            worktree_folder.exists(),
            "worktree should exist before cleanup"
        );

        // Act
        let result = App::cleanup_merged_session_worktree(
            worktree_folder.clone(),
            branch_name.to_string(),
            None,
        )
        .await;

        // Assert
        assert!(result.is_ok(), "cleanup should succeed: {:?}", result.err());
        assert!(
            !worktree_folder.exists(),
            "worktree should be removed after cleanup"
        );

        let branch_output = Command::new("git")
            .args(["branch", "--list", branch_name])
            .current_dir(dir.path())
            .output()
            .expect("failed to list branches");
        let branches = String::from_utf8_lossy(&branch_output.stdout);
        assert!(
            branches.trim().is_empty(),
            "branch should be removed after cleanup"
        );
    }

    #[tokio::test]
    async fn test_active_project_id_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert!(app.active_project_id() > 0);
    }

    #[tokio::test]
    async fn test_projects_auto_registered() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert — cwd auto-registered as a project
        assert!(
            app.projects
                .iter()
                .any(|project| project.path == Path::new("/tmp/test"))
        );
    }

    #[tokio::test]
    async fn test_switch_project() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        let other_id = app
            .db
            .upsert_project("/tmp/other", Some("develop"))
            .await
            .expect("failed to upsert");

        // Act
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert
        assert_eq!(app.active_project_id(), other_id);
        assert_eq!(app.working_dir(), &PathBuf::from("/tmp/other"));
        assert_eq!(app.git_branch(), Some("develop"));
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_switch_project_keeps_existing_pr_polling_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        let other_id = app
            .db
            .upsert_project("/tmp/other", None)
            .await
            .expect("failed to upsert");
        if let Ok(mut polling) = app.pr_poll_cancel.lock() {
            polling.insert("manual01".to_string(), Arc::new(AtomicBool::new(false)));
        }

        // Act
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert
        let polling = app.pr_poll_cancel.lock().expect("failed to lock polling");
        assert!(polling.contains_key("manual01"));
    }

    #[tokio::test]
    async fn test_switch_project_not_found() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.switch_project(999).await;

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("expected missing project error");
        assert!(error.contains("Project not found"));
    }

    #[tokio::test]
    async fn test_switch_project_shows_all_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Session A").await;
        assert_eq!(app.session_state.sessions.len(), 1);

        let other_id = app
            .db
            .upsert_project("/tmp/other", None)
            .await
            .expect("failed to upsert");

        // Act — switch to other project
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert — all sessions still visible after switching projects
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(app.active_project_id(), other_id);
    }

    #[tokio::test]
    async fn test_create_session_scoped_to_project() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let project_id = app.active_project_id();

        // Act
        app.create_session()
            .await
            .expect("failed to create session");

        // Assert — session belongs to the active project
        let sessions = app.db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_discover_sibling_projects() {
        // Arrange — create a parent dir with two git repo subdirectories
        let parent = tempdir().expect("failed to create temp dir");
        let repo_a = parent.path().join("repo_a");
        let repo_b = parent.path().join("repo_b");
        let not_repo = parent.path().join("plain_dir");
        std::fs::create_dir(&repo_a).expect("failed to create repo_a");
        std::fs::create_dir(&repo_b).expect("failed to create repo_b");
        std::fs::create_dir(&not_repo).expect("failed to create plain_dir");
        setup_test_git_repo(&repo_a);
        setup_test_git_repo(&repo_b);

        // Act — launch app from repo_a
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            parent.path().to_path_buf(),
            repo_a.clone(),
            Some("main".to_string()),
            db,
        )
        .await;

        // Assert — repo_a (cwd) and repo_b (sibling) are discovered, plain_dir is not
        assert_eq!(app.projects.len(), 2);
        let paths: Vec<&Path> = app.projects.iter().map(|p| p.path.as_path()).collect();
        assert!(paths.contains(&repo_a.as_path()));
        assert!(paths.contains(&repo_b.as_path()));
    }

    #[test]
    fn test_summarize_title_short() {
        // Arrange & Act & Assert
        assert_eq!(App::summarize_title("Fix bug"), "Fix bug");
    }

    #[test]
    fn test_summarize_title_exact_30() {
        // Arrange
        let prompt = "a23456789012345678901234567890"; // exactly 30 chars

        // Act & Assert
        assert_eq!(prompt.len(), 30);
        assert_eq!(App::summarize_title(prompt), prompt);
    }

    #[test]
    fn test_summarize_title_long_with_space() {
        // Arrange
        let prompt = "Fix the authentication bug in the login flow";

        // Act
        let title = App::summarize_title(prompt);

        // Assert
        assert_eq!(title, "Fix the authentication bug in…");
        assert!(title.len() <= 34); // 30 chars + ellipsis (3 bytes)
    }

    #[test]
    fn test_summarize_title_long_without_spaces() {
        // Arrange
        let prompt = "abcdefghijklmnopqrstuvwxyz1234567890";

        // Act
        let title = App::summarize_title(prompt);

        // Assert
        assert_eq!(title, "abcdefghijklmnopqrstuvwxyz1234…");
    }

    #[test]
    fn test_summarize_title_multiline() {
        // Arrange
        let prompt = "First line\nSecond line\nThird line";

        // Act
        let title = App::summarize_title(prompt);

        // Assert
        assert_eq!(title, "First line");
    }

    #[test]
    fn test_summarize_title_empty() {
        // Arrange & Act & Assert
        assert_eq!(App::summarize_title(""), "");
    }
}
