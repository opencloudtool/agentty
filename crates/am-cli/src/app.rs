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
        self.agents.push(Agent {
            name,
            prompt,
            status: Status::InProgress,
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
