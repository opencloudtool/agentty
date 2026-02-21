use crate::domain::input::InputState;
use super::palette::{PaletteCommand, PaletteFocus};
use super::prompt::{PromptAtMentionState, PromptHistoryState, PromptSlashState};

pub enum AppMode {
    List,
    ConfirmDeleteSession {
        selected_confirmation_index: usize,
        session_id: String,
        session_title: String,
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
    List,
    View {
        is_done: bool,
        session_id: String,
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
    /// Returns the keybinding pairs `(key, description)` for the originating
    /// page.
    pub fn keybindings(&self) -> &[(&str, &str)] {
        match self {
            HelpContext::List => &[
                ("q", "Quit"),
                ("a", "Add session"),
                ("d", "Delete session"),
                ("c", "Cancel session"),
                ("o", "Open"),
                ("Enter", "Open session"),
                ("Tab", "Switch tab"),
                ("/", "Command palette"),
                ("?", "Help"),
            ],
            HelpContext::View { is_done: true, .. } => {
                &[("q", "Quit"), ("j", "Scroll down"), ("k", "Scroll up")]
            }
            HelpContext::View { .. } => &[
                ("q", "Back to list"),
                ("Enter", "Reply"),
                ("Ctrl+c", "Stop agent"),
                ("d", "Show diff"),
                ("m", "Merge"),
                ("r", "Rebase"),
                ("S-Tab", "Toggle permission mode"),
                ("g", "Scroll to top"),
                ("G", "Scroll to bottom"),
                ("Ctrl+d", "Half page down"),
                ("Ctrl+u", "Half page up"),
                ("?", "Help"),
            ],
            HelpContext::Diff { .. } => &[
                ("q / Esc", "Back to session"),
                ("j / k", "Select file"),
                ("Up / Down", "Scroll selected file"),
                ("?", "Help"),
            ],
        }
    }

    /// Reconstructs the `AppMode` that was active before help was opened.
    pub fn restore_mode(self) -> AppMode {
        match self {
            HelpContext::List => AppMode::List,
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
