use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;

use crate::model::{Agent, AppMode};

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
        let agents = Self::load_agents();
        if agents.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }
        Self {
            agents,
            table_state,
            mode: AppMode::List,
        }
    }

    fn load_agents() -> Vec<Agent> {
        let base = PathBuf::from("/var/tmp/.agentty");
        let Ok(entries) = std::fs::read_dir(&base) else {
            return Vec::new();
        };
        let mut agents: Vec<Agent> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let folder = entry.path();
                if !folder.is_dir() {
                    return None;
                }
                let prompt = std::fs::read_to_string(folder.join("prompt.txt")).ok()?;
                Some(Agent {
                    name: folder.file_name()?.to_string_lossy().into_owned(),
                    prompt,
                    folder,
                    output: Arc::new(Mutex::new(String::new())),
                    running: Arc::new(AtomicBool::new(false)),
                })
            })
            .collect();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        agents
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

        let output = Arc::new(Mutex::new(String::new()));
        let running = Arc::new(AtomicBool::new(true));

        // Spawn background process
        let output_clone = Arc::clone(&output);
        let running_clone = Arc::clone(&running);
        std::thread::spawn(move || {
            let child = Command::new("bash")
                .arg("-c")
                .arg("for i in $(seq 0 60); do echo $i; sleep 1; done")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();

            match child {
                Ok(mut child) => {
                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Ok(mut buf) = output_clone.lock() {
                                buf.push_str(&line);
                                buf.push('\n');
                            }
                        }
                    }
                    let _ = child.wait();
                }
                Err(e) => {
                    if let Ok(mut buf) = output_clone.lock() {
                        let _ = writeln!(buf, "Failed to spawn process: {e}");
                    }
                }
            }
            running_clone.store(false, Ordering::Relaxed);
        });

        self.agents.push(Agent {
            name,
            prompt,
            folder,
            output,
            running,
        });
        if self.table_state.selected().is_none() {
            self.table_state.select(Some(0));
        }
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.table_state.selected().and_then(|i| self.agents.get(i))
    }

    pub fn delete_selected_agent(&mut self) {
        let Some(i) = self.table_state.selected() else {
            return;
        };
        if i >= self.agents.len() {
            return;
        }
        let agent = self.agents.remove(i);
        let _ = std::fs::remove_dir_all(&agent.folder);
        if self.agents.is_empty() {
            self.table_state.select(None);
        } else if i >= self.agents.len() {
            self.table_state.select(Some(self.agents.len() - 1));
        }
    }
}
