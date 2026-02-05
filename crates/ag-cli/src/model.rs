use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Sessions,
    Roadmap,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    InProgress,
    Done,
}

pub enum AppMode {
    List,
    Prompt {
        input: String,
    },
    View {
        session_index: usize,
        scroll_offset: Option<u16>,
    },
    Reply {
        session_index: usize,
        input: String,
        scroll_offset: Option<u16>,
    },
}

pub struct Session {
    pub name: String,
    pub prompt: String,
    pub folder: PathBuf,
    pub agent: String,
    pub output: Arc<Mutex<String>>,
    pub running: Arc<AtomicBool>,
}

impl Session {
    pub fn status(&self) -> Status {
        if self.running.load(std::sync::atomic::Ordering::Relaxed) {
            Status::InProgress
        } else {
            Status::Done
        }
    }
}

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Sessions => "Sessions",
            Tab::Roadmap => "Roadmap",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Tab::Sessions => Tab::Roadmap,
            Tab::Roadmap => Tab::Sessions,
        }
    }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_status() {
        // Arrange
        let session = Session {
            name: "test".to_string(),
            prompt: "prompt".to_string(),
            folder: PathBuf::new(),
            agent: "gemini".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            running: Arc::new(AtomicBool::new(true)),
        };

        // Act & Assert (InProgress)
        assert_eq!(session.status(), Status::InProgress);

        // Act
        session
            .running
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // Assert (Done)
        assert_eq!(session.status(), Status::Done);
    }

    #[test]
    fn test_status_icon() {
        // Arrange & Act & Assert
        assert_eq!(Status::InProgress.icon(), "⏳");
        assert_eq!(Status::Done.icon(), "✅");
    }

    #[test]
    fn test_status_color() {
        // Arrange & Act & Assert
        assert_eq!(Status::InProgress.color(), Color::Yellow);
        assert_eq!(Status::Done.color(), Color::Green);
    }

    #[test]
    fn test_tab_title() {
        // Arrange & Act & Assert
        assert_eq!(Tab::Sessions.title(), "Sessions");
        assert_eq!(Tab::Roadmap.title(), "Roadmap");
    }

    #[test]
    fn test_tab_next() {
        // Arrange & Act & Assert
        assert_eq!(Tab::Sessions.next(), Tab::Roadmap);
        assert_eq!(Tab::Roadmap.next(), Tab::Sessions);
    }
}
