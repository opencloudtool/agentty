use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

use crate::icon::Icon;

pub const SESSION_DATA_DIR: &str = ".agentty";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Sessions,
    Roadmap,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    InProgress,
    Processing,
    Done,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::InProgress => write!(f, "InProgress"),
            Status::Processing => write!(f, "Processing"),
            Status::Done => write!(f, "Done"),
        }
    }
}

impl std::str::FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "InProgress" => Ok(Status::InProgress),
            "Processing" => Ok(Status::Processing),
            "Done" => Ok(Status::Done),
            _ => Err(format!("Unknown status: {s}")),
        }
    }
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
    pub status: Arc<Mutex<Status>>,
}

impl Session {
    pub fn status(&self) -> Status {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn append_output(&self, message: &str) {
        Self::write_output(&self.output, &self.folder, message);
    }

    pub(crate) fn write_output(output: &Mutex<String>, folder: &Path, message: &str) {
        if let Ok(mut buf) = output.lock() {
            buf.push_str(message);
        }
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join(SESSION_DATA_DIR).join("output.txt"))
            .and_then(|mut file| write!(file, "{message}"));
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
            Status::InProgress | Status::Processing => Icon::current_spinner(),
            Status::Done => Icon::Check,
        }
    }

    pub fn color(self) -> Color {
        match self {
            Status::InProgress => Color::Yellow,
            Status::Processing => Color::Cyan,
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
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act & Assert (InProgress)
        assert_eq!(session.status(), Status::InProgress);

        // Act
        if let Ok(mut status) = session.status.lock() {
            *status = Status::Done;
        }

        // Assert (Done)
        assert_eq!(session.status(), Status::Done);

        // Act
        if let Ok(mut status) = session.status.lock() {
            *status = Status::Processing;
        }

        // Assert (Processing because creating PR)
        assert_eq!(session.status(), Status::Processing);
    }

    #[test]
    fn test_append_output() {
        // Arrange
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let data_dir = dir.path().join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        std::fs::write(data_dir.join("output.txt"), "").expect("failed to write output");

        let session = Session {
            agent: "gemini".to_string(),
            folder: dir.path().to_path_buf(),
            name: "test".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            prompt: "prompt".to_string(),
            status: Arc::new(Mutex::new(Status::Done)),
        };

        // Act
        session.append_output("[Test] Hello\n");

        // Assert — in-memory buffer
        let buf = session.output.lock().expect("lock failed");
        assert_eq!(*buf, "[Test] Hello\n");
        drop(buf);

        // Assert — file on disk
        let content =
            std::fs::read_to_string(data_dir.join("output.txt")).expect("failed to read output");
        assert_eq!(content, "[Test] Hello\n");
    }

    #[test]
    fn test_status_icon() {
        // Arrange & Act & Assert
        assert!(matches!(Status::InProgress.icon(), Icon::Spinner(_)));
        assert!(matches!(Status::Processing.icon(), Icon::Spinner(_)));
        assert_eq!(Status::Done.icon(), Icon::Check);
    }

    #[test]
    fn test_status_color() {
        // Arrange & Act & Assert
        assert_eq!(Status::InProgress.color(), Color::Yellow);
        assert_eq!(Status::Processing.color(), Color::Cyan);
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
