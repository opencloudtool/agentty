use ratatui::layout::Rect;

use super::help_action::{self, HelpAction, ViewHelpState, ViewSessionState};
use super::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashState,
};
use crate::domain::input::InputState;
use crate::domain::session::{FollowUpTaskAction, PublishBranchAction};
use crate::infra::agent::protocol::QuestionItem;

/// Selects the visible panel content for session view output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoneSessionOutputMode {
    /// Renders the concise final summary, when available.
    Summary,
    /// Renders the full captured session output stream.
    Output,
    /// Renders a concise review view with critical diff highlights.
    Review,
}

impl DoneSessionOutputMode {
    /// Returns the opposite done-session output mode.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Summary => Self::Output,
            Self::Output | Self::Review => Self::Summary,
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
    /// Confirms canceling a selected review session.
    CancelSession,
    /// Confirms queueing merge for the active view session.
    MergeSession,
    /// Confirms regenerating the focused review for the active view session.
    RegenerateReview,
}

/// Stored view-mode values used to restore session view after merge
/// confirmation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationViewMode {
    pub done_session_output_mode: DoneSessionOutputMode,
    pub review_status_message: Option<String>,
    pub review_text: Option<String>,
    pub scroll_offset: Option<u16>,
    pub session_id: String,
}

impl ConfirmationViewMode {
    /// Restores this snapshot as `AppMode::View`.
    #[must_use]
    pub fn into_view_mode(self) -> AppMode {
        AppMode::View {
            done_session_output_mode: self.done_session_output_mode,
            review_status_message: self.review_status_message,
            review_text: self.review_text,
            session_id: self.session_id,
            scroll_offset: self.scroll_offset,
        }
    }
}

/// Cached scroll bounds for the current diff selection and content area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiffScrollCache {
    pub content_area: Rect,
    pub file_explorer_selected_index: usize,
    pub max_scroll_offset: u16,
}

/// Captured question-mode state for restoring after diff preview.
///
/// When the user opens diff preview from question mode (`d` key while chat
/// is focused), the full question state is snapshotted here so it can be
/// restored when leaving the diff view.
pub struct QuestionModeSnapshot {
    pub at_mention_state: Option<PromptAtMentionState>,
    pub current_index: usize,
    pub input: InputState,
    pub questions: Vec<QuestionItem>,
    pub responses: Vec<String>,
    pub scroll_offset: Option<u16>,
    pub selected_option_index: Option<usize>,
    pub session_id: String,
}

impl QuestionModeSnapshot {
    /// Restores this snapshot as `AppMode::Question` with `Answer` focus.
    #[must_use]
    pub fn into_question_mode(self) -> AppMode {
        AppMode::Question {
            at_mention_state: self.at_mention_state,
            current_index: self.current_index,
            focus: QuestionFocus::Answer,
            input: self.input,
            questions: self.questions,
            responses: self.responses,
            scroll_offset: self.scroll_offset,
            selected_option_index: self.selected_option_index,
            session_id: self.session_id,
        }
    }
}

