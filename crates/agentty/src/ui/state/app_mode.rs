use super::help_action::{
    self, HelpAction, PlanFollowupNavigation, ViewHelpState, ViewSessionState,
};
use super::palette::{PaletteCommand, PaletteFocus};
use super::prompt::{PromptAtMentionState, PromptHistoryState, PromptSlashState};
use crate::domain::input::InputState;

/// Represents the active UI mode for the application.
pub enum AppMode {
    List,
    /// Displays a generic confirmation overlay with `Yes` and `No` options.
    Confirmation {
        confirmation_message: String,
        confirmation_title: String,
        session_id: Option<String>,
        selected_confirmation_index: usize,
    },
    /// Informational popup displayed for sync outcomes, including success and
    /// blocked/failed states.
    SyncBlockedPopup {
        /// Selected project name for which sync was requested.
        project_name: Option<String>,
        /// Repository default branch used as sync target.
        default_branch: Option<String>,
        /// Whether sync is still running in the background.
        is_loading: bool,
        /// Body text describing current sync state or final outcome.
        message: String,
        /// Popup title describing sync state.
        title: String,
    },
    Prompt {
        at_mention_state: Option<PromptAtMentionState>,
        history_state: PromptHistoryState,
        slash_state: PromptSlashState,
        session_id: String,
        input: InputState,
        scroll_offset: Option<u16>,
    },
    View {
        session_id: String,
        scroll_offset: Option<u16>,
    },
    Diff {
        session_id: String,
        diff: String,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
    },
    CommandPalette {
        input: String,
        selected_index: usize,
        focus: PaletteFocus,
    },
    CommandOption {
        command: PaletteCommand,
        selected_index: usize,
    },
    Help {
        context: HelpContext,
        scroll_offset: u16,
    },
}

/// Captures which page opened the help overlay so it can be restored on close.
pub enum HelpContext {
    /// Generic list-mode help context with precomputed keybindings.
    List { keybindings: Vec<HelpAction> },
    View {
        plan_followup_navigation: Option<PlanFollowupNavigation>,
        session_id: String,
        session_state: ViewSessionState,
        scroll_offset: Option<u16>,
    },
    Diff {
        session_id: String,
        diff: String,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
    },
}

impl HelpContext {
    /// Returns projected keybinding entries for the originating page.
    pub fn keybindings(&self) -> Vec<HelpAction> {
        match self {
            HelpContext::View {
                plan_followup_navigation,
                session_state,
                ..
            } => help_action::view_actions(ViewHelpState {
                plan_followup_navigation: *plan_followup_navigation,
                session_state: *session_state,
            }),
            HelpContext::List { keybindings } => keybindings.clone(),
            HelpContext::Diff { .. } => help_action::diff_actions(),
        }
    }

    /// Reconstructs the `AppMode` that was active before help was opened.
    pub fn restore_mode(self) -> AppMode {
        match self {
            HelpContext::List { .. } => AppMode::List,
            HelpContext::View {
                session_id,
                scroll_offset,
                ..
            } => AppMode::View {
                session_id,
                scroll_offset,
            },
            HelpContext::Diff {
                session_id,
                diff,
                scroll_offset,
                file_explorer_selected_index,
            } => AppMode::Diff {
                session_id,
                diff,
                scroll_offset,
                file_explorer_selected_index,
            },
        }
    }

    /// Display title for the help overlay header.
    pub fn title(&self) -> &'static str {
        "Keybindings"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help_context_view_keybindings_for_in_progress_hide_edit_actions() {
        // Arrange
        let context = HelpContext::View {
            plan_followup_navigation: None,
            session_id: "session-id".to_string(),
            session_state: ViewSessionState::InProgress,
            scroll_offset: Some(2),
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|binding| binding.key == "q"));
        assert!(bindings.iter().any(|binding| binding.key == "j/k"));
        assert!(bindings.iter().any(|binding| binding.key == "?"));
        assert!(bindings.iter().any(|binding| binding.key == "Ctrl+c"));
        assert!(!bindings.iter().any(|binding| binding.key == "Enter"));
        assert!(!bindings.iter().any(|binding| binding.key == "d"));
        assert!(!bindings.iter().any(|binding| binding.key == "m"));
        assert!(!bindings.iter().any(|binding| binding.key == "r"));
        assert!(!bindings.iter().any(|binding| binding.key == "S-Tab"));
    }

    #[test]
    fn test_help_context_restore_mode_ignores_view_help_flags() {
        // Arrange
        let context = HelpContext::View {
            plan_followup_navigation: Some(PlanFollowupNavigation::Vertical),
            session_id: "session-id".to_string(),
            session_state: ViewSessionState::InProgress,
            scroll_offset: Some(4),
        };

        // Act
        let mode = context.restore_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(4),
            } if session_id == "session-id"
        ));
    }

    #[test]
    fn test_help_context_list_keybindings_return_stored_actions() {
        // Arrange
        let keybindings = vec![
            HelpAction::new("quit", "q", "Quit"),
            HelpAction::new("help", "?", "Help"),
        ];
        let context = HelpContext::List { keybindings };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().any(|binding| binding.key == "q"));
        assert!(bindings.iter().any(|binding| binding.key == "?"));
    }
}
