//! Roadmap cache helpers for the app core module.

use std::path::Path;

use super::state::App;
use crate::infra::fs::FsClient;

/// Repository-relative roadmap path that enables the project-specific
/// `Tasks` tab when present.
pub(super) const TASKS_ROADMAP_PATH: &str = "docs/plan/roadmap.md";

/// Cached roadmap snapshot for the active project `Tasks` tab.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ActiveProjectRoadmap {
    /// Successfully loaded roadmap markdown from `docs/plan/roadmap.md`.
    Loaded(String),
    /// Roadmap file exists but could not be read.
    LoadError(String),
}

impl App {
    /// Returns whether the active project exposes the roadmap file required by
    /// the `Tasks` tab.
    pub fn active_project_has_tasks_tab(&self) -> bool {
        self.active_project_has_tasks_tab
    }

    /// Returns the current `Tasks`-tab vertical scroll offset.
    pub fn task_roadmap_scroll_offset(&self) -> u16 {
        self.task_roadmap_scroll_offset
    }

    /// Scrolls the active project's roadmap view down by one wrapped line.
    pub fn scroll_task_roadmap_down(&mut self) {
        self.task_roadmap_scroll_offset = self.task_roadmap_scroll_offset.saturating_add(1);
    }

    /// Scrolls the active project's roadmap view up by one wrapped line.
    pub fn scroll_task_roadmap_up(&mut self) {
        self.task_roadmap_scroll_offset = self.task_roadmap_scroll_offset.saturating_sub(1);
    }

    /// Resets the active project's roadmap view back to the top.
    pub fn reset_task_roadmap_scroll(&mut self) {
        self.task_roadmap_scroll_offset = 0;
    }

    /// Refreshes cached roadmap availability and content for the active
    /// project.
    pub(super) async fn refresh_active_project_roadmap(&mut self) {
        self.active_project_roadmap = Self::load_project_roadmap(
            self.services.fs_client().as_ref(),
            self.projects.working_dir(),
        )
        .await;
        self.active_project_has_tasks_tab = self.active_project_roadmap.is_some();
        self.task_roadmap_scroll_offset = 0;
    }

    /// Refreshes cached roadmap state and re-normalizes top-level tabs.
    pub(super) async fn refresh_active_project_roadmap_and_tabs(&mut self) {
        self.refresh_active_project_roadmap().await;
        self.tabs.normalize(self.active_project_has_tasks_tab());
    }

    /// Loads the active project's roadmap snapshot when the roadmap file
    /// exists.
    pub(super) async fn load_project_roadmap(
        fs_client: &dyn FsClient,
        working_dir: &Path,
    ) -> Option<ActiveProjectRoadmap> {
        let roadmap_path = working_dir.join(TASKS_ROADMAP_PATH);
        if !fs_client.is_file(roadmap_path.clone()) {
            return None;
        }

        match fs_client.read_file(roadmap_path).await {
            Ok(contents) => Some(ActiveProjectRoadmap::Loaded(
                String::from_utf8_lossy(&contents).into_owned(),
            )),
            Err(error) => Some(ActiveProjectRoadmap::LoadError(format!(
                "Failed to load `{TASKS_ROADMAP_PATH}`: {error}"
            ))),
        }
    }
}
