use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    InProgress,
    Done,
}

pub enum AppMode {
    List,
    Prompt { input: String },
}

pub struct Agent {
    pub name: String,
    pub prompt: String,
    pub status: Status,
}

impl Status {
    pub fn icon(self) -> &'static str {
        match self {
            Status::InProgress => "⏳",
            Status::Done => "✅",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Status::InProgress => Color::Yellow,
            Status::Done => Color::Green,
        }
    }

    pub fn toggle(&mut self) {
        *self = match self {
            Status::InProgress => Status::Done,
            Status::Done => Status::InProgress,
        };
    }
}