/// Tracks which panel has input focus during question-answer mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QuestionFocus {
    /// Question panel is focused for option navigation or free-text input.
    #[default]
    Answer,
    /// Chat output area is focused for scrolling.
    Chat,
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
    /// Informational popup rendered above session view for review-request
    /// workflows.
    ViewInfoPopup {
        /// Whether the background review-request workflow is still running.
        is_loading: bool,
        /// Spinner label rendered while the popup remains in the loading
        /// state.
        loading_label: String,
        /// Body text describing the current review-request outcome.
        message: String,
        /// View state restored after the popup is dismissed.
        restore_view: ConfirmationViewMode,
        /// Popup title describing the current review-request phase.
        title: String,
    },
    /// Command selector overlay opened from session view when multiple open
    /// commands are configured.
    OpenCommandSelector {
        /// Available open commands in display/selection order.
        commands: Vec<String>,
        /// View state restored after command selection or cancel.
        restore_view: ConfirmationViewMode,
        /// Highlighted command index in `commands`.
        selected_command_index: usize,
    },
    /// Session-view popup that collects an optional remote branch name before
    /// publishing the current session branch.
    PublishBranchInput {
        /// Default remote branch name used when users leave the field blank.
        default_branch_name: String,
        /// Editable remote branch name. An empty value keeps the default push
        /// target for the session branch.
        input: InputState,
        /// Existing upstream reference, when the session branch already tracks
        /// one remote branch and the input must stay locked.
        locked_upstream_ref: Option<String>,
        /// Publish action that will run when users confirm the popup.
        publish_branch_action: PublishBranchAction,
        /// View state restored after publish or cancel.
        restore_view: ConfirmationViewMode,
    },
    /// Session chat composer for the first prompt or a follow-up reply.
    Prompt {
        /// Active `@`-mention dropdown state for file and directory lookup.
        at_mention_state: Option<PromptAtMentionState>,
        /// Ordered local image attachments referenced by inline placeholders in
        /// `input`.
        attachment_state: PromptAttachmentState,
        /// Prompt-history navigation state for `Up`/`Down`.
        history_state: PromptHistoryState,
        /// Focused-review status text preserved while the composer is open so
        /// canceling the prompt restores the same session output view.
        review_status_message: Option<String>,
        /// Focused-review output preserved while the composer is open so the
        /// session transcript remains stable until a new prompt is submitted.
        review_text: Option<String>,
        /// Slash-command selection state for the current prompt input.
        slash_state: PromptSlashState,
        /// Session whose prompt composer is currently active.
        session_id: String,
        /// Editable prompt text, including inline attachment placeholders.
        input: InputState,
        /// Scroll position applied to the session transcript above the
        /// composer.
        scroll_offset: Option<u16>,
    },
    View {
        /// Selected content view for the session output panel.
        done_session_output_mode: DoneSessionOutputMode,
        /// Optional status line shown while review text is loading or
        /// unavailable.
        review_status_message: Option<String>,
        /// Agent-assisted review text for the active session.
        review_text: Option<String>,
        session_id: String,
        scroll_offset: Option<u16>,
    },
    /// Focused diff view with file-tree navigation and independent scrolling.
    Diff {
        /// Raw git diff rendered in the right-hand panel.
        diff: String,
        /// Selected file or folder in the left explorer tree.
        file_explorer_selected_index: usize,
        /// Cached max scroll bound for the current content-area and selection.
        scroll_cache: Option<DiffScrollCache>,
        /// Captured question state restored when leaving diff, if the diff was
        /// opened from question mode. `None` restores to `View` mode.
        restore_question: Option<QuestionModeSnapshot>,
        /// Session whose diff is currently visible.
        session_id: String,
        /// Vertical offset inside the rendered diff panel.
        scroll_offset: u16,
    },

    /// Interactive clarification flow that asks agent questions one-by-one.
    Question {
        /// File/directory mention dropdown state for the free-text input.
        at_mention_state: Option<PromptAtMentionState>,
        /// Session receiving the follow-up clarification reply.
        session_id: String,
        /// Ordered clarification prompts emitted by the model.
        questions: Vec<QuestionItem>,
        /// Collected user responses aligned to `questions`.
        responses: Vec<String>,
        /// Active question index inside `questions`.
        current_index: usize,
        /// Which panel currently owns keyboard focus.
        focus: QuestionFocus,
        /// Editable response input for the active question.
        input: InputState,
        /// Scroll position applied to the session transcript above the
        /// question panel.
        scroll_offset: Option<u16>,
        /// Highlighted option index when the current question has predefined
        /// options. `None` means free-text input is active.
        selected_option_index: Option<usize>,
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
        can_sync_review_request: bool,
        done_session_output_mode: DoneSessionOutputMode,
        follow_up_task_action: Option<FollowUpTaskAction>,
        has_multiple_follow_up_tasks: bool,
        review_status_message: Option<String>,
        review_text: Option<String>,
        publish_branch_action: Option<PublishBranchAction>,
        publish_pull_request_action: Option<PublishBranchAction>,
        session_id: String,
        session_state: ViewSessionState,
        scroll_offset: Option<u16>,
    },
    Diff {
        diff: String,
        file_explorer_selected_index: usize,
        /// Preserved question-mode snapshot so the help→diff→exit path can
        /// still return to question mode when the diff was opened from there.
        restore_question: Option<QuestionModeSnapshot>,
        session_id: String,
        scroll_offset: u16,
    },
}

