use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;
use tokio::io::{AsyncBufReadExt as _, AsyncRead};

use crate::agent::{AgentBackend, AgentKind};
use crate::db::Database;
use crate::git;
use crate::health::{self, HealthEntry};
use crate::model::{AppMode, Project, SESSION_DATA_DIR, Session, Status, Tab};

pub const AGENTTY_WORKSPACE: &str = "/var/tmp/.agentty";
const PR_MERGE_POLL_INTERVAL: Duration = Duration::from_secs(30);
type SessionHandles = (Arc<Mutex<String>>, Arc<Mutex<Status>>);

pub struct App {
    pub current_tab: Tab,
    pub mode: AppMode,
    pub projects: Vec<Project>,
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    active_project_id: i64,
    agent_kind: AgentKind,
    backend: Box<dyn AgentBackend>,
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
        agent_kind: AgentKind,
        backend: Box<dyn AgentBackend>,
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
            sessions,
            table_state,
            active_project_id,
            agent_kind,
            backend,
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

    pub fn agent_kind(&self) -> AgentKind {
        self.agent_kind
    }

    pub fn set_agent_kind(&mut self, agent_kind: AgentKind) {
        self.agent_kind = agent_kind;
        self.backend = agent_kind.create_backend();
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
        let existing_sessions = std::mem::take(&mut self.sessions);
        self.sessions = Self::load_sessions(
            &self.base_path,
            &self.db,
            &self.projects,
            &existing_sessions,
        )
        .await;
        self.start_pr_polling_for_pull_request_sessions();
        if self.sessions.is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state.select(Some(0));
        }

