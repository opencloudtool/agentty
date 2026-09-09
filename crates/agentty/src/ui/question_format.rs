//! Question-mode text and footer formatting.

use ag_tui_text::text_util;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::presentation::app_mode::ChatFocus;
use crate::presentation::help_action;
use crate::ui::style;

/// Availability of file matches in the current question-answer lookup.
#[derive(Clone, Copy)]
pub enum QuestionLookupState {
    /// The terminal has no room for a visible suggestion row.
    Clipped,
    /// No lookup is active at the input cursor.
    Closed,
    /// A lookup is active but has no results yet, or no matching files.
    Empty,
    /// The lookup has a nonempty suggestion list.
    Matches,
}

/// Returns wrapped question-panel lines with the correct focus styling.
pub(crate) fn question_panel_lines(
    question_title: &str,
    question: &str,
    is_chat_focused: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let title_color = if is_chat_focused {
        style::palette::text_muted()
    } else {
        style::palette::question()
    };
    let text_color = if is_chat_focused {
        style::palette::text_muted()
    } else {
        style::palette::warning()
    };
    let mut lines = vec![Line::from(Span::styled(
        question_title.to_string(),
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    ))];
    lines.extend(
        text_util::wrap_lines(question, usize::from(width.max(1)))
            .into_iter()
            .map(|line| Line::from(line.to_string()).style(Style::default().fg(text_color))),
    );

    lines
}

/// Returns wrapped and styled option rows for the question panel.
pub(crate) fn question_option_lines(
    options: &[String],
    selected_option_index: Option<usize>,
    dimmed: bool,
) -> Vec<Line<'static>> {
    let header_color = if dimmed {
        style::palette::text_muted()
    } else {
        style::palette::warning()
    };
    let mut lines = Vec::with_capacity(options.len() + 1);
    lines.push(Line::from(Span::styled(
        "Options:",
        Style::default().fg(header_color),
    )));

    for (option_index, option_text) in options.iter().enumerate() {
        let is_selected = selected_option_index == Some(option_index);
        let prefix = if is_selected { "▸ " } else { "  " };
        let label = format!("{prefix}{}. {option_text}", option_index + 1);
        let style = if dimmed {
            Style::default().fg(style::palette::text_muted())
        } else if is_selected {
            Style::default()
                .fg(style::palette::surface_overlay())
                .bg(style::palette::warning())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(style::palette::text())
        };

        lines.push(Line::from(Span::styled(label, style)));
    }

    lines
}

/// Builds the question-mode help footer line for the current focus target.
///
/// `has_session_diff` controls whether chat focus advertises the diff preview;
/// the shortcut is hidden only for a known-empty diff.
///
/// `is_navigating_options` mirrors the runtime predicate that treats plain `q`
/// as a navigation key while the user is moving through predefined options. The
/// `q: Sessions` hint is surfaced whenever that predicate is satisfied so the
/// shortcut stays discoverable in answer focus too, not only in chat focus.
/// `lookup_state` distinguishes selectable matches from an active lookup that
/// can only be dismissed while entries are loading or no files match.
///
/// Footer entries follow the canonical composer-footer ordering shared with
/// prompt mode: the `Tab` focus toggle first as the stable anchor, then the
/// primary `Enter` action, reading extras, and exit actions last. An active
/// file lookup replaces focus and send actions with selection controls.
pub fn question_help_footer_line(
    focus: ChatFocus,
    has_session_diff: bool,
    is_navigating_options: bool,
    lookup_state: QuestionLookupState,
) -> Line<'static> {
    if focus == ChatFocus::Input
        && !is_navigating_options
        && !matches!(lookup_state, QuestionLookupState::Closed)
    {
        let mut help_actions = if matches!(lookup_state, QuestionLookupState::Matches) {
            vec![
                help_action::HelpAction::new("select", "Tab/Enter", "Select file"),
                help_action::HelpAction::new("navigate", "Up/Down", "Navigate files"),
            ]
        } else if matches!(lookup_state, QuestionLookupState::Empty) {
            vec![help_action::HelpAction::new(
                "close @",
                "Tab/Enter",
                "Close lookup",
            )]
        } else {
            Vec::new()
        };
        help_actions.extend([
            help_action::HelpAction::new("cancel @", "Esc", "Cancel @"),
            help_action::HelpAction::new("end turn", "Ctrl+C", "End turn"),
        ]);

        return crate::ui::help_format::footer_line(&help_actions);
    }

    let is_chat_focused = focus == ChatFocus::Chat;
    let focus_label = if is_chat_focused { "Answer" } else { "Chat" };
    let mut help_actions = vec![help_action::HelpAction::new("focus", "Tab", focus_label)];

    if is_chat_focused {
        help_actions.push(help_action::HelpAction::new("scroll", "j/k", "Scroll chat"));
        if has_session_diff {
            help_actions.push(help_action::HelpAction::new("diff", "d", "Diff"));
        }
    } else {
        help_actions.push(help_action::HelpAction::new("send", "Enter", "Send answer"));
    }

    if is_chat_focused || is_navigating_options {
        help_actions.push(help_action::HelpAction::new("sessions", "q", "Sessions"));
    }

    if !is_chat_focused {
        help_actions.push(help_action::HelpAction::new(
            "end turn", "Ctrl+C", "End turn",
        ));
    }

    crate::ui::help_format::footer_line(&help_actions)
}
