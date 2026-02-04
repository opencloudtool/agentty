use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Eq)]
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
        agent_index: usize,
        scroll_offset: Option<u16>,
    },
    Reply {
        agent_index: usize,
        input: String,
        scroll_offset: Option<u16>,
    },
}

pub struct Agent {
    pub name: String,
    pub prompt: String,
    pub folder: PathBuf,
    pub output: Arc<Mutex<String>>,
    pub running: Arc<AtomicBool>,
}

impl Agent {
    pub fn status(&self) -> Status {
        if self.running.load(std::sync::atomic::Ordering::Relaxed) {
            Status::InProgress
        } else {
            Status::Done
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