        Ok(())
    }

    pub fn next_tab(&mut self) {
        self.current_tab = self.current_tab.next();
    }

    pub fn next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.sessions.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.sessions.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    /// Creates a blank session with an empty prompt and output.
    ///
    /// Returns the index of the newly created session in the sorted list.
    /// The session is created with `New` status and no agent is started —
    /// call [`start_session`] to submit a prompt and launch the agent.
    pub async fn create_session(&mut self) -> Result<usize, String> {
        let base_branch = self
            .git_branch
            .as_deref()
            .ok_or_else(|| "Git branch is required to create a session".to_string())?;

        let mut hasher = DefaultHasher::new();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        nanos.hash(&mut hasher);
        let hash = format!("{:016x}", hasher.finish());
        let name = hash.clone();

        let folder = self.base_path.join(&hash);
        if folder.exists() {
            return Err(format!("Session folder {hash} already exists"));
        }

        // Create git worktree
        let worktree_branch = format!("agentty/{hash}");
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
                &name,
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
                &name,
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
                &name,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!("Failed to write session output: {err}"));
        }

        if let Err(err) = self
            .db
            .insert_session(
                &name,
                &self.agent_kind.to_string(),
                base_branch,
                &Status::New.to_string(),
                self.active_project_id,
            )
            .await
        {
            self.rollback_failed_session_creation(
                &folder,
                &repo_root,
                &name,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!("Failed to save session metadata: {err}"));
        }

        self.backend.setup(&folder);

        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new(Status::New));

        let project_name = self
            .working_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        self.sessions.push(Session {
            agent: self.agent_kind.to_string(),
            folder,
            name: name.clone(),
            output,
            project_name,
            prompt: String::new(),
            status,
        });
        self.sessions.sort_by(|a, b| a.name.cmp(&b.name));

        let index = self
            .sessions
            .iter()
            .position(|a| a.name == name)
            .unwrap_or(0);
        self.table_state.select(Some(index));

        Ok(index)
    }

    /// Submits the first prompt for a blank session and starts the agent.
    pub async fn start_session(
        &mut self,
        session_index: usize,
        prompt: String,
    ) -> Result<(), String> {
        let session = self
            .sessions
            .get_mut(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        session.prompt = prompt.clone();

        let data_dir = session.folder.join(SESSION_DATA_DIR);
        std::fs::write(data_dir.join("prompt.txt"), &prompt)
            .map_err(|err| format!("Failed to write session prompt: {err}"))?;

        let initial_output = format!(" › {prompt}\n\n");
        session.append_output(&initial_output);

        let folder = session.folder.clone();
        let output = Arc::clone(&session.output);
        let status = Arc::clone(&session.status);
        let name = session.name.clone();
        let db = self.db.clone();

        let _ = Self::update_status(&status, &db, &name, Status::InProgress).await;

        let cmd = self.backend.build_start_command(&folder, &prompt);
        Self::spawn_session_task(folder, cmd, output, status, db, name);

        Ok(())
    }

    pub fn reply(&mut self, session_index: usize, prompt: &str) {
        let session_agent = self
            .sessions
            .get(session_index)
            .and_then(|s| s.agent.parse::<AgentKind>().ok())
            .unwrap_or(self.agent_kind);
        let backend = session_agent.create_backend();
        self.reply_with_backend(session_index, prompt, backend.as_ref());
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.table_state
            .selected()
            .and_then(|i| self.sessions.get(i))
    }

    pub async fn delete_selected_session(&mut self) {
        let Some(i) = self.table_state.selected() else {
            return;
        };
        if i >= self.sessions.len() {
            return;
        }
        let session = self.sessions.remove(i);

        let _ = self.db.delete_session(&session.name).await;
        self.cancel_pr_polling_for_session(&session.name);
        self.clear_pr_creation_in_flight(&session.name);

        // Remove git worktree and branch if in a git repo
        if self.git_branch.is_some() {
            let branch_name = format!("agentty/{}", session.name);

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
        if self.sessions.is_empty() {
            self.table_state.select(None);
        } else if i >= self.sessions.len() {
            self.table_state.select(Some(self.sessions.len() - 1));
        }
    }

    pub async fn commit_session(&self, session_index: usize) -> Result<String, String> {
        let session = self
            .sessions
            .get(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        // Verify this session has a git worktree via DB
        if self.db.get_base_branch(&session.name).await?.is_none() {
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

    pub async fn merge_session(&self, session_index: usize) -> Result<String, String> {
        let session = self
            .sessions
            .get(session_index)
            .ok_or_else(|| "Session not found".to_string())?;
        if !matches!(session.status(), Status::Review | Status::PullRequest) {
            return Err("Session must be in review or pull request status".to_string());
        }

        // Read base branch from DB
        let base_branch = self
            .db
            .get_base_branch(&session.name)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Find repo root
        let repo_root = git::find_git_repo_root(&self.working_dir)
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        // Build source branch name
        let source_branch = format!("agentty/{}", session.name);

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

        if !Self::update_status(&session.status, &self.db, &session.name, Status::Done).await {
            return Err("Invalid status transition to Done".to_string());
        }

        self.cancel_pr_polling_for_session(&session.name);
        self.clear_pr_creation_in_flight(&session.name);
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

    pub async fn create_pr_session(&self, session_index: usize) -> Result<(), String> {
        let session = self
            .sessions
            .get(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        if session.status() != Status::Review {
            return Err("Session must be in review to create a pull request".to_string());
        }

        if self.is_pr_creation_in_flight(&session.name) {
            return Err("Pull request creation is already in progress".to_string());
        }

        if self.is_pr_polling_active(&session.name) {
            return Err("Pull request is already being tracked".to_string());
        }

        // Read base branch from DB
        let base_branch = self
            .db
            .get_base_branch(&session.name)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Build source branch name
        let source_branch = format!("agentty/{}", session.name);

        // Build PR title from session prompt (first line only)
        let title = session
            .prompt
            .lines()
            .next()
            .unwrap_or("New Session")
            .to_string();

        self.mark_pr_creation_in_flight(&session.name)?;

        let status = Arc::clone(&session.status);
        let output = Arc::clone(&session.output);
        let folder = session.folder.clone();
        let db = self.db.clone();
        let name = session.name.clone();
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
                        Self::spawn_pr_poll_task(
                            db,
                            folder,
                            name.clone(),
                            output,
                            status,
                            source_branch,
                            repo_root,
                            pr_poll_cancel,
                        );
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
        for session in &self.sessions {
            if session.status() != Status::PullRequest {
                continue;
            }

            let source_branch = format!("agentty/{}", session.name);
            Self::spawn_pr_poll_task(
                self.db.clone(),
                session.folder.clone(),
                session.name.clone(),
                Arc::clone(&session.output),
                Arc::clone(&session.status),
                source_branch,
                Self::resolve_repo_root_from_worktree(&session.folder),
                Arc::clone(&self.pr_poll_cancel),
            );
        }
    }

    fn mark_pr_creation_in_flight(&self, name: &str) -> Result<(), String> {
        let mut in_flight = self
            .pr_creation_in_flight
            .lock()
            .map_err(|_| "Failed to lock PR creation state".to_string())?;

        if in_flight.contains(name) {
            return Err("Pull request creation is already in progress".to_string());
        }

        in_flight.insert(name.to_string());

        Ok(())
    }

    fn is_pr_creation_in_flight(&self, name: &str) -> bool {
        self.pr_creation_in_flight
            .lock()
            .map(|in_flight| in_flight.contains(name))
            .unwrap_or(false)
    }

    fn is_pr_polling_active(&self, name: &str) -> bool {
        self.pr_poll_cancel
            .lock()
            .map(|polling| polling.contains_key(name))
            .unwrap_or(false)
    }

    fn clear_pr_creation_in_flight(&self, name: &str) {
        if let Ok(mut in_flight) = self.pr_creation_in_flight.lock() {
            in_flight.remove(name);
        }
    }

    fn cancel_pr_polling_for_session(&self, name: &str) {
        if let Ok(mut polling) = self.pr_poll_cancel.lock() {
            if let Some(cancel) = polling.remove(name) {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    fn spawn_pr_poll_task(
        db: Database,
        folder: PathBuf,
        name: String,
        output: Arc<Mutex<String>>,
        status: Arc<Mutex<Status>>,
        source_branch: String,
        repo_root: Option<PathBuf>,
        pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut polling) = pr_poll_cancel.lock() {
            if polling.contains_key(&name) {
                return;
            }
            polling.insert(name.clone(), Arc::clone(&cancel));
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
                    if !Self::update_status(&status, &db, &name, Status::Done).await {
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
                polling.remove(&name);
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
        session_index: usize,
        prompt: &str,
        backend: &dyn AgentBackend,
    ) {
        let Some(session) = self.sessions.get_mut(session_index) else {
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
            let data_dir = session.folder.join(SESSION_DATA_DIR);
            let _ = std::fs::write(data_dir.join("prompt.txt"), prompt);
        }

        let reply_line = format!("\n › {prompt}\n\n");
        session.append_output(&reply_line);

        let folder = session.folder.clone();
        let output = Arc::clone(&session.output);
        let status = Arc::clone(&session.status);
        let name = session.name.clone();
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
            backend.build_start_command(&folder, prompt)
        } else {
            backend.build_resume_command(&folder, prompt)
        };
        Self::spawn_session_task(folder, cmd, output, status, db, name);
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
                    session.name.clone(),
                    (Arc::clone(&session.output), Arc::clone(&session.status)),
                )
            })
            .collect();

        let db_rows = db.load_sessions().await.unwrap_or_default();
        let mut sessions: Vec<Session> = db_rows
            .into_iter()
            .filter_map(|row| {
                let folder = base.join(&row.name);
                if !folder.is_dir() {
                    return None;
                }
                let data_dir = folder.join(SESSION_DATA_DIR);
                let prompt = std::fs::read_to_string(data_dir.join("prompt.txt")).ok()?;
                let output_text =
                    std::fs::read_to_string(data_dir.join("output.txt")).unwrap_or_default();
                let status = row.status.parse::<Status>().unwrap_or(Status::Done);
                let project_name = row
                    .project_id
                    .and_then(|id| project_names.get(&id))
                    .cloned()
                    .unwrap_or_default();
                let (output, status) = if let Some((existing_output, existing_status)) =
                    existing_sessions_by_name.get(&row.name)
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
                    name: row.name,
                    output,
                    project_name,
                    prompt,
                    status,
                })
            })
            .collect();
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
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
        name: String,
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

            Self::update_status(&status, &db, &name, Status::Review).await;
        });
    }

    async fn update_status(status: &Mutex<Status>, db: &Database, name: &str, new: Status) -> bool {
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
        let _ = db.update_session_status(name, &new.to_string()).await;

        true
    }

    async fn process_output<R: AsyncRead + Unpin>(
        source: R,
        file: &Arc<Mutex<Option<std::io::BufWriter<std::fs::File>>>>,
        output: &Arc<Mutex<String>>,
    ) {
        let mut reader = tokio::io::BufReader::new(source).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(mut f_guard) = file.lock() {
                if let Some(f) = f_guard.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
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
        mock.expect_setup().returning(|_| {});
        mock.expect_build_start_command().returning(|folder, _| {
            let mut cmd = Command::new("echo");
            cmd.arg("mock-start")
                .current_dir(folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            cmd
        });
        mock.expect_build_resume_command().returning(|folder, _| {
            let mut cmd = Command::new("echo");
            cmd.arg("mock-resume")
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
        App::new(
            path,
            working_dir,
            None,
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await
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
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await
    }

    fn add_manual_session(app: &mut App, base_path: &Path, name: &str, prompt: &str) {
        let folder = base_path.join(name);
        let data_dir = folder.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        std::fs::write(data_dir.join("prompt.txt"), prompt).expect("failed to write prompt");
        std::fs::write(data_dir.join("output.txt"), "").expect("failed to write output");
        app.sessions.push(Session {
            agent: "gemini".to_string(),
            folder,
            name: name.to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: String::new(),
            prompt: prompt.to_string(),
            status: Arc::new(Mutex::new(Status::Review)),
        });
        if app.table_state.selected().is_none() {
            app.table_state.select(Some(0));
        }
    }

    /// Helper: creates a session and starts it with the given prompt (two-step
    /// flow).
    async fn create_and_start_session(app: &mut App, prompt: &str) {
        let index = app
            .create_session()
            .await
            .expect("failed to create session");
        app.start_session(index, prompt.to_string())
            .await
            .expect("failed to start session");
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
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_agent_kind_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert_eq!(app.agent_kind(), AgentKind::Gemini);
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
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
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
    async fn test_set_agent_kind() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        assert_eq!(app.agent_kind(), AgentKind::Gemini);

        // Act
        app.set_agent_kind(AgentKind::Claude);

        // Assert
        assert_eq!(app.agent_kind(), AgentKind::Claude);
    }

    #[tokio::test]
    async fn test_navigation() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        create_and_start_session(&mut app, "B").await;

        // Act & Assert (Next)
        app.table_state.select(Some(0));
        app.next();
        assert_eq!(app.table_state.selected(), Some(1));
        app.next();
        assert_eq!(app.table_state.selected(), Some(0)); // Loop back

        // Act & Assert (Previous)
        app.previous();
        assert_eq!(app.table_state.selected(), Some(1)); // Loop back
        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_navigation_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        app.next();
        assert_eq!(app.table_state.selected(), None);

        app.previous();
        assert_eq!(app.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_navigation_recovery() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;

        // Act & Assert — next recovers from None
        app.table_state.select(None);
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));

        // Act & Assert — previous recovers from None
        app.table_state.select(None);
        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_create_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;

        // Act
        let index = app
            .create_session()
            .await
            .expect("failed to create session");

        // Assert — blank session
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(index, 0);
        assert!(app.sessions[0].prompt.is_empty());
        assert_eq!(app.sessions[0].status(), Status::New);
        assert_eq!(app.table_state.selected(), Some(0));
        assert_eq!(app.sessions[0].agent, "gemini");

        // Check filesystem
        let session_dir = &app.sessions[0].folder;
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
        let index = app
            .create_session()
            .await
            .expect("failed to create session");

        // Act
        app.start_session(index, "Hello".to_string())
            .await
            .expect("failed to start session");

        // Assert
        assert_eq!(app.sessions[0].prompt, "Hello");
        let output = app.sessions[0]
            .output
            .lock()
            .expect("failed to lock output")
            .clone();
        assert!(output.contains("Hello"));
        assert!(output.contains("Git worktree"));

        // Check filesystem
        let data_dir = app.sessions[0].folder.join(SESSION_DATA_DIR);
        let prompt_content =
            std::fs::read_to_string(data_dir.join("prompt.txt")).expect("failed to read prompt");
        assert_eq!(prompt_content, "Hello");
    }

    #[tokio::test]
    async fn test_esc_deletes_blank_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let index = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_folder = app.sessions[index].folder.clone();
        assert!(session_folder.exists());

        // Act — simulate Esc: delete the blank session
        app.delete_selected_session().await;

        // Assert
        assert!(app.sessions.is_empty());
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

        // Act
        app.reply(0, "Reply");

        // Assert
        let session = &app.sessions[0];
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

        app.table_state.select(None);
        assert!(app.selected_session().is_none());
    }

    #[tokio::test]
    async fn test_delete_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        let session_folder = app.sessions[0].folder.clone();

        // Act
        app.delete_selected_session().await;

        // Assert
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
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
        app.table_state.select(Some(99));
        app.delete_selected_session().await;
        assert_eq!(app.sessions.len(), 2);

        // Act & Assert — None selected
        app.table_state.select(None);
        app.delete_selected_session().await;
        assert_eq!(app.sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_last_session_update_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "1").await;
        create_and_start_session(&mut app, "2").await;

        // Act & Assert — delete last item
        app.table_state.select(Some(1));
        app.delete_selected_session().await;
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.table_state.selected(), Some(0));

        // Act & Assert — delete remaining item
        app.delete_selected_session().await;
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
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
        db.insert_session("12345678", "claude", "main", "Done", project_id)
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
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await;

        // Assert
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].name, "12345678");
        assert_eq!(app.sessions[0].prompt, "Existing");
        assert_eq!(app.sessions[0].agent, "claude");
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_load_sessions_invalid_path() {
        // Arrange
        let path = PathBuf::from("/invalid/path/that/does/not/exist");

        // Act
        let app = new_test_app(path).await;

        // Assert
        assert!(app.sessions.is_empty());
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
        db.insert_session("missing01", "gemini", "main", "Done", project_id)
            .await
            .expect("failed to insert");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await;

        // Assert — session is skipped because folder doesn't exist
        assert!(app.sessions.is_empty());
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
        mock.expect_setup().returning(|_| {});
        mock.expect_build_start_command()
            .returning(|folder, prompt| {
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
        mock.expect_build_resume_command()
            .returning(|folder, prompt| {
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
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(mock),
            db,
        )
        .await;

        // Act — create and start session (start command)
        create_and_start_session(&mut app, "SpawnInit").await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Assert
        {
            let session = &app.sessions[0];
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
            .returning(|folder, prompt| {
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
        app.reply_with_backend(0, "SpawnReply", &resume_mock);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Assert
        {
            let session = &app.sessions[0];
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
        assert!(app.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_create_session_with_git_no_actual_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mock = MockAgentBackend::new();
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(mock),
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
        let mock = MockAgentBackend::new();
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(mock),
            db,
        )
        .await;

        // Act
        let result = app.create_session().await;

        // Assert - session should not be created
        assert!(result.is_err());
        assert_eq!(app.sessions.len(), 0);

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
        assert_eq!(app.sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_commit_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.commit_session(0).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_commit_session_invalid_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.commit_session(99).await;

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
        let result = app.merge_session(0).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_merge_session_invalid_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.merge_session(99).await;

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
        wait_for_status(&app.sessions[0], Status::Review).await;
        app.commit_session(0)
            .await
            .expect("failed to commit session");
        let session_folder = app.sessions[0].folder.clone();
        let session_name = app.sessions[0].name.clone();
        let branch_name = format!("agentty/{session_name}");

        // Act
        let result = app.merge_session(0).await;

        // Assert
        assert!(result.is_ok(), "merge should succeed: {:?}", result.err());
        assert_eq!(app.sessions[0].status(), Status::Done);
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
        let result = app.create_pr_session(0).await;

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
        if let Ok(mut status) = app.sessions[0].status.lock() {
            *status = Status::Done;
        }

        // Act
        let result = app.create_pr_session(0).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("must be in review")
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_invalid_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.create_pr_session(99).await;

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
        assert!(app.sessions.is_empty());
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
        assert_eq!(app.sessions.len(), 1);

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
        assert_eq!(app.sessions.len(), 1);
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
        let sessions = app
            .db
            .load_sessions_for_project(project_id)
            .await
            .expect("failed to load");
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
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await;

        // Assert — repo_a (cwd) and repo_b (sibling) are discovered, plain_dir is not
        assert_eq!(app.projects.len(), 2);
        let paths: Vec<&Path> = app.projects.iter().map(|p| p.path.as_path()).collect();
        assert!(paths.contains(&repo_a.as_path()));
        assert!(paths.contains(&repo_b.as_path()));
    }
}
