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
        table_state.select(Some(0));
        Self {
            agents: vec![
                Agent {
                    name: "Search Agent".to_string(),
                    prompt: String::new(),
                    status: Status::InProgress,
                },
                Agent {
                    name: "Writing Agent".to_string(),
                    prompt: String::new(),
                    status: Status::Done,
                },
                Agent {
                    name: "Research Agent".to_string(),
                    prompt: String::new(),
                    status: Status::InProgress,
                },
            ],
            table_state,
            mode: AppMode::List,
        }
    }

    pub fn next(&mut self) {
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

    pub fn toggle_all(&mut self) {
        for agent in &mut self.agents {
            agent.status.toggle();
        }
    }
}
