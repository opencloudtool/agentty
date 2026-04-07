use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::domain::session::{FollowUpTaskAction, PublishBranchAction, Session, Status};

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
    /// Session was canceled locally; read-only navigation remains available,
    /// plus any linked review-request refresh flow.
    Canceled,
    /// Session is currently running; worktree-open remains available, while
    /// reply and diff shortcuts are hidden.
    InProgress,
    /// Session is rebasing; worktree-open remains available, while reply and
    /// diff shortcuts are hidden.
    Rebasing,
    /// Session is in merge-queue processing; only read-only navigation
    /// shortcuts are available.
    MergeQueue,
    /// Session is still collecting staged draft messages before its first
    /// live turn starts.
    NewSession,
    /// Session is ready for review; reply, worktree-open, merge, rebase,
    /// review, and diff shortcuts are available.
    Review,
    /// Session is generating focused review output; reply, worktree-open,
    /// merge, review, and diff stay available while rebase remains hidden
    /// until the status returns to `Review`.
    AgentReview,
    /// Session allows reply/merge/rebase actions but is not in review mode, so
    /// diff remains hidden.
    Interactive,
}

/// Action availability snapshot for view-mode help projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ViewHelpState {
    /// Whether the session can sync its review request state from the forge.
    pub(crate) can_sync_review_request: bool,
    /// Launch/open action available for the selected follow-up task, when the
    /// current session exposes one.
    pub(crate) follow_up_task_action: Option<FollowUpTaskAction>,
    /// Whether the current session exposes more than one follow-up task to
    /// cycle through.
    pub(crate) has_multiple_follow_up_tasks: bool,
    /// Branch-publish action available for the current session, when any.
    pub(crate) publish_branch_action: Option<PublishBranchAction>,
    /// High-level view-mode state that gates the rest of the shortcut set.
    pub(crate) session_state: ViewSessionState,
}

