//! List-view tab definitions and state management.

/// Describes whether a tab is global or tied to the active project.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabScope {
    Global,
    Project,
}

/// Available top-level tabs in list mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tab {
    Projects,
    Sessions,
    Tasks,
    Stats,
    Settings,
}

impl Tab {
    /// Tabs in the order they are rendered when the active project does not
    /// expose a roadmap-backed tasks page.
    pub const ALL_WITHOUT_TASKS: [Self; 4] =
        [Self::Projects, Self::Sessions, Self::Stats, Self::Settings];
    /// Tabs in the order they are rendered when the active project exposes a
    /// roadmap-backed tasks page.
    pub const ALL_WITH_TASKS: [Self; 5] = [
        Self::Projects,
        Self::Sessions,
        Self::Tasks,
        Self::Stats,
        Self::Settings,
    ];
    /// Project-scoped tabs in display order when the tasks page is hidden.
    pub const PROJECT_SCOPED_WITHOUT_TASKS: [Self; 3] =
        [Self::Sessions, Self::Stats, Self::Settings];
    /// Project-scoped tabs in display order when the tasks page is available.
    pub const PROJECT_SCOPED_WITH_TASKS: [Self; 4] =
        [Self::Sessions, Self::Tasks, Self::Stats, Self::Settings];

    /// Returns the tabs available for the current project context.
    pub fn available_tabs(has_tasks_tab: bool) -> &'static [Self] {
        if has_tasks_tab {
            return &Self::ALL_WITH_TASKS;
        }

        &Self::ALL_WITHOUT_TASKS
    }

    /// Returns the project-scoped tabs available for the current project
    /// context.
    pub fn project_scoped_tabs(has_tasks_tab: bool) -> &'static [Self] {
        if has_tasks_tab {
            return &Self::PROJECT_SCOPED_WITH_TASKS;
        }

        &Self::PROJECT_SCOPED_WITHOUT_TASKS
    }

    /// Returns the display label used in the tabs header.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Tasks => "Tasks",
            Tab::Stats => "Stats",
            Tab::Settings => "Settings",
        }
    }

    /// Returns whether the tab is global or tied to the active project.
    #[must_use]
    pub fn scope(self) -> TabScope {
        match self {
            Tab::Projects => TabScope::Global,
            Tab::Sessions | Tab::Tasks | Tab::Stats | Tab::Settings => TabScope::Project,
        }
    }

    /// Cycles to the next tab in display order.
    #[must_use]
    fn next(self, has_tasks_tab: bool) -> Self {
        let tabs = Self::available_tabs(has_tasks_tab);
        let tab_index = self.index(has_tasks_tab);
        let next_index = (tab_index + 1) % tabs.len();

        tabs[next_index]
    }

    /// Cycles to the previous tab in display order.
    #[must_use]
    fn previous(self, has_tasks_tab: bool) -> Self {
        let tabs = Self::available_tabs(has_tasks_tab);
        let tab_index = self.index(has_tasks_tab);
        let previous_index = (tab_index + tabs.len() - 1) % tabs.len();

        tabs[previous_index]
    }

    /// Returns the display-order index for the tab.
    fn index(self, has_tasks_tab: bool) -> usize {
        match Self::available_tabs(has_tasks_tab)
            .iter()
            .position(|tab| *tab == self)
        {
            Some(tab_index) => tab_index,
            None => unreachable!("tab must exist in the display order"),
        }
    }
}

/// Manages selection state for top-level tabs.
pub struct TabManager {
    current: Tab,
}

impl TabManager {
    /// Creates a manager with `Tab::Projects` selected.
    pub fn new() -> Self {
        Self {
            current: Tab::Projects,
        }
    }

    /// Returns the currently selected tab.
    #[must_use]
    pub fn current(&self) -> Tab {
        self.current
    }

    /// Cycles selection to the next tab.
    pub fn next(&mut self, has_tasks_tab: bool) {
        self.normalize(has_tasks_tab);
        self.current = self.current.next(has_tasks_tab);
    }

    /// Cycles selection to the previous tab.
    pub fn previous(&mut self, has_tasks_tab: bool) {
        self.normalize(has_tasks_tab);
        self.current = self.current.previous(has_tasks_tab);
    }

    /// Sets the currently selected tab.
    pub fn set(&mut self, tab: Tab) {
        self.current = tab;
    }

    /// Falls back to `Tab::Sessions` when the current tab is no longer
    /// available for the active project.
    pub fn normalize(&mut self, has_tasks_tab: bool) {
        if self.current == Tab::Tasks && !has_tasks_tab {
            self.current = Tab::Sessions;
        }
    }
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tab_title() {
        // Arrange

        // Act
        let titles = Tab::ALL_WITH_TASKS.map(Tab::title);

        // Assert
        assert_eq!(
            titles,
            ["Projects", "Sessions", "Tasks", "Stats", "Settings"]
        );
    }

