use ratatui::style::Color;
use crate::domain::session::Status;
use super::icon::Icon;

pub fn status_color(status: Status) -> Color {
    match status {
        Status::New => Color::DarkGray,
        Status::InProgress => Color::Yellow,
        Status::Review => Color::LightBlue,
        Status::Rebasing | Status::Merging => Color::Cyan,
        Status::Done => Color::Green,
        Status::Canceled => Color::Red,
    }
}

pub fn status_icon(status: Status) -> Icon {
    match status {
        Status::New | Status::Review => Icon::Pending,
        Status::InProgress | Status::Rebasing | Status::Merging => Icon::current_spinner(),
        Status::Done => Icon::Check,
        Status::Canceled => Icon::Cross,
    }
}