/// Maps one session snapshot into the shared view-mode shortcut state used by
/// both runtime handlers and footer rendering.
pub(crate) fn session_view_state(session: &Session) -> ViewSessionState {
    match session.status {
        Status::Done => ViewSessionState::Done,
        Status::Canceled => ViewSessionState::Canceled,
        Status::InProgress => ViewSessionState::InProgress,
        Status::New if session.is_draft_session() => ViewSessionState::NewSession,
        Status::New => ViewSessionState::Interactive,
        Status::Question => ViewSessionState::Interactive,
        Status::Rebasing => ViewSessionState::Rebasing,
        Status::Merging | Status::Queued => ViewSessionState::MergeQueue,
        Status::Review => ViewSessionState::Review,
        Status::AgentReview => ViewSessionState::AgentReview,
    }
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
    actions.push(HelpAction::new(
        "start new session",
        "a",
        "Start new session",
    ));
    actions.push(HelpAction::new(
        "start draft session",
        "Shift+A",
        "Start draft session",
    ));

    if can_delete_selected_session {
        actions.push(HelpAction::new("delete", "d", "Delete session"));
    }

    if can_cancel_selected_session {
        actions.push(HelpAction::new("cancel", "c", "Cancel session"));
    }

    if can_open_selected_session {
        actions.push(HelpAction::new("open session", "Enter", "Open session"));
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
    actions.push(HelpAction::new("select", "Enter", "Select active project"));
    actions.push(HelpAction::new("nav", "j/k", "Navigate projects"));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact projects footer actions for the page-level hint line.
pub(crate) fn project_list_footer_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("select", "Enter", "Select active project"),
        HelpAction::new("nav", "j/k", "Navigate projects"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns compact session list footer actions for the page-level hint line.
pub(crate) fn session_list_footer_actions(can_open_selected_session: bool) -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.push(HelpAction::new(
        "start new session",
        "a",
        "Start new session",
    ));
    actions.push(HelpAction::new(
        "start draft",
        "Shift+A",
        "Start draft session",
    ));

    if can_open_selected_session {
        actions.push(HelpAction::new("open session", "Enter", "Open session"));
    }

    actions.push(HelpAction::new("nav", "j/k", "Navigate sessions"));
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
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Projects currently available view-mode actions into help entries.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn view_actions(state: ViewHelpState) -> Vec<HelpAction> {
    let can_open_worktree = matches!(
        state.session_state,
        ViewSessionState::Interactive
            | ViewSessionState::InProgress
            | ViewSessionState::NewSession
            | ViewSessionState::Rebasing
            | ViewSessionState::Review
            | ViewSessionState::AgentReview
    );
    let can_edit_session = matches!(
        state.session_state,
        ViewSessionState::Interactive
            | ViewSessionState::NewSession
            | ViewSessionState::Review
            | ViewSessionState::AgentReview
    );
    let can_show_diff = matches!(
        state.session_state,
        ViewSessionState::Review | ViewSessionState::AgentReview
    );
    let can_show_review = matches!(
        state.session_state,
        ViewSessionState::Review | ViewSessionState::AgentReview
    );
    let can_toggle_done_output = state.session_state == ViewSessionState::Done;

    let mut actions = vec![HelpAction::new("back", "q", "Back to list")];

    append_view_prompt_actions(&mut actions, state.session_state, can_edit_session);

    if state.session_state == ViewSessionState::NewSession {
        actions.push(HelpAction::new("start", "s", "Start staged session"));
    }

    if can_open_worktree {
        actions.push(HelpAction::new("open", "o", "Open worktree"));
    }

    if can_show_diff {
        actions.push(HelpAction::new("diff", "d", "Show diff"));
    }

    if can_show_review {
        actions.push(HelpAction::new("review", "f", "Focused review"));
    }

    if let Some(publish_branch_action) = state.publish_branch_action {
        actions.push(publish_branch_help_action(publish_branch_action));
    }

    if state.can_sync_review_request {
        actions.push(HelpAction::new("sync", "s", "Sync review request status"));
    }

    if can_edit_session {
        actions.push(HelpAction::new(
            "add to merge queue",
            "m",
            "Add to merge queue",
        ));

        if state.session_state != ViewSessionState::AgentReview {
            actions.push(HelpAction::new("rebase", "r", "Rebase"));
        }
    }

    if can_toggle_done_output {
        actions.push(HelpAction::new("toggle view", "t", "Switch summary/output"));
    }

    if let Some(follow_up_task_action) = state.follow_up_task_action {
        actions.push(follow_up_task_help_action(follow_up_task_action));
    }

    if state.has_multiple_follow_up_tasks {
        actions.push(HelpAction::new(
            "prev task",
            "[",
            "Select previous follow-up task",
        ));
        actions.push(HelpAction::new(
            "next task",
            "]",
            "Select next follow-up task",
        ));
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
/// Interactive and review-oriented sessions keep merge controls discoverable
/// in the footer, while `AgentReview` hides the rebase shortcut until focused
/// review generation finishes.
pub(crate) fn view_footer_actions(state: ViewHelpState) -> Vec<HelpAction> {
    let can_open_worktree = matches!(
        state.session_state,
        ViewSessionState::Interactive
            | ViewSessionState::InProgress
            | ViewSessionState::NewSession
            | ViewSessionState::Rebasing
            | ViewSessionState::Review
            | ViewSessionState::AgentReview
    );
    let can_edit_session = matches!(
        state.session_state,
        ViewSessionState::Interactive
            | ViewSessionState::NewSession
            | ViewSessionState::Review
            | ViewSessionState::AgentReview
    );
    let can_show_review = matches!(
        state.session_state,
        ViewSessionState::Review | ViewSessionState::AgentReview
    );

    let mut actions = vec![HelpAction::new("back", "q", "Back to list")];

    append_view_footer_edit_actions(&mut actions, state.session_state, can_edit_session);

    if can_open_worktree {
        actions.push(HelpAction::new("open", "o", "Open worktree"));
    }

    if can_show_review {
        actions.push(HelpAction::new("review", "f", "Focused review"));
    }

    if let Some(publish_branch_action) = state.publish_branch_action {
        actions.push(publish_branch_help_action(publish_branch_action));
    }

    if state.session_state == ViewSessionState::Done {
        actions.push(HelpAction::new("toggle view", "t", "Switch summary/output"));
    }

    if let Some(follow_up_task_action) = state.follow_up_task_action {
        actions.push(follow_up_task_help_action(follow_up_task_action));
    }

    if state.has_multiple_follow_up_tasks {
        actions.push(HelpAction::new(
            "prev task",
            "[",
            "Select previous follow-up task",
        ));
        actions.push(HelpAction::new(
            "next task",
            "]",
            "Select next follow-up task",
        ));
    }

    actions.push(HelpAction::new("scroll", "j/k", "Scroll output"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Appends footer actions that operate on an editable session in their
/// canonical order.
///
/// The explicit draft-session start action stays grouped with the prompt,
/// merge, and rebase controls so future edits do not accidentally apply a
/// different guard to one of those related actions. `AgentReview` keeps the
/// prompt and merge actions visible while suppressing `r` until background
/// review generation finishes.
fn append_view_footer_edit_actions(
    actions: &mut Vec<HelpAction>,
    session_state: ViewSessionState,
    can_edit_session: bool,
) {
    if !can_edit_session {
        return;
    }

    append_view_prompt_actions(actions, session_state, can_edit_session);

    if session_state == ViewSessionState::NewSession {
        actions.push(HelpAction::new("start", "s", "Start staged session"));
    }

    actions.push(HelpAction::new(
        "add to merge queue",
        "m",
        "Add to merge queue",
    ));

    if session_state != ViewSessionState::AgentReview {
        actions.push(HelpAction::new("rebase", "r", "Rebase"));
    }
}

/// Appends the session-view shortcuts that open the prompt composer.
///
/// Editable sessions expose both `Enter` for a blank composer and `/` for the
/// commands menu with a prefilled leading slash.
fn append_view_prompt_actions(
    actions: &mut Vec<HelpAction>,
    session_state: ViewSessionState,
    can_edit_session: bool,
) {
    if !can_edit_session {
        return;
    }

    actions.push(prompt_action_help_action(session_state));
    actions.push(HelpAction::new("commands menu", "/", "Open commands menu"));
}

/// Returns the `Enter` prompt-entry action label appropriate for the current
/// session state.
fn prompt_action_help_action(session_state: ViewSessionState) -> HelpAction {
    if session_state == ViewSessionState::NewSession {
        return HelpAction::new("add draft", "Enter", "Add draft");
    }

    HelpAction::new("reply", "Enter", "Reply")
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

/// Renders one-line footer help as styled spans where keys are emphasized and
/// labels are muted for faster scanning.
pub(crate) fn footer_line(actions: &[HelpAction]) -> Line<'static> {
    let mut spans = Vec::new();

    for (index, action) in actions.iter().enumerate() {
        if index > 0 {
            spans.push(footer_separator_span());
        }

        spans.push(footer_key_span(action.key));
        spans.push(footer_muted_span(": "));
        spans.push(footer_muted_span(action.footer_label));
    }

    Line::from(spans)
}

/// Returns one highlighted footer key span.
pub(crate) fn footer_key_span(key: &'static str) -> Span<'static> {
    Span::styled(
        key.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

/// Returns one muted footer text span for labels and informational notes.
pub(crate) fn footer_muted_span(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(Color::Gray))
}

/// Returns the shared separator span used between footer help items.
pub(crate) fn footer_separator_span() -> Span<'static> {
    Span::styled(" | ", Style::default().fg(Color::DarkGray))
}

/// Returns list-mode actions shared by all tabs.
fn list_base_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("sync", "s", "Sync"),
    ]
}

/// Returns the view-mode shortcut entry for the current branch-publish action.
fn publish_branch_help_action(action: PublishBranchAction) -> HelpAction {
    match action {
        PublishBranchAction::Push => {
            HelpAction::new("publish branch", "p", "Publish session branch to remote")
        }
    }
}

/// Returns the view-mode shortcut entry for the selected follow-up task.
fn follow_up_task_help_action(action: FollowUpTaskAction) -> HelpAction {
    match action {
        FollowUpTaskAction::Launch => HelpAction::new("launch task", "l", "Launch sibling session"),
        FollowUpTaskAction::Open => {
            HelpAction::new("open task", "l", "Open launched sibling session")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::PublishBranchAction;

    #[test]
    fn test_project_list_actions_exclude_new_session_shortcut() {
        // Arrange
        // Act
        let actions = project_list_actions();

        // Assert
        assert!(!actions.iter().any(|action| action.key == "a"));
    }

    #[test]
    fn test_settings_actions_exclude_new_session_shortcut() {
        // Arrange
        // Act
        let actions = settings_actions();

        // Assert
        assert!(!actions.iter().any(|action| action.key == "a"));
    }

    #[test]
    fn test_stats_actions_exclude_new_session_shortcut() {
        // Arrange
        // Act
        let actions = stats_actions();

        // Assert
        assert!(!actions.iter().any(|action| action.key == "a"));
    }

    #[test]
    fn test_session_list_actions_include_new_session_shortcut() {
        // Arrange
        // Act
        let actions = session_list_actions(false, false, false);

        // Assert
        assert!(actions.iter().any(|action| action.key == "a"));
        assert!(actions.iter().any(|action| action.key == "Shift+A"));
    }

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
        assert!(actions.iter().any(|action| action.key == "Shift+A"));
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "c"));
        assert!(!actions.iter().any(|action| action.key == "Tab"));
    }

    #[test]
    fn test_view_actions_in_progress_shows_open_and_hides_edit_actions() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::InProgress,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_rebasing_shows_open_without_stop() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::Rebasing,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_merge_queue_hides_worktree_shortcuts_and_stop() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::MergeQueue,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_review_shows_diff() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "d"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(
            actions
                .iter()
                .any(|action| action.key == "p" && action.footer_label == "publish branch")
        );
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| {
            action.key == "/"
                && action.footer_label == "commands menu"
                && action.popup_label == "Open commands menu"
        }));
        assert!(!actions.iter().any(|action| action.key == "S-Tab"));
        assert!(
            actions
                .iter()
                .any(|action| action.key == "f" && action.popup_label == "Focused review")
        );
    }

    #[test]
    fn test_view_actions_agent_review_hides_rebase() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::AgentReview,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "d"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_actions_interactive_hides_diff() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::Interactive,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "/"));
    }

    #[test]
    fn test_view_actions_new_session_shows_add_draft_and_start() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::NewSession,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(
            actions
                .iter()
                .any(|action| action.key == "Enter" && action.footer_label == "add draft")
        );
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "s"));
    }

    #[test]
    fn test_view_actions_done_shows_toggle_and_hides_edit_actions() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::Done,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "t"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "f"));
        assert!(!actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_review_shows_advanced_actions() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_agent_review_hides_rebase() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::AgentReview,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_rebasing_shows_open_without_stop() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::Rebasing,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
    }

    #[test]
    fn test_view_footer_actions_new_session_keeps_edit_actions_grouped() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::NewSession,
        };

        // Act
        let actions = view_footer_actions(state);
        let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

        // Assert
        assert_eq!(&ordered_keys[..6], ["q", "Enter", "/", "s", "m", "r"]);
    }

    #[test]
    fn test_view_footer_actions_merge_queue_hides_worktree_shortcuts_and_stop() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::MergeQueue,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(actions.iter().any(|action| action.key == "q"));
        assert!(actions.iter().any(|action| action.key == "j/k"));
    }

    #[test]
    fn test_view_actions_canceled_without_branch_publish_action() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: None,
            session_state: ViewSessionState::Canceled,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "p"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "t"));
    }

    #[test]
    fn test_footer_line_styles_keys_labels_and_separator() {
        // Arrange
        let actions = vec![
            HelpAction::new("quit", "q", "Quit"),
            HelpAction::new("help", "?", "Help"),
        ];

        // Act
        let line = footer_line(&actions);

        // Assert
        assert_eq!(line.to_string(), "q: quit | ?: help");
        assert_eq!(
            line.spans[0].style,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(line.spans[1].style, Style::default().fg(Color::Gray));
        assert_eq!(line.spans[2].style, Style::default().fg(Color::Gray));
        assert_eq!(line.spans[3].style, Style::default().fg(Color::DarkGray));
        assert_eq!(
            line.spans[4].style,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn test_footer_muted_span_uses_muted_footer_style() {
        // Arrange
        // Act
        let span = footer_muted_span("note");

        // Assert
        assert_eq!(span.content, "note");
        assert_eq!(span.style, Style::default().fg(Color::Gray));
    }

    #[test]
    fn test_footer_separator_span_uses_shared_separator_style() {
        // Arrange
        // Act
        let span = footer_separator_span();

        // Assert
        assert_eq!(span.content, " | ");
        assert_eq!(span.style, Style::default().fg(Color::DarkGray));
    }

    #[test]
    fn test_view_actions_shows_sync_when_can_sync_review_request_true() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: true,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(
            actions
                .iter()
                .any(|action| action.key == "s"
                    && action.popup_label == "Sync review request status")
        );
    }

    #[test]
    fn test_view_actions_hides_sync_when_can_sync_review_request_false() {
        // Arrange
        let state = ViewHelpState {
            can_sync_review_request: false,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            publish_branch_action: Some(PublishBranchAction::Push),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(
            !actions
                .iter()
                .any(|action| action.popup_label == "Sync review request status")
        );
    }
}