    #[test]
    fn test_tab_scope_marks_only_projects_as_global() {
        // Arrange

        // Act
        let scopes = Tab::ALL_WITH_TASKS.map(Tab::scope);

        // Assert
        assert_eq!(
            scopes,
            [
                TabScope::Global,
                TabScope::Project,
                TabScope::Project,
                TabScope::Project,
                TabScope::Project
            ]
        );
    }

    #[test]
    fn test_tab_next_with_tasks() {
        // Arrange

        // Act
        let next_tabs = Tab::ALL_WITH_TASKS.map(|tab| tab.next(true));

        // Assert
        assert_eq!(
            next_tabs,
            [
                Tab::Sessions,
                Tab::Tasks,
                Tab::Stats,
                Tab::Settings,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_next_without_tasks_skips_tasks_tab() {
        // Arrange

        // Act
        let next_tabs = Tab::ALL_WITHOUT_TASKS.map(|tab| tab.next(false));

        // Assert
        assert_eq!(
            next_tabs,
            [Tab::Sessions, Tab::Stats, Tab::Settings, Tab::Projects]
        );
    }

    #[test]
    fn test_tab_previous_with_tasks() {
        // Arrange

        // Act
        let previous_tabs = Tab::ALL_WITH_TASKS.map(|tab| tab.previous(true));

        // Assert
        assert_eq!(
            previous_tabs,
            [
                Tab::Settings,
                Tab::Projects,
                Tab::Sessions,
                Tab::Tasks,
                Tab::Stats
            ]
        );
    }

    #[test]
    fn test_tab_previous_without_tasks_skips_tasks_tab() {
        // Arrange

        // Act
        let previous_tabs = Tab::ALL_WITHOUT_TASKS.map(|tab| tab.previous(false));

        // Assert
        assert_eq!(
            previous_tabs,
            [Tab::Settings, Tab::Projects, Tab::Sessions, Tab::Stats]
        );
    }

    #[test]
    fn test_tab_project_scoped_order_with_tasks_keeps_project_pages_grouped() {
        // Arrange

        // Act
        let project_scoped_tabs = Tab::project_scoped_tabs(true);

        // Assert
        assert_eq!(
            project_scoped_tabs,
            &[Tab::Sessions, Tab::Tasks, Tab::Stats, Tab::Settings]
        );
    }

    #[test]
    fn test_tab_project_scoped_order_without_tasks_keeps_project_pages_grouped() {
        // Arrange

        // Act
        let project_scoped_tabs = Tab::project_scoped_tabs(false);

        // Assert
        assert_eq!(
            project_scoped_tabs,
            &[Tab::Sessions, Tab::Stats, Tab::Settings]
        );
    }

    #[test]
    fn test_tab_manager_new_defaults_to_projects() {
        // Arrange

        // Act
        let manager = TabManager::new();

        // Assert
        assert_eq!(manager.current(), Tab::Projects);
    }

    #[test]
    fn test_tab_manager_next_cycles_tabs_with_tasks() {
        // Arrange
        let mut manager = TabManager::new();
        let mut observed_tabs = Vec::new();

        // Act
        observed_tabs.push(manager.current());
        manager.next(true);
        observed_tabs.push(manager.current());
        manager.next(true);
        observed_tabs.push(manager.current());
        manager.next(true);
        observed_tabs.push(manager.current());
        manager.next(true);
        observed_tabs.push(manager.current());
        manager.next(true);
        observed_tabs.push(manager.current());

        // Assert
        assert_eq!(
            observed_tabs,
            vec![
                Tab::Projects,
                Tab::Sessions,
                Tab::Tasks,
                Tab::Stats,
                Tab::Settings,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_manager_previous_cycles_tabs_without_tasks() {
        // Arrange
        let mut manager = TabManager::new();
        let mut observed_tabs = Vec::new();

        // Act
        observed_tabs.push(manager.current());
        manager.previous(false);
        observed_tabs.push(manager.current());
        manager.previous(false);
        observed_tabs.push(manager.current());
        manager.previous(false);
        observed_tabs.push(manager.current());
        manager.previous(false);
        observed_tabs.push(manager.current());

        // Assert
        assert_eq!(
            observed_tabs,
            vec![
                Tab::Projects,
                Tab::Settings,
                Tab::Stats,
                Tab::Sessions,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_manager_normalize_falls_back_from_hidden_tasks_tab() {
        // Arrange
        let mut manager = TabManager::new();
        manager.set(Tab::Tasks);

        // Act
        manager.normalize(false);

        // Assert
        assert_eq!(manager.current(), Tab::Sessions);
    }

    #[test]
    fn test_tab_manager_set_updates_current_tab() {
        // Arrange
        let mut manager = TabManager::new();

        // Act
        manager.set(Tab::Settings);

        // Assert
        assert_eq!(manager.current(), Tab::Settings);
    }
}
