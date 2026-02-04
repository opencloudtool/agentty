use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;

use crate::model::{Agent, AppMode, Status};

pub struct App {
    pub agents: Vec<Agent>,
    pub table_state: TableState,
    pub mode: AppMode,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(None);
        Self {
            agents: Vec::new(),
            table_state,
            mode: AppMode::List,
        }
    }

    pub fn next(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.agents.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.agents.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn add_agent(&mut self, prompt: String) {
        let name = format!("Agent {}", self.agents.len() + 1);

        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        prompt.hash(&mut hasher);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        nanos.hash(&mut hasher);
        let hash = format!("{:016x}", hasher.finish());
        let short_hash = &hash[..8];

        let folder = PathBuf::from(format!("/var/tmp/.agentty/{short_hash}"));
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::fs::write(folder.join("prompt.txt"), &prompt);

        self.agents.push(Agent {
            name,
            prompt,
            status: Status::InProgress,
            folder,
        });
        if self.table_state.selected().is_none() {
            self.table_state.select(Some(0));
        }
    }

    pub fn toggle_all(&mut self) {
        for agent in &mut self.agents {
            agent.status.toggle();
        }
    }
}