impl HelpContext {
    /// Returns projected keybinding entries for the originating page.
    pub fn keybindings(&self) -> Vec<HelpAction> {
        match self {
            HelpContext::View {
                can_sync_review_request,
                follow_up_task_action,
                has_multiple_follow_up_tasks,
                publish_branch_action,
                publish_pull_request_action,
                session_state,
                ..
            } => help_action::view_actions(ViewHelpState {
                can_sync_review_request: *can_sync_review_request,
                follow_up_task_action: *follow_up_task_action,
                has_multiple_follow_up_tasks: *has_multiple_follow_up_tasks,
                publish_branch_action: *publish_branch_action,
                publish_pull_request_action: *publish_pull_request_action,
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
                follow_up_task_action: _,
                has_multiple_follow_up_tasks: _,
                review_status_message,
                review_text,
                publish_branch_action: _,
                publish_pull_request_action: _,
                session_id,
                scroll_offset,
                ..
            } => AppMode::View {
                done_session_output_mode,
                review_status_message,
                review_text,
                session_id,
                scroll_offset,
            },
            HelpContext::Diff {
                diff,
                file_explorer_selected_index,
                restore_question,
                session_id,
                scroll_offset,
            } => AppMode::Diff {
                diff,
                file_explorer_selected_index,
                restore_question,
                scroll_cache: None,
                session_id,
                scroll_offset,
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
    use crate::app::review_loading_message;
    use crate::domain::agent::AgentModel;
    use crate::domain::session::PublishBranchAction;

    #[test]
    fn test_done_session_output_mode_toggle_switches_between_variants() {
        // Arrange
        let summary_mode = DoneSessionOutputMode::Summary;
        let output_mode = DoneSessionOutputMode::Output;
        let review_mode = DoneSessionOutputMode::Review;

        // Act
        let toggled_from_summary = summary_mode.toggled();
        let toggled_from_output = output_mode.toggled();
        let toggled_from_review = review_mode.toggled();

        // Assert
        assert_eq!(toggled_from_summary, DoneSessionOutputMode::Output);
        assert_eq!(toggled_from_output, DoneSessionOutputMode::Summary);
        assert_eq!(toggled_from_review, DoneSessionOutputMode::Summary);
    }

    #[test]
    fn test_confirmation_view_mode_into_view_mode_restores_snapshot_values() {
        // Arrange
        let confirmation_view_mode = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Review,
            review_status_message: Some(review_loading_message(AgentModel::Gpt54)),
            review_text: Some("Critical finding".to_string()),
            scroll_offset: Some(7),
            session_id: "session-id".to_string(),
        };

        // Act
        let mode = confirmation_view_mode.into_view_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::Review,
                review_status_message: Some(ref review_status_message),
                review_text: Some(ref review_text),
                ref session_id,
                scroll_offset: Some(7),
            } if session_id == "session-id"
                && review_status_message == &review_loading_message(AgentModel::Gpt54)
                && review_text == "Critical finding"
        ));
    }

    #[test]
    fn test_help_context_view_keybindings_for_in_progress_hide_edit_actions() {
        // Arrange
        let context = HelpContext::View {
            can_sync_review_request: false,
            done_session_output_mode: DoneSessionOutputMode::Summary,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            review_status_message: None,
            review_text: None,
            publish_branch_action: None,
            publish_pull_request_action: None,
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
            can_sync_review_request: false,
            done_session_output_mode: DoneSessionOutputMode::Summary,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            review_status_message: Some("Preparing review...".to_string()),
            review_text: Some("Ready".to_string()),
            publish_branch_action: Some(PublishBranchAction::Push),
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
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
                review_status_message: Some(ref review_status_message),
                review_text: Some(ref review_text),
                scroll_offset: Some(4),
                ..
            } if session_id == "session-id"
                && review_status_message == "Preparing review..."
                && review_text == "Ready"
        ));
    }

    #[test]
    fn test_help_context_view_keybindings_include_publish_branch_action() {
        // Arrange
        let context = HelpContext::View {
            can_sync_review_request: false,
            done_session_output_mode: DoneSessionOutputMode::Summary,
            follow_up_task_action: None,
            has_multiple_follow_up_tasks: false,
            review_status_message: None,
            review_text: None,
            publish_branch_action: Some(PublishBranchAction::Push),
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_id: "session-id".to_string(),
            session_state: ViewSessionState::Interactive,
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|binding| binding.key == "p"));
        assert!(bindings.iter().any(|binding| binding.key == "Shift+P"));
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
