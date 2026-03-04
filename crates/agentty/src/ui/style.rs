use ratatui::style::Color;

use super::icon::Icon;
use crate::domain::session::Status;

/// Returns the terminal color used for one session status label.
pub fn status_color(status: Status) -> Color {
    match status {
        Status::New => Color::DarkGray,
        Status::InProgress | Status::Question => Color::Yellow,
        Status::Review => Color::LightBlue,
        Status::Queued => Color::LightCyan,
        Status::Rebasing | Status::Merging => Color::Cyan,
        Status::Done => Color::Green,
        Status::Canceled => Color::Red,
    }
}

/// Returns the icon used for one session status indicator.
pub fn status_icon(status: Status) -> Icon {
    match status {
        Status::New | Status::Review | Status::Question | Status::Queued => Icon::Pending,
        Status::InProgress | Status::Rebasing | Status::Merging => Icon::current_spinner(),
        Status::Done => Icon::Check,
        Status::Canceled => Icon::Cross,
    }
}
