use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::app::App;
use crate::db::Database;
use crate::git;
use crate::model::Project;

impl App {
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
}
