use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

use crate::icon::Icon;

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
    Diff {
        session_index: usize,
        diff: String,
        scroll_offset: u16,
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
    Health,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteFocus {
    Input,
    Dropdown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteCommand {
    Agents,
    Health,
}

impl PaletteCommand {
    pub const ALL: &[PaletteCommand] = &[PaletteCommand::Agents, PaletteCommand::Health];

    pub fn label(self) -> &'static str {
        match self {
            PaletteCommand::Agents => "agents",
            PaletteCommand::Health => "health",
        }
    }

    pub fn filter(query: &str) -> Vec<PaletteCommand> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<PaletteCommand> = Self::ALL
            .iter()
            .filter(|cmd| cmd.label().contains(&query_lower))
            .copied()
            .collect();
        results.sort_by_key(|cmd| cmd.label());
        results
    }
}

pub struct Session {
    pub agent: String,
    pub folder: PathBuf,
    pub name: String,
    pub output: Arc<Mutex<String>>,
    pub prompt: String,
    pub running: Arc<AtomicBool>,
    pub is_creating_pr: Arc<AtomicBool>,
}

impl Session {
    pub fn status(&self) -> Status {
        if self.running.load(std::sync::atomic::Ordering::Relaxed)
            || self
                .is_creating_pr
                .load(std::sync::atomic::Ordering::Relaxed)
        {
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
    pub fn icon(self) -> Icon {
        match self {
            Status::InProgress => Icon::current_spinner(),
            Status::Done => Icon::Check,
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
            agent: "gemini".to_string(),
            folder: PathBuf::new(),
            name: "test".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            prompt: "prompt".to_string(),
            running: Arc::new(AtomicBool::new(true)),
            is_creating_pr: Arc::new(AtomicBool::new(false)),
        };

        // Act & Assert (InProgress because running)
        assert_eq!(session.status(), Status::InProgress);

        // Act
        session
            .running
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // Assert (Done)
        assert_eq!(session.status(), Status::Done);

        // Act
        session
            .is_creating_pr
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // Assert (InProgress because processing)
        assert_eq!(session.status(), Status::InProgress);
    }

    #[test]
    fn test_status_icon() {
        // Arrange & Act & Assert
        assert!(matches!(Status::InProgress.icon(), Icon::Spinner(_)));
        assert_eq!(Status::Done.icon(), Icon::Check);
    }

    #[test]
    fn test_status_color() {
        // Arrange & Act & Assert
        assert_eq!(Status::InProgress.color(), Color::Yellow);
        assert_eq!(Status::Done.color(), Color::Green);
    }

    #[test]
    fn test_palette_command_label() {
        // Arrange & Act & Assert
        assert_eq!(PaletteCommand::Agents.label(), "agents");
        assert_eq!(PaletteCommand::Health.label(), "health");
    }

    #[test]
    fn test_palette_command_all() {
        // Arrange & Act & Assert
        assert_eq!(PaletteCommand::ALL.len(), 2);
        assert_eq!(PaletteCommand::ALL[0], PaletteCommand::Agents);
        assert_eq!(PaletteCommand::ALL[1], PaletteCommand::Health);
    }

    #[test]
    fn test_palette_command_filter() {
        // Arrange & Act
        let results = PaletteCommand::filter("age");

        // Assert
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], PaletteCommand::Agents);
    }

    #[test]
    fn test_palette_command_filter_case_insensitive() {
        // Arrange & Act
        let results = PaletteCommand::filter("AGE");

        // Assert
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], PaletteCommand::Agents);
    }

    #[test]
    fn test_palette_command_filter_no_match() {
        // Arrange & Act
        let results = PaletteCommand::filter("xyz");

        // Assert
        assert!(results.is_empty());
    }

    #[test]
    fn test_palette_command_filter_empty_query() {
        // Arrange & Act
        let results = PaletteCommand::filter("");

        // Assert — empty query matches all commands
        assert_eq!(results.len(), PaletteCommand::ALL.len());
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
