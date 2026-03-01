use std::collections::BTreeSet;

use super::help_action::{self, HelpAction, ViewHelpState, ViewSessionState};
use super::prompt::{PromptAtMentionState, PromptHistoryState, PromptSlashState};
use crate::domain::input::InputState;
use crate::infra::file_index::FileEntry;

/// Selects the visible panel content for session view output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoneSessionOutputMode {
    /// Renders the concise final summary, when available.
    Summary,
    /// Renders the full captured session output stream.
    Output,
    /// Renders a concise focused-review view with critical diff highlights.
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
        /// Selected content view for the session output panel.
        done_session_output_mode: DoneSessionOutputMode,
        /// Optional status line shown while focused-review text is loading or
        /// unavailable.
        focused_review_status_message: Option<String>,
        /// Agent-assisted focused-review text for the active session.
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
    /// Displays the project file explorer with a preview rooted to either the
    /// active project working directory or a session worktree.
    ProjectExplorer {
        /// Full gitignore-aware file index used to derive visible tree rows.
        all_entries: Vec<FileEntry>,
        /// Indexed file and directory rows shown in the explorer list.
        entries: Vec<FileEntry>,
        /// Expanded directory paths used to project visible tree rows.
        expanded_directories: BTreeSet<String>,
        /// Preview text for the currently selected row.
        preview: String,
        /// Whether closing explorer should return to list mode.
        return_to_list: bool,
        /// Scroll position for the right-side preview panel.
        scroll_offset: u16,
        /// Selected row index in `entries`.
        selected_index: usize,
        /// Session identifier used to restore navigation context and, for
        /// view-origin explorer mode, resolve the session worktree root.
        session_id: String,
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
    /// Help overlay opened from the project explorer page.
    ProjectExplorer {
        /// Full gitignore-aware file index used to derive visible tree rows.
        all_entries: Vec<FileEntry>,
        /// Indexed file and directory rows shown in the explorer list.
        entries: Vec<FileEntry>,
        /// Expanded directory paths used to project visible tree rows.
        expanded_directories: BTreeSet<String>,
        /// Preview text for the currently selected row.
        preview: String,
        /// Whether closing explorer should return to list mode.
        return_to_list: bool,
        /// Scroll position for the right-side preview panel.
        scroll_offset: u16,
        /// Selected row index in `entries`.
        selected_index: usize,
        /// Session identifier used to restore navigation context and, for
        /// view-origin explorer mode, resolve the session worktree root.
        session_id: String,
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
            HelpContext::ProjectExplorer { .. } => help_action::project_explorer_actions(),
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
            HelpContext::ProjectExplorer {
                all_entries,
                entries,
                expanded_directories,
                preview,
                return_to_list,
                scroll_offset,
                selected_index,
                session_id,
            } => AppMode::ProjectExplorer {
                all_entries,
                entries,
                expanded_directories,
                preview,
                return_to_list,
                scroll_offset,
                selected_index,
                session_id,
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
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: Some("Preparing focused review...".to_string()),
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
                && focused_review_status_message == "Preparing focused review..."
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

    #[test]
    fn test_help_context_project_explorer_keybindings_include_navigation() {
        // Arrange
        let context = HelpContext::ProjectExplorer {
            all_entries: vec![],
            entries: vec![],
            expanded_directories: BTreeSet::new(),
            preview: String::new(),
            return_to_list: true,
            scroll_offset: 0,
            selected_index: 0,
            session_id: "session-id".to_string(),
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|binding| binding.key == "q/Esc"));
        assert!(bindings.iter().any(|binding| binding.key == "j/k"));
        assert!(bindings.iter().any(|binding| binding.key == "Up/Down"));
        assert!(bindings.iter().any(|binding| binding.key == "?"));
    }

    #[test]
    fn test_help_context_restore_mode_returns_project_explorer_mode() {
        // Arrange
        let context = HelpContext::ProjectExplorer {
            all_entries: vec![FileEntry {
                is_dir: true,
                path: "src".to_string(),
            }],
            entries: vec![FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            }],
            expanded_directories: BTreeSet::from(["src".to_string()]),
            preview: "fn main() {}".to_string(),
            return_to_list: false,
            scroll_offset: 3,
            selected_index: 0,
            session_id: "session-id".to_string(),
        };

        // Act
        let mode = context.restore_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::ProjectExplorer {
                ref session_id,
                return_to_list: false,
                scroll_offset: 3,
                selected_index: 0,
                ..
            } if session_id == "session-id"
        ));
    }
}
