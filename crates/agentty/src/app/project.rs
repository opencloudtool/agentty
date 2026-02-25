//! Active-project state, project discovery snapshots, and quick-switch helpers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::widgets::TableState;

use crate::domain::project::ProjectListItem;

/// Project domain state and git status tracking for the active project.
pub struct ProjectManager {
    active_project_id: i64,
    active_project_name: String,
    git_branch: Option<String>,
    git_status: Option<(u32, u32)>,
    git_status_cancel: Arc<AtomicBool>,
    previous_project_id: Option<i64>,
    project_items: Vec<ProjectListItem>,
    table_state: TableState,
    working_dir: PathBuf,
}

impl ProjectManager {
    /// Creates a project manager with initial active-project context and list.
    pub fn new(
        active_project_id: i64,
        active_project_name: String,
        git_branch: Option<String>,
        git_status_cancel: Arc<AtomicBool>,
        project_items: Vec<ProjectListItem>,
        working_dir: PathBuf,
    ) -> Self {
        let mut manager = Self {
            active_project_id,
            active_project_name,
            git_branch,
            git_status: None,
            git_status_cancel,
            previous_project_id: None,
            project_items,
            table_state: TableState::default(),
            working_dir,
        };
        manager.select_active_project_row();

        manager
    }

    /// Returns all persisted projects with list-level metadata.
    pub(crate) fn project_items(&self) -> &[ProjectListItem] {
        &self.project_items
    }

    /// Returns mutable table selection state for the projects list UI.
    pub(crate) fn project_table_state_mut(&mut self) -> &mut TableState {
        &mut self.table_state
    }

    /// Returns the active project identifier.
    pub(crate) fn active_project_id(&self) -> i64 {
        self.active_project_id
    }

    /// Returns the active project display name.
    pub(crate) fn project_name(&self) -> &str {
        &self.active_project_name
    }

    /// Returns the git branch of the active project, when available.
    pub(crate) fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Returns whether a git branch is configured for the active project.
    pub(crate) fn has_git_branch(&self) -> bool {
        self.git_branch.is_some()
    }

    /// Returns the latest ahead/behind snapshot.
    pub(crate) fn git_status(&self) -> Option<(u32, u32)> {
        self.git_status
    }

    /// Returns the active project working directory.
    pub(crate) fn working_dir(&self) -> &Path {
        self.working_dir.as_path()
    }

