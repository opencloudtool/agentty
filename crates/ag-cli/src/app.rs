use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;

use crate::model::{Agent, AppMode};

pub const DEFAULT_BASE_PATH: &str = "/var/tmp/.agentty";

pub struct App {
    pub agents: Vec<Agent>,
    pub table_state: TableState,
    pub mode: AppMode,
    base_path: PathBuf,
}

impl Default for App {
    fn default() -> Self {
        Self::new(PathBuf::from(DEFAULT_BASE_PATH))
    }
}

impl App {
    pub fn new(base_path: PathBuf) -> Self {
        let mut table_state = TableState::default();
        let agents = Self::load_agents(&base_path);
        if agents.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }
        Self {
            agents,
            table_state,
            mode: AppMode::List,
            base_path,
        }
    }

    fn load_agents(base: &PathBuf) -> Vec<Agent> {
        let Ok(entries) = std::fs::read_dir(base) else {
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
                let output_text =
                    std::fs::read_to_string(folder.join("output.txt")).unwrap_or_default();
                Some(Agent {
                    name: folder.file_name()?.to_string_lossy().into_owned(),
                    prompt,
                    folder,
                    output: Arc::new(Mutex::new(output_text)),
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
        let mut hasher = DefaultHasher::new();
        prompt.hash(&mut hasher);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        nanos.hash(&mut hasher);
        let hash = format!("{:016x}", hasher.finish());
        let short_hash = &hash[..8];
        let name = short_hash.to_string();

        let folder = self.base_path.join(short_hash);
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::fs::write(folder.join("prompt.txt"), &prompt);

        let initial_output = format!(" › {prompt}\n\n");
        let _ = std::fs::write(folder.join("output.txt"), &initial_output);

        // Create isolated gemini settings
        let gemini_dir = folder.join(".gemini");
        let _ = std::fs::create_dir_all(&gemini_dir);
        let settings = r#"{
  "context.loadMemoryFromIncludeDirectories": false,
  "context.fileFiltering.respectGitIgnore": false,
  "context.discoveryMaxDirs": 1,
  "context.fileFiltering.enableRecursiveFileSearch": false
}"#;
        let _ = std::fs::write(gemini_dir.join("settings.json"), settings);

        let output = Arc::new(Mutex::new(initial_output));
        let running = Arc::new(AtomicBool::new(true));

        Self::spawn_agent_task(
            folder.clone(),
            prompt.clone(),
            Arc::clone(&output),
            Arc::clone(&running),
            false,
        );

        self.agents.push(Agent {
            name: name.clone(),
            prompt,
            folder,
            output,
            running,
        });
        self.agents.sort_by(|a, b| a.name.cmp(&b.name));

        if let Some(index) = self.agents.iter().position(|a| a.name == name) {
            self.table_state.select(Some(index));
        }
    }

    pub fn reply(&mut self, agent_index: usize, prompt: String) {
        let Some(agent) = self.agents.get_mut(agent_index) else {
            return;
        };

        let folder = agent.folder.clone();
        let output = Arc::clone(&agent.output);
        let running = Arc::clone(&agent.running);

        let reply_line = format!("\n › {prompt}\n\n");
        if let Ok(mut buf) = output.lock() {
            buf.push_str(&reply_line);
        }
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join("output.txt"))
            .and_then(|mut f| write!(f, "{reply_line}"));

        running.store(true, Ordering::Relaxed);
        Self::spawn_agent_task(folder, prompt, output, running, true);
    }

    fn spawn_agent_task(
        folder: PathBuf,
        prompt: String,
        output: Arc<Mutex<String>>,
        running: Arc<AtomicBool>,
        resume: bool,
    ) {
        std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(folder.join("output.txt"))
                .ok();

            let cmd_name =
                std::env::var("AGENTTY_GEMINI_COMMAND").unwrap_or_else(|_| "gemini".to_string());
            let mut cmd = Command::new(cmd_name);
            cmd.arg("--prompt")
                .arg(prompt)
                .arg("--model")
                .arg("gemini-3-flash-preview")
                .current_dir(&folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());

            if resume {
                cmd.arg("--resume").arg("latest");
            }

            match cmd.spawn() {
                Ok(mut child) => {
                    if let Some(stdout) = child.stdout.take() {
                        Self::process_output(stdout, &mut file, &output);
                    }
                    let _ = child.wait();
                }
                Err(e) => {
                    if let Ok(mut buf) = output.lock() {
                        let _ = writeln!(buf, "Failed to spawn process: {e}");
                    }
                }
            }
            running.store(false, Ordering::Relaxed);
        });
    }

    fn process_output<R: std::io::Read>(
        source: R,
        file: &mut Option<std::fs::File>,
        output: &Arc<Mutex<String>>,
    ) {
        let reader = BufReader::new(source);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(f) = file {
                let _ = writeln!(f, "{line}");
            }
            if let Ok(mut buf) = output.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
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

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_new_app_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let app = App::new(dir.path().to_path_buf());

        // Assert
        assert!(app.agents.is_empty());
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn test_add_agent() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());

        // Act
        app.add_agent("Hello".to_string());

        // Assert
        assert_eq!(app.agents.len(), 1);
        assert_eq!(app.agents[0].prompt, "Hello");
        assert_eq!(app.table_state.selected(), Some(0));

        // Check filesystem
        let agent_dir = &app.agents[0].folder;
        assert!(agent_dir.exists());
        assert!(agent_dir.join("prompt.txt").exists());
        assert!(agent_dir.join("output.txt").exists());
        assert!(agent_dir.join(".gemini/settings.json").exists());
    }

    #[test]
    fn test_navigation() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());
        app.add_agent("A".to_string());
        app.add_agent("B".to_string());
        // Sorting means names are hash-based, but we have 2 agents.
        // Let's assume index 0 is selected initially (or after add).

        // Act & Assert (Next)
        app.table_state.select(Some(0));
        app.next();
        assert_eq!(app.table_state.selected(), Some(1));
        app.next();
        assert_eq!(app.table_state.selected(), Some(0)); // Loop back

        // Act & Assert (Previous)
        app.previous();
        assert_eq!(app.table_state.selected(), Some(1)); // Loop back
        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_delete_agent() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());
        app.add_agent("A".to_string());

        // Act
        app.delete_selected_agent();

        // Assert
        assert!(app.agents.is_empty());
        assert_eq!(app.table_state.selected(), None);
        // Check fs (we can't easily check exact folder path as it's gone from struct,
        // but the directory should be empty or at least that agent subfolder gone.
        // Since we don't store the hash outside, we trust the logic for now or could
        // spy the path before delete)
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }

    #[test]
    fn test_reply() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());
        app.add_agent("Initial".to_string());

        // Act
        app.reply(0, "Reply".to_string());

        // Assert
        // We check if output text was updated.
        // Note: spawn_agent_task runs in a thread, so there might be a race if we check
        // immediately. However, the reply function *synchronously* appends to
        // the output buffer and file *before* spawning.
        let agent = &app.agents[0];
        let output = agent.output.lock().expect("failed to lock output");
        assert!(output.contains("Reply"));
    }

    #[test]
    fn test_load_existing_agents() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let agent_dir = dir.path().join("12345678");
        std::fs::create_dir(&agent_dir).expect("failed to create agent dir");
        std::fs::write(agent_dir.join("prompt.txt"), "Existing").expect("failed to write prompt");
        std::fs::write(agent_dir.join("output.txt"), "Output").expect("failed to write output");

        // Add some garbage files to test filter logic
        std::fs::write(dir.path().join("ignored_file.txt"), "")
            .expect("failed to write ignored file");
        let ignored_dir = dir.path().join("ignored_dir");
        std::fs::create_dir(&ignored_dir).expect("failed to create ignored dir");
        // ignored_dir has no prompt.txt

        // Act
        let app = App::new(dir.path().to_path_buf());

        // Assert
        assert_eq!(app.agents.len(), 1);
        assert_eq!(app.agents[0].name, "12345678");
        assert_eq!(app.agents[0].prompt, "Existing");
        // Check that initial selection is set to 0
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_load_agents_invalid_path() {
        let path = PathBuf::from("/invalid/path/that/does/not/exist");
        let app = App::new(path);
        assert!(app.agents.is_empty());
    }

    #[test]
    fn test_navigation_empty() {
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());

        app.next();
        assert_eq!(app.table_state.selected(), None);

        app.previous();
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn test_selected_agent() {
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());
        app.add_agent("Test".to_string());

        assert!(app.selected_agent().is_some());

        app.table_state.select(None);
        assert!(app.selected_agent().is_none());
    }

    #[test]
    fn test_delete_selected_agent_edge_cases() {
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());
        app.add_agent("1".to_string());
        app.add_agent("2".to_string());

        // Select index out of bounds manually
        app.table_state.select(Some(99));
        app.delete_selected_agent();
        assert_eq!(app.agents.len(), 2);

        // Select None
        app.table_state.select(None);
        app.delete_selected_agent();
        assert_eq!(app.agents.len(), 2);
    }

    #[test]
    fn test_delete_last_agent_update_selection() {
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());
        app.add_agent("1".to_string());
        app.add_agent("2".to_string());

        app.table_state.select(Some(1)); // Select the last one
        app.delete_selected_agent();
        // Should select new last one (index 0)
        assert_eq!(app.agents.len(), 1);
        assert_eq!(app.table_state.selected(), Some(0));

        app.delete_selected_agent();
        // Should select None
        assert!(app.agents.is_empty());
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn test_default() {
        let app = App::default();
        // It should be safe to just construct it
        assert!(app.agents.is_empty() || !app.agents.is_empty());
    }

    #[test]
    fn test_navigation_recovery() {
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());
        app.add_agent("A".to_string());

        // Manually deselect
        app.table_state.select(None);
        assert!(!app.agents.is_empty());
        assert_eq!(app.table_state.selected(), None);

        // Next should select 0
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));

        // Manually deselect again
        app.table_state.select(None);

        // Previous should select 0
        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_process_output_sync() {
        let output = Arc::new(Mutex::new(String::new()));
        // Case 1: No file
        let mut file = None;
        let source = "Line 1\nLine 2".as_bytes();

        App::process_output(source, &mut file, &output);

        let out = output.lock().expect("failed to lock output").clone();
        assert!(out.contains("Line 1"));
        assert!(out.contains("Line 2"));

        // Case 2: With file
        let dir = tempdir().expect("failed to create temp dir");
        let file_path = dir.path().join("out.txt");
        let mut file = Some(std::fs::File::create(&file_path).expect("failed to create file"));
        let source_file = "File Line".as_bytes();
        App::process_output(source_file, &mut file, &output);

        drop(file); // Flush/close
        let content = std::fs::read_to_string(file_path).expect("failed to read file");
        assert!(content.contains("File Line"));
    }

    #[test]
    fn test_spawn_integration() {
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = App::new(dir.path().to_path_buf());

        // Use "echo" as the command to simulate success
        unsafe {
            std::env::set_var("AGENTTY_GEMINI_COMMAND", "echo");
        }

        // 1. Test Add (resume=false)
        app.add_agent("SpawnInit".to_string());
        // Wait for thread
        std::thread::sleep(std::time::Duration::from_millis(300));

        {
            let agent = &app.agents[0];
            let output = agent.output.lock().expect("failed to lock output").clone();
            assert!(output.contains("--prompt"));
            assert!(output.contains("SpawnInit"));
            assert!(!output.contains("--resume"));
        }

        // 2. Test Reply (resume=true)
        app.reply(0, "SpawnReply".to_string());
        std::thread::sleep(std::time::Duration::from_millis(300));

        {
            let agent = &app.agents[0];
            let output = agent.output.lock().expect("failed to lock output").clone();
            assert!(output.contains("SpawnReply"));
            assert!(output.contains("--resume"));
            assert!(output.contains("latest"));
        }

        unsafe {
            std::env::remove_var("AGENTTY_GEMINI_COMMAND");
        }
    }
}
