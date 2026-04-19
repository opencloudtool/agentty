//! Shared helpers and message sets for page-scoped status-bar FYIs.

use crate::app::Tab;
use crate::ui::state::app_mode::{AppMode, HelpContext};

/// Rotating FYI messages shown in the top status bar while the sessions list
/// is visible.
const SESSION_LIST_FYI_MESSAGES: [&str; 2] = [
    "Press Enter to open the selected session.",
    "Agentty refreshes PR statuses every minute.",
];

/// Rotating FYI messages shown in the top status bar while session chat is
/// visible.
///
/// These preserve the existing help and slash-command reminders while also
/// calling out the automatic focused-review pass that begins after each turn
/// and the `f` shortcut for opening or regenerating the same review on
/// demand.
const SESSION_CHAT_FYI_MESSAGES: [&str; 4] = [
    "Press ? to inspect the shortcuts available for the current session state.",
    "Press / to open slash commands without typing into the composer first.",
    "Agentty starts focused review automatically after each turn.",
    "Press f to open or regenerate focused review output on demand.",
];

/// Returns the full rotating sessions-list FYI set used by the top status bar.
pub(crate) fn session_list_messages() -> &'static [&'static str] {
    &SESSION_LIST_FYI_MESSAGES
}

/// Returns the full rotating session-chat FYI set used by the top status bar.
pub(crate) fn session_chat_messages() -> &'static [&'static str] {
    &SESSION_CHAT_FYI_MESSAGES
}

/// Returns the page-scoped FYI set that should be visible in the status bar
/// for the active page, if any.
pub(crate) fn current_page_messages(
    current_tab: Tab,
    mode: &AppMode,
) -> Option<&'static [&'static str]> {
    match mode {
        AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Question { .. }
        | AppMode::ViewInfoPopup { .. }
        | AppMode::OpenCommandSelector { .. }
        | AppMode::PublishBranchInput { .. }
        | AppMode::Confirmation {
            restore_view: Some(_),
            ..
        }
        | AppMode::Help {
            context: HelpContext::View { .. },
            ..
        } => Some(session_chat_messages()),
        AppMode::List
        | AppMode::Confirmation {
            restore_view: None, ..
        }
        | AppMode::Help {
            context: HelpContext::List { .. },
            ..
        } if current_tab == Tab::Sessions => Some(session_list_messages()),
        _ => None,
    }
}

/// Returns the FYI message visible for the provided absolute rotation slot.
pub(crate) fn rotating_message<'a>(
    fyi_messages: &'a [&'a str],
    rotation_index: u64,
) -> Option<&'a str> {
    if fyi_messages.is_empty() {
        return None;
    }

    let message_count = u64::try_from(fyi_messages.len()).unwrap_or(u64::MAX);
    let message_index = rotation_index % message_count;
    let message_index = usize::try_from(message_index).unwrap_or_default();

    fyi_messages.get(message_index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::app_mode::DoneSessionOutputMode;

    #[test]
    fn rotating_message_cycles_through_messages() {
        // Arrange
        let fyi_messages = ["First", "Second", "Third"];

        // Act
        let zero = rotating_message(&fyi_messages, 0);
        let one = rotating_message(&fyi_messages, 1);
        let wrapped = rotating_message(&fyi_messages, 4);

        // Assert
        assert_eq!(zero, Some("First"));
        assert_eq!(one, Some("Second"));
        assert_eq!(wrapped, Some("Second"));
    }

    #[test]
    fn rotating_message_returns_none_for_empty_set() {
        // Arrange
        let fyi_messages: [&str; 0] = [];

        // Act
        let selected_message = rotating_message(&fyi_messages, 2);

        // Assert
        assert_eq!(selected_message, None);
    }

    #[test]
    fn current_page_messages_returns_session_list_guidance_for_sessions_tab() {
        // Arrange
        let mode = AppMode::List;

        // Act
        let page_fyis = current_page_messages(Tab::Sessions, &mode);

        // Assert
        assert_eq!(page_fyis, Some(session_list_messages()));
    }

    #[test]
    fn current_page_messages_returns_session_chat_guidance_for_view_mode() {
        // Arrange
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: "session-id".to_string(),
            scroll_offset: None,
        };

        // Act
        let page_fyis = current_page_messages(Tab::Sessions, &mode);

        // Assert
        assert_eq!(page_fyis, Some(session_chat_messages()));
    }

    #[test]
    fn current_page_messages_skips_non_session_pages_and_diff_mode() {
        // Arrange
        let list_mode = AppMode::List;
        let diff_mode = AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
            session_id: "session-id".to_string(),
            scroll_offset: 0,
        };

        // Act
        let settings_page_fyis = current_page_messages(Tab::Settings, &list_mode);
        let diff_page_fyis = current_page_messages(Tab::Sessions, &diff_mode);

        // Assert
        assert_eq!(settings_page_fyis, None);
        assert_eq!(diff_page_fyis, None);
    }
}