    /// Returns the current git-status cancellation token.
    pub(crate) fn git_status_cancel(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.git_status_cancel)
    }

    /// Replaces the git-status cancellation token and cancels the previous one.
    pub(crate) fn replace_git_status_cancel(&mut self) -> Arc<AtomicBool> {
        self.git_status_cancel.store(true, Ordering::Relaxed);

        let next_cancel_token = Arc::new(AtomicBool::new(false));
        self.git_status_cancel = Arc::clone(&next_cancel_token);

        next_cancel_token
    }

    /// Returns the selected project in the project list, when present.
    pub(crate) fn selected_project(&self) -> Option<&ProjectListItem> {
        let selected_index = self.table_state.selected()?;

        self.project_items.get(selected_index)
    }

    /// Returns the selected project identifier, when present.
    pub(crate) fn selected_project_id(&self) -> Option<i64> {
        self.selected_project()
            .map(|project_item| project_item.project.id)
    }

    /// Returns the previously active project identifier, when known.
    pub(crate) fn previous_project_id(&self) -> Option<i64> {
        self.previous_project_id
    }

    /// Selects the next project row.
    pub(crate) fn next_project(&mut self) {
        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        let next_index = match self.table_state.selected() {
            Some(selected_index) => (selected_index + 1) % self.project_items.len(),
            None => 0,
        };
        self.table_state.select(Some(next_index));
    }

    /// Selects the previous project row.
    pub(crate) fn previous_project(&mut self) {
        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        let previous_index = match self.table_state.selected() {
            Some(selected_index) => {
                if selected_index == 0 {
                    self.project_items.len() - 1
                } else {
                    selected_index - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(previous_index));
    }

    /// Updates the active project context values.
    pub(crate) fn update_active_project_context(
        &mut self,
        active_project_id: i64,
        active_project_name: String,
        git_branch: Option<String>,
        working_dir: PathBuf,
    ) {
        if self.active_project_id != active_project_id {
            self.previous_project_id = Some(self.active_project_id);
        }
        self.active_project_id = active_project_id;
        self.active_project_name = active_project_name;
        self.git_branch = git_branch;
        self.git_status = None;
        self.working_dir = working_dir;
        self.select_active_project_row();
    }

    /// Replaces loaded project list snapshots and keeps selection stable.
    pub(crate) fn replace_project_items(&mut self, project_items: Vec<ProjectListItem>) {
        let selected_project_id = self.selected_project_id();
        self.project_items = project_items;

        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        if let Some(selected_project_id) = selected_project_id
            && let Some(selected_index) = self
                .project_items
                .iter()
                .position(|project_item| project_item.project.id == selected_project_id)
        {
            self.table_state.select(Some(selected_index));

            return;
        }

        self.select_active_project_row();
    }

    /// Updates the last known git status.
    pub(crate) fn set_git_status(&mut self, git_status: Option<(u32, u32)>) {
        self.git_status = git_status;
    }

    /// Re-selects active project row in the projects list when present.
    fn select_active_project_row(&mut self) {
        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        let selected_index = self
            .project_items
            .iter()
            .position(|project_item| project_item.project.id == self.active_project_id)
            .unwrap_or(0);
        self.table_state.select(Some(selected_index));
    }
}

#[cfg(test)]
impl ProjectManager {
    /// Replaces the active project context values.
    ///
    /// Only available in test builds to set up test-specific state without
    /// exposing mutable context updates to production code.
    pub(crate) fn replace_context(
        &mut self,
        active_project_id: i64,
        git_branch: Option<String>,
        working_dir: PathBuf,
    ) {
        self.active_project_id = active_project_id;
        self.active_project_name =
            crate::domain::project::project_name_from_path(working_dir.as_path());
        self.git_branch = git_branch;
        self.working_dir = working_dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::project::{Project, ProjectListItem};

    #[test]
    fn test_next_project_wraps_to_first_row() {
        // Arrange
        let mut manager = project_manager_fixture();
        manager.project_table_state_mut().select(Some(1));

        // Act
        manager.next_project();

        // Assert
        assert_eq!(manager.project_table_state_mut().selected(), Some(0));
    }

    #[test]
    fn test_previous_project_wraps_to_last_row() {
        // Arrange
        let mut manager = project_manager_fixture();
        manager.project_table_state_mut().select(Some(0));

        // Act
        manager.previous_project();

        // Assert
        assert_eq!(manager.project_table_state_mut().selected(), Some(1));
    }

    fn project_manager_fixture() -> ProjectManager {
        let project_items = vec![
            ProjectListItem {
                last_session_updated_at: Some(10),
                project: Project {
                    created_at: 1,
                    display_name: Some("agentty".to_string()),
                    git_branch: Some("main".to_string()),
                    id: 1,
                    is_favorite: false,
                    last_opened_at: Some(20),
                    path: PathBuf::from("/tmp/agentty"),
                    updated_at: 2,
                },
                session_count: 3,
            },
            ProjectListItem {
                last_session_updated_at: Some(11),
                project: Project {
                    created_at: 1,
                    display_name: Some("service".to_string()),
                    git_branch: Some("main".to_string()),
                    id: 2,
                    is_favorite: true,
                    last_opened_at: Some(21),
                    path: PathBuf::from("/tmp/service"),
                    updated_at: 2,
                },
                session_count: 2,
            },
        ];

        ProjectManager::new(
            1,
            "agentty".to_string(),
            Some("main".to_string()),
            Arc::new(AtomicBool::new(false)),
            project_items,
            PathBuf::from("/tmp/agentty"),
        )
    }
}
