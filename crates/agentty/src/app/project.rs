//! Project discovery and active-project switching workflows.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::app::task::TaskService;
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::db::Database;
use crate::git;
use crate::model::Project;

/// Project domain state and git status tracking.
pub struct ProjectManager {
    active_project_id: i64,
    git_branch: Option<String>,
    git_status: Option<(u32, u32)>,
    git_status_cancel: Arc<AtomicBool>,
    projects: Vec<Project>,
    working_dir: PathBuf,
}

impl ProjectManager {
    /// Creates a project manager with initial state.
    pub fn new(
        active_project_id: i64,
        git_branch: Option<String>,
        git_status_cancel: Arc<AtomicBool>,
        projects: Vec<Project>,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            active_project_id,
            git_branch,
            git_status: None,
            git_status_cancel,
            projects,
            working_dir,
        }
    }

    /// Returns the active project identifier.
    pub(crate) fn active_project_id(&self) -> i64 {
        self.active_project_id
    }

    /// Returns the git branch of the active project, when available.
    pub(crate) fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Returns the latest ahead/behind snapshot.
    pub(crate) fn git_status(&self) -> Option<(u32, u32)> {
        self.git_status
    }

    /// Returns the active project working directory.
    pub(crate) fn working_dir(&self) -> &Path {
        self.working_dir.as_path()
    }

    /// Switches the active project context and reloads project sessions.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or session state cannot
    /// be reloaded from persisted storage.
    pub(super) async fn switch_project(
        &mut self,
        project_id: i64,
        services: &AppServices,
        sessions: &mut SessionManager,
    ) -> Result<(), String> {
        let project = services
            .db()
            .get_project(project_id)
            .await?
            .ok_or_else(|| "Project not found".to_string())?;

        self.cancel_git_status_task();
        self.replace_context(
            project_id,
            project.git_branch.clone(),
            PathBuf::from(&project.path),
        );
        self.reset_git_status();

        let new_cancel = Arc::new(AtomicBool::new(false));
        self.replace_git_status_cancel(new_cancel.clone());
        if self.has_git_branch() {
            TaskService::spawn_git_status_task(
                self.working_dir(),
                new_cancel,
                services.event_sender(),
            );
        }

        let projects = Self::load_projects_from_db(services.db()).await;
        self.replace_projects(projects);
        sessions.table_state.select(Some(0));
        services.emit_app_event(AppEvent::RefreshSessions);

        Ok(())
    }

    /// Scans sibling directories for git repositories and stores them as
    /// known projects.
    pub(super) async fn discover_sibling_projects(working_dir: &Path, db: &Database) {
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

    /// Loads all persisted projects from the database.
    pub(super) async fn load_projects_from_db(db: &Database) -> Vec<Project> {
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

    /// Replaces the in-memory project list snapshot.
    pub(crate) fn replace_projects(&mut self, projects: Vec<Project>) {
        self.projects = projects;
    }

    /// Replaces the active project context values.
    pub(crate) fn replace_context(
        &mut self,
        active_project_id: i64,
        git_branch: Option<String>,
        working_dir: PathBuf,
    ) {
        self.active_project_id = active_project_id;
        self.git_branch = git_branch;
        self.working_dir = working_dir;
    }

    /// Replaces the git-status cancellation token.
    pub(crate) fn replace_git_status_cancel(&mut self, cancel: Arc<AtomicBool>) {
        self.git_status_cancel = cancel;
    }

    /// Returns the current git-status cancellation token.
    pub(crate) fn git_status_cancel(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.git_status_cancel)
    }

    /// Updates the last known git status.
    pub(crate) fn set_git_status(&mut self, git_status: Option<(u32, u32)>) {
        self.git_status = git_status;
    }

    /// Resets the git status snapshot to unknown.
    pub(crate) fn reset_git_status(&mut self) {
        self.git_status = None;
    }

    /// Requests cancellation for the current git status background task.
    pub(crate) fn cancel_git_status_task(&self) {
        self.git_status_cancel.store(true, Ordering::Relaxed);
    }

    /// Returns whether a git branch is configured for the active project.
    pub(crate) fn has_git_branch(&self) -> bool {
        self.git_branch.is_some()
    }
}

impl Deref for ProjectManager {
    type Target = Vec<Project>;

    fn deref(&self) -> &Self::Target {
        &self.projects
    }
}

impl DerefMut for ProjectManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.projects
    }
}
