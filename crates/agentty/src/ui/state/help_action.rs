/// One user-visible shortcut entry that can be rendered in the footer and
/// in the help popup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelpAction {
    pub(crate) footer_label: &'static str,
    pub(crate) key: &'static str,
    pub(crate) popup_label: &'static str,
}

impl HelpAction {
    /// Creates one help action descriptor.
    pub(crate) const fn new(
        footer_label: &'static str,
        key: &'static str,
        popup_label: &'static str,
    ) -> Self {
        Self {
            footer_label,
            key,
            popup_label,
        }
    }
}

/// Encodes which shortcut family is available for the viewed session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewSessionState {
    /// Session is completed; only read-only view actions and output toggling
    /// remain available.
    Done,
    /// Session is currently running; open-worktree and stop are available but
    /// edit and diff shortcuts are hidden.
    InProgress,
    /// Session is ready for review; full edit shortcuts, including diff, are
    /// available.
    Review,
    /// Session allows reply/merge/rebase actions but is not in review mode, so
    /// diff remains hidden.
    Interactive,
}

/// Action availability snapshot for view-mode help projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ViewHelpState {
    pub(crate) session_state: ViewSessionState,
}

/// Returns help actions for the sessions page in list mode.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn session_list_actions(
    can_cancel_selected_session: bool,
    can_delete_selected_session: bool,
    can_open_selected_session: bool,
) -> Vec<HelpAction> {
    let mut actions = list_base_actions();

    if can_delete_selected_session {
        actions.push(HelpAction::new("delete", "d", "Delete session"));
    }

    if can_cancel_selected_session {
        actions.push(HelpAction::new("cancel", "c", "Cancel session"));
    }

    if can_open_selected_session {
        actions.push(HelpAction::new("view", "Enter", "Open session"));
    }

    actions.push(HelpAction::new("nav", "j/k", "Navigate sessions"));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns help actions for the projects page.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn project_list_actions() -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.push(HelpAction::new("switch", "Enter", "Switch active project"));
    actions.push(HelpAction::new(
        "previous",
        "b",
        "Switch to previous project",
    ));
    actions.push(HelpAction::new("nav", "j/k", "Navigate projects"));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact projects footer actions for the page-level hint line.
pub(crate) fn project_list_footer_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("switch", "Enter", "Switch active project"),
        HelpAction::new("previous", "b", "Switch to previous project"),
        HelpAction::new("quick switch", "p", "Open switcher"),
        HelpAction::new("nav", "j/k", "Navigate projects"),
        HelpAction::new("next tab", "Tab", "Switch tab"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns compact session list footer actions for the page-level hint line.
pub(crate) fn session_list_footer_actions(can_open_selected_session: bool) -> Vec<HelpAction> {
    let mut actions = list_base_actions();

    if can_open_selected_session {
        actions.push(HelpAction::new("view", "Enter", "Open session"));
    }

    actions.push(HelpAction::new("nav", "j/k", "Navigate sessions"));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns help actions for the settings page.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn settings_actions() -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.push(HelpAction::new("nav", "j/k", "Navigate settings"));
    actions.push(HelpAction::new("edit", "Enter", "Edit setting"));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact settings footer actions for the page-level hint line.
pub(crate) fn settings_footer_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("nav", "j/k", "Navigate settings"),
        HelpAction::new("edit", "Enter", "Edit setting"),
        HelpAction::new("next tab", "Tab", "Switch tab"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns help actions for the stats page.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn stats_actions() -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact stats footer actions for the page-level hint line.
pub(crate) fn stats_footer_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("next tab", "Tab", "Switch tab"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Projects currently available view-mode actions into help entries.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn view_actions(state: ViewHelpState) -> Vec<HelpAction> {
    let can_open_worktree = state.session_state != ViewSessionState::Done;
    let can_edit_session = matches!(
        state.session_state,
        ViewSessionState::Interactive | ViewSessionState::Review
    );
    let can_show_diff = state.session_state == ViewSessionState::Review;
    let can_toggle_done_output = state.session_state == ViewSessionState::Done;

    let mut actions = vec![HelpAction::new("back", "q", "Back to list")];

    if can_edit_session {
        actions.push(HelpAction::new("reply", "Enter", "Reply"));
    }

    if can_open_worktree {
        actions.push(HelpAction::new("open", "o", "Open worktree"));
    }

    if can_show_diff {
        actions.push(HelpAction::new("diff", "d", "Show diff"));
    }

    if can_edit_session {
        actions.push(HelpAction::new("queue merge", "m", "Queue merge"));
        actions.push(HelpAction::new("rebase", "r", "Rebase"));
        actions.push(HelpAction::new("mode", "S-Tab", "Toggle permission mode"));
    }

    if state.session_state == ViewSessionState::InProgress {
        actions.push(HelpAction::new("stop", "Ctrl+c", "Stop agent"));
    }

    if can_toggle_done_output {
        actions.push(HelpAction::new("toggle view", "t", "Switch summary/output"));
    }

    actions.push(HelpAction::new("scroll", "j/k", "Scroll output"));
    actions.push(HelpAction::new("top", "g", "Scroll to top"));
    actions.push(HelpAction::new("bottom", "G", "Scroll to bottom"));
    actions.push(HelpAction::new("half down", "Ctrl+d", "Half page down"));
    actions.push(HelpAction::new("half up", "Ctrl+u", "Half page up"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact session-view footer actions for the page-level hint line.
///
/// Interactive and review sessions include merge and rebase controls directly
/// in the footer to keep those actions discoverable without opening help.
pub(crate) fn view_footer_actions(state: ViewHelpState) -> Vec<HelpAction> {
    let can_open_worktree = state.session_state != ViewSessionState::Done;
    let can_edit_session = matches!(
        state.session_state,
        ViewSessionState::Interactive | ViewSessionState::Review
    );

    let mut actions = vec![HelpAction::new("back", "q", "Back to list")];

    if can_edit_session {
        actions.push(HelpAction::new("reply", "Enter", "Reply"));
        actions.push(HelpAction::new("queue merge", "m", "Queue merge"));
        actions.push(HelpAction::new("rebase", "r", "Rebase"));
    }

    if can_open_worktree {
        actions.push(HelpAction::new("open", "o", "Open worktree"));
    }

    if state.session_state == ViewSessionState::InProgress {
        actions.push(HelpAction::new("stop", "Ctrl+c", "Stop agent"));
    }

    if state.session_state == ViewSessionState::Done {
        actions.push(HelpAction::new("toggle view", "t", "Switch summary/output"));
    }

    actions.push(HelpAction::new("scroll", "j/k", "Scroll output"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns help entries for diff-mode actions.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn diff_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("back", "q/Esc", "Back to session"),
        HelpAction::new("select file", "j/k", "Select file"),
        HelpAction::new("scroll file", "Up/Down", "Scroll selected file"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns compact diff footer actions for the page-level hint line.
pub(crate) fn diff_footer_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("back", "q/Esc", "Back to session"),
        HelpAction::new("select file", "j/k", "Select file"),
        HelpAction::new("scroll file", "Up/Down", "Scroll selected file"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Renders one-line footer help text from projected actions.
pub(crate) fn footer_text(actions: &[HelpAction]) -> String {
    let mut help_text = String::new();

    for (index, action) in actions.iter().enumerate() {
        if index > 0 {
            help_text.push_str(" | ");
        }

        help_text.push_str(action.key);
        help_text.push_str(": ");
        help_text.push_str(action.footer_label);
    }

    help_text
}

/// Returns list-mode actions that are shared by sessions, stats, and settings
/// pages.
///
/// The `"a"` shortcut starts a new session.
fn list_base_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("start new session", "a", "Start new session"),
        HelpAction::new("quick switch", "p", "Open project switcher"),
        HelpAction::new("sync", "s", "Sync"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_list_actions_hide_enter_without_openable_session() {
        // Arrange
        // Act
        let actions = session_list_actions(false, false, false);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "j/k"));
    }

    #[test]
    fn test_session_list_footer_actions_hides_non_critical_session_commands() {
        // Arrange

        // Act
        let actions = session_list_footer_actions(true);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "c"));
    }

    #[test]
    fn test_view_actions_in_progress_shows_open_and_stop_and_hides_edit_actions() {
        // Arrange
        let state = ViewHelpState {
            session_state: ViewSessionState::InProgress,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_review_shows_diff() {
        // Arrange
        let state = ViewHelpState {
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "d"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
    }

    #[test]
    fn test_view_actions_interactive_hides_diff() {
        // Arrange
        let state = ViewHelpState {
            session_state: ViewSessionState::Interactive,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
    }

    #[test]
    fn test_view_actions_done_shows_toggle_and_hides_edit_actions() {
        // Arrange
        let state = ViewHelpState {
            session_state: ViewSessionState::Done,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "t"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_review_shows_advanced_actions() {
        // Arrange
        let state = ViewHelpState {
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_footer_text_joins_actions_in_order() {
        // Arrange
        let actions = vec![
            HelpAction::new("quit", "q", "Quit"),
            HelpAction::new("help", "?", "Help"),
        ];

        // Act
        let help_text = footer_text(&actions);

        // Assert
        assert_eq!(help_text, "q: quit | ?: help");
    }
}
