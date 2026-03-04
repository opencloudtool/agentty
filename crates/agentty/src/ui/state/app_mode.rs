use super::help_action::{self, HelpAction, ViewHelpState, ViewSessionState};
use super::prompt::{PromptAtMentionState, PromptHistoryState, PromptSlashState};
use crate::domain::input::InputState;

/// Selects the visible panel content for session view output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoneSessionOutputMode {
    /// Renders the concise final summary, when available.
    Summary,
    /// Renders the full captured session output stream.
    Output,
    /// Renders a concise review view with critical diff highlights.
    FocusedReview,
}

impl DoneSessionOutputMode {
    /// Returns the opposite done-session output mode.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Summary => Self::Output,
            Self::Output | Self::FocusedReview => Self::Summary,
        }
    }
}

/// Semantic intent for a `Confirmation` overlay interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationIntent {
    /// Confirms quitting the application.
    Quit,
    /// Confirms deleting a selected session.
    DeleteSession,
    /// Confirms queueing merge for the active view session.
    MergeSession,
}

/// Stored view-mode values used to restore session view after merge
/// confirmation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationViewMode {
    pub done_session_output_mode: DoneSessionOutputMode,
    pub focused_review_status_message: Option<String>,
    pub focused_review_text: Option<String>,
    pub scroll_offset: Option<u16>,
    pub session_id: String,
}

impl ConfirmationViewMode {
    /// Restores this snapshot as `AppMode::View`.
    #[must_use]
    pub fn into_view_mode(self) -> AppMode {
        AppMode::View {
            done_session_output_mode: self.done_session_output_mode,
            focused_review_status_message: self.focused_review_status_message,
            focused_review_text: self.focused_review_text,
            session_id: self.session_id,
            scroll_offset: self.scroll_offset,
        }
    }
}

/// Represents the active UI mode for the application.
pub enum AppMode {
    List,
    /// Displays a generic confirmation overlay with `Yes` and `No` options.
    Confirmation {
        /// Semantic action to execute when users choose `Yes`.
        confirmation_intent: ConfirmationIntent,
        confirmation_message: String,
        confirmation_title: String,
        /// View state to restore when dismissing merge confirmation.
        restore_view: Option<ConfirmationViewMode>,
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
        /// Selected content view for the session output panel.
        done_session_output_mode: DoneSessionOutputMode,
        /// Optional status line shown while review text is loading or
        /// unavailable.
        focused_review_status_message: Option<String>,
        /// Agent-assisted review text for the active session.
        focused_review_text: Option<String>,
        session_id: String,
        scroll_offset: Option<u16>,
    },
    Diff {
        session_id: String,
        diff: String,
        scroll_offset: u16,
        file_explorer_selected_index: usize,
    },

    /// Interactive clarification flow that asks agent questions one-by-one.
    Question {
        /// Session receiving the follow-up clarification reply.
        session_id: String,
        /// Ordered clarification prompts emitted by the model.
        questions: Vec<String>,
        /// Collected user responses aligned to `questions`.
        responses: Vec<String>,
        /// Active question index inside `questions`.
        current_index: usize,
        /// Editable response input for the active question.
        input: InputState,
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
        done_session_output_mode: DoneSessionOutputMode,
        focused_review_status_message: Option<String>,
        focused_review_text: Option<String>,
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
            HelpContext::View { session_state, .. } => help_action::view_actions(ViewHelpState {
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
                done_session_output_mode,
                focused_review_status_message,
                focused_review_text,
                session_id,
                scroll_offset,
                ..
            } => AppMode::View {
                done_session_output_mode,
                focused_review_status_message,
                focused_review_text,
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
    fn test_done_session_output_mode_toggle_switches_between_variants() {
        // Arrange
        let summary_mode = DoneSessionOutputMode::Summary;
        let output_mode = DoneSessionOutputMode::Output;
        let focused_review_mode = DoneSessionOutputMode::FocusedReview;

        // Act
        let toggled_from_summary = summary_mode.toggled();
        let toggled_from_output = output_mode.toggled();
        let toggled_from_focused_review = focused_review_mode.toggled();

        // Assert
        assert_eq!(toggled_from_summary, DoneSessionOutputMode::Output);
        assert_eq!(toggled_from_output, DoneSessionOutputMode::Summary);
        assert_eq!(toggled_from_focused_review, DoneSessionOutputMode::Summary);
    }

    #[test]
    fn test_confirmation_view_mode_into_view_mode_restores_snapshot_values() {
        // Arrange
        let confirmation_view_mode = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::FocusedReview,
            focused_review_status_message: Some("Preparing focused review".to_string()),
            focused_review_text: Some("Critical finding".to_string()),
            scroll_offset: Some(7),
            session_id: "session-id".to_string(),
        };

        // Act
        let mode = confirmation_view_mode.into_view_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::FocusedReview,
                focused_review_status_message: Some(ref focused_review_status_message),
                focused_review_text: Some(ref focused_review_text),
                ref session_id,
                scroll_offset: Some(7),
            } if session_id == "session-id"
                && focused_review_status_message == "Preparing focused review"
                && focused_review_text == "Critical finding"
        ));
    }

    #[test]
    fn test_help_context_view_keybindings_for_in_progress_hide_edit_actions() {
        // Arrange
        let context = HelpContext::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
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
        assert!(!bindings.iter().any(|binding| binding.key == "Ctrl+c"));
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
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: Some("Preparing review...".to_string()),
            focused_review_text: Some("Ready".to_string()),
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
                focused_review_status_message: Some(ref focused_review_status_message),
                focused_review_text: Some(ref focused_review_text),
                scroll_offset: Some(4),
                ..
            } if session_id == "session-id"
                && focused_review_status_message == "Preparing review..."
                && focused_review_text == "Ready"
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
