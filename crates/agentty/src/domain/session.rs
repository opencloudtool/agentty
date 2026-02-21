use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::fmt;
use std::str::FromStr;

use ratatui::style::Color;

use super::agent::AgentModel;
use super::permission::PermissionMode;

pub const SESSION_DATA_DIR: &str = ".agentty";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    New,
    InProgress,
    Review,
    Rebasing,
    Merging,
    Done,
    Canceled,
}

impl Status {
    pub fn color(&self) -> Color {
        match self {
            Status::New => Color::DarkGray,
            Status::InProgress => Color::Yellow,
            Status::Review => Color::LightBlue,
            Status::Rebasing | Status::Merging => Color::Cyan,
            Status::Done => Color::Green,
            Status::Canceled => Color::Red,
        }
    }

    pub fn can_transition_to(self, next: Status) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (Status::New, Status::InProgress)
                | (Status::New | Status::InProgress, Status::Rebasing)
                | (
                    Status::Review,
                    Status::InProgress | Status::Rebasing | Status::Merging | Status::Canceled
                )
                | (Status::InProgress | Status::Rebasing, Status::Review)
                | (Status::Merging, Status::Done | Status::Review)
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::New => write!(f, "New"),
            Status::InProgress => write!(f, "InProgress"),
            Status::Review => write!(f, "Review"),
            Status::Rebasing => write!(f, "Rebasing"),
            Status::Merging => write!(f, "Merging"),
            Status::Done => write!(f, "Done"),
            Status::Canceled => write!(f, "Canceled"),
        }
    }
}

impl FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "New" => Ok(Status::New),
            "InProgress" | "Committing" => Ok(Status::InProgress),
            "Review" => Ok(Status::Review),
            "Rebasing" => Ok(Status::Rebasing),
            "Merging" => Ok(Status::Merging),
            "Done" => Ok(Status::Done),
            "Canceled" => Ok(Status::Canceled),
            _ => Err(format!("Unknown status: {s}")),
        }
    }
}

/// Size bucket derived from a session's git diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SessionSize {
    #[default]
    Xs,
    S,
    M,
    L,
    Xl,
    Xxl,
}

impl SessionSize {
    pub const ALL: [SessionSize; 6] = [
        SessionSize::Xs,
        SessionSize::S,
        SessionSize::M,
        SessionSize::L,
        SessionSize::Xl,
        SessionSize::Xxl,
    ];

    pub fn from_diff(diff: &str) -> Self {
        let changed_line_count = diff
            .lines()
            .filter(|line| {
                (line.starts_with('+') && !line.starts_with("+++"))
                    || (line.starts_with('-') && !line.starts_with("---"))
            })
            .count();

        Self::from_changed_line_count(changed_line_count)
    }

    fn from_changed_line_count(changed_line_count: usize) -> Self {
        match changed_line_count {
            0..=10 => SessionSize::Xs,
            11..=30 => SessionSize::S,
            31..=80 => SessionSize::M,
            81..=200 => SessionSize::L,
            201..=500 => SessionSize::Xl,
            _ => SessionSize::Xxl,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SessionSize::Xs => "XS",
            SessionSize::S => "S",
            SessionSize::M => "M",
            SessionSize::L => "L",
            SessionSize::Xl => "XL",
            SessionSize::Xxl => "XXL",
        }
    }
}

impl fmt::Display for SessionSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl FromStr for SessionSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "XS" | "Xs" | "xs" => Ok(SessionSize::Xs),
            "S" | "s" => Ok(SessionSize::S),
            "M" | "m" => Ok(SessionSize::M),
            "L" | "l" => Ok(SessionSize::L),
            "XL" | "Xl" | "xl" => Ok(SessionSize::Xl),
            "XXL" | "Xxl" | "xxl" => Ok(SessionSize::Xxl),
            _ => Err(format!("Unknown session size: {s}")),
        }
    }
}

/// Per-session token statistics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub struct Session {
    pub base_branch: String,
    pub folder: PathBuf,
    pub id: String,
    pub model: AgentModel,
    pub output: String,
    pub permission_mode: PermissionMode,
    pub project_name: String,
    pub prompt: String,
    pub size: SessionSize,
    pub stats: SessionStats,
    pub status: Status,
    pub summary: Option<String>,
    pub title: Option<String>,
}

impl Session {
    /// Returns the display title for this session.
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("No title")
    }
}

pub struct SessionHandles {
    pub child_pid: Arc<Mutex<Option<u32>>>,
    pub output: Arc<Mutex<String>>,
    pub status: Arc<Mutex<Status>>,
}

impl SessionHandles {
    /// Creates handles initialized with the given values.
    pub fn new(output: String, status: Status) -> Self {
        Self {
            child_pid: Arc::new(Mutex::new(None)),
            output: Arc::new(Mutex::new(output)),
            status: Arc::new(Mutex::new(status)),
        }
    }

    /// Appends text to the output buffer.
    pub fn append_output(&self, message: &str) {
        if let Ok(mut buf) = self.output.lock() {
            buf.push_str(message);
        }
    }
}
