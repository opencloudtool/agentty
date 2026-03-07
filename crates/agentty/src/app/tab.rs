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
    Stats,
    Settings,
}

impl Tab {
    /// Tabs in the order they are rendered and cycled in list mode.
    pub const ALL: [Self; 4] = [Self::Projects, Self::Sessions, Self::Stats, Self::Settings];
    /// Project-scoped tabs in display order.
    pub const PROJECT_SCOPED: [Self; 3] = [Self::Sessions, Self::Stats, Self::Settings];

    /// Returns the display label used in the tabs header.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Stats => "Stats",
            Tab::Settings => "Settings",
        }
    }

    /// Returns whether the tab is global or tied to the active project.
    #[must_use]
    pub fn scope(self) -> TabScope {
        match self {
            Tab::Projects => TabScope::Global,
            Tab::Sessions | Tab::Stats | Tab::Settings => TabScope::Project,
        }
    }

    /// Cycles to the next tab in display order.
    #[must_use]
    pub fn next(self) -> Self {
        let tab_index = self.index();
        let next_index = (tab_index + 1) % Self::ALL.len();

        Self::ALL[next_index]
    }

    /// Cycles to the previous tab in display order.
    #[must_use]
    pub fn previous(self) -> Self {
        let tab_index = self.index();
        let previous_index = (tab_index + Self::ALL.len() - 1) % Self::ALL.len();

        Self::ALL[previous_index]
    }

    /// Returns the display-order index for the tab.
    fn index(self) -> usize {
        match Self::ALL.iter().position(|tab| *tab == self) {
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
    pub fn next(&mut self) {
        self.current = self.current.next();
    }

    /// Cycles selection to the previous tab.
    pub fn previous(&mut self) {
        self.current = self.current.previous();
    }

    /// Sets the currently selected tab.
    pub fn set(&mut self, tab: Tab) {
        self.current = tab;
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
        let titles = Tab::ALL.map(Tab::title);

        // Assert
        assert_eq!(titles, ["Projects", "Sessions", "Stats", "Settings"]);
    }

    #[test]
    fn test_tab_scope_marks_only_projects_as_global() {
        // Arrange

        // Act
        let scopes = Tab::ALL.map(Tab::scope);

        // Assert
        assert_eq!(
            scopes,
            [
                TabScope::Global,
                TabScope::Project,
                TabScope::Project,
                TabScope::Project
            ]
        );
    }

    #[test]
    fn test_tab_next() {
        // Arrange

        // Act
        let next_tabs = Tab::ALL.map(Tab::next);

        // Assert
        assert_eq!(
            next_tabs,
            [Tab::Sessions, Tab::Stats, Tab::Settings, Tab::Projects]
        );
    }

    #[test]
    fn test_tab_previous() {
        // Arrange

        // Act
        let previous_tabs = Tab::ALL.map(Tab::previous);

        // Assert
        assert_eq!(
            previous_tabs,
            [Tab::Settings, Tab::Projects, Tab::Sessions, Tab::Stats]
        );
    }

    #[test]
    fn test_tab_project_scoped_order_keeps_project_pages_grouped() {
        // Arrange

        // Act
        let project_scoped_tabs = Tab::PROJECT_SCOPED;

        // Assert
        assert_eq!(
            project_scoped_tabs,
            [Tab::Sessions, Tab::Stats, Tab::Settings]
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
    fn test_tab_manager_next_cycles_tabs() {
        // Arrange
        let mut manager = TabManager::new();
        let mut observed_tabs = Vec::new();

        // Act
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());

        // Assert
        assert_eq!(
            observed_tabs,
            vec![
                Tab::Projects,
                Tab::Sessions,
                Tab::Stats,
                Tab::Settings,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_manager_previous_cycles_tabs() {
        // Arrange
        let mut manager = TabManager::new();
        let mut observed_tabs = Vec::new();

        // Act
        observed_tabs.push(manager.current());
        manager.previous();
        observed_tabs.push(manager.current());
        manager.previous();
        observed_tabs.push(manager.current());
        manager.previous();
        observed_tabs.push(manager.current());
        manager.previous();
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
    fn test_tab_manager_set_updates_current_tab() {
        // Arrange
        let mut manager = TabManager::new();

        // Act
        manager.set(Tab::Settings);

        // Assert
        assert_eq!(manager.current(), Tab::Settings);
    }
}
