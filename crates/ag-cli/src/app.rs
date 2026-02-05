use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;

use crate::agent::{AgentBackend, AgentKind};
use crate::model::{AppMode, Session, Tab};

pub const AGENTTY_WORKSPACE: &str = "/var/tmp/.agentty";

pub struct App {
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    pub mode: AppMode,
    pub current_tab: Tab,
    base_path: PathBuf,
    agent_kind: AgentKind,
    backend: Box<dyn AgentBackend>,
}

impl App {
    pub fn new(base_path: PathBuf, agent_kind: AgentKind, backend: Box<dyn AgentBackend>) -> Self {
        let mut table_state = TableState::default();
        let sessions = Self::load_sessions(&base_path);
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }
        Self {
            sessions,
            table_state,
            mode: AppMode::List,
            current_tab: Tab::Sessions,
            base_path,
            agent_kind,
            backend,
        }
    }

    pub fn agent_kind(&self) -> AgentKind {
        self.agent_kind
    }

    pub fn next_tab(&mut self) {
        self.current_tab = self.current_tab.next();
    }

    pub fn next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.sessions.len() - 1 {
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
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.sessions.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn add_session(&mut self, prompt: String) {
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
        let _ = std::fs::write(folder.join("agent.txt"), self.agent_kind.to_string());

        let initial_output = format!(" › {prompt}\n\n");
        let _ = std::fs::write(folder.join("output.txt"), &initial_output);

        self.backend.setup(&folder);

        let output = Arc::new(Mutex::new(initial_output));
        let running = Arc::new(AtomicBool::new(true));

        let cmd = self.backend.build_start_command(&folder, &prompt);
        Self::spawn_session_task(
            folder.clone(),
            cmd,
            Arc::clone(&output),
            Arc::clone(&running),
        );

        self.sessions.push(Session {
            name: name.clone(),
            prompt,
            folder,
            agent: self.agent_kind.to_string(),
            output,
            running,
        });
        self.sessions.sort_by(|a, b| a.name.cmp(&b.name));

        if let Some(index) = self.sessions.iter().position(|a| a.name == name) {
            self.table_state.select(Some(index));
        }
    }

    pub fn reply(&mut self, session_index: usize, prompt: &str) {
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };

        let folder = session.folder.clone();
        let output = Arc::clone(&session.output);
        let running = Arc::clone(&session.running);

        let reply_line = format!("\n › {prompt}\n\n");
        if let Ok(mut buf) = output.lock() {
            buf.push_str(&reply_line);
        }
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join("output.txt"))
            .and_then(|mut f| write!(f, "{reply_line}"));

        running.store(true, Ordering::Relaxed);
        let cmd = self.backend.build_resume_command(&folder, prompt);
        Self::spawn_session_task(folder, cmd, output, running);
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.table_state
            .selected()
            .and_then(|i| self.sessions.get(i))
    }

    pub fn delete_selected_session(&mut self) {
        let Some(i) = self.table_state.selected() else {
            return;
        };
        if i >= self.sessions.len() {
            return;
        }
        let session = self.sessions.remove(i);
        let _ = std::fs::remove_dir_all(&session.folder);
        if self.sessions.is_empty() {
            self.table_state.select(None);
        } else if i >= self.sessions.len() {
            self.table_state.select(Some(self.sessions.len() - 1));
        }
    }

    fn load_sessions(base: &PathBuf) -> Vec<Session> {
        let Ok(entries) = std::fs::read_dir(base) else {
            return Vec::new();
        };
        let mut sessions: Vec<Session> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let folder = entry.path();
                if !folder.is_dir() {
                    return None;
                }
                let prompt = std::fs::read_to_string(folder.join("prompt.txt")).ok()?;
                let output_text =
                    std::fs::read_to_string(folder.join("output.txt")).unwrap_or_default();
                let agent = std::fs::read_to_string(folder.join("agent.txt"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                Some(Session {
                    name: folder.file_name()?.to_string_lossy().into_owned(),
                    prompt,
                    folder,
                    agent,
                    output: Arc::new(Mutex::new(output_text)),
                    running: Arc::new(AtomicBool::new(false)),
                })
            })
            .collect();
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        sessions
    }

    fn spawn_session_task(
        folder: PathBuf,
        mut cmd: Command,
        output: Arc<Mutex<String>>,
        running: Arc<AtomicBool>,
    ) {
        std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(folder.join("output.txt"))
                .ok();

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
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::MockAgentBackend;

    fn create_mock_backend() -> MockAgentBackend {
        let mut mock = MockAgentBackend::new();
        mock.expect_setup().returning(|_| {});
        mock.expect_build_start_command().returning(|folder, _| {
            let mut cmd = Command::new("echo");
            cmd.arg("mock-start")
                .current_dir(folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            cmd
        });
        mock.expect_build_resume_command().returning(|folder, _| {
            let mut cmd = Command::new("echo");
            cmd.arg("mock-resume")
                .current_dir(folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            cmd
        });
        mock
    }

    fn new_test_app(path: PathBuf) -> App {
        App::new(path, AgentKind::Gemini, Box::new(create_mock_backend()))
    }

    #[test]
    fn test_new_app_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let app = new_test_app(dir.path().to_path_buf());

        // Assert
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn test_agent_kind_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf());

        // Act & Assert
        assert_eq!(app.agent_kind(), AgentKind::Gemini);
    }

    #[test]
    fn test_navigation() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());
        app.add_session("A".to_string());
        app.add_session("B".to_string());

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
    fn test_navigation_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());

        // Act & Assert
        app.next();
        assert_eq!(app.table_state.selected(), None);

        app.previous();
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn test_navigation_recovery() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());
        app.add_session("A".to_string());

        // Act & Assert — next recovers from None
        app.table_state.select(None);
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));

        // Act & Assert — previous recovers from None
        app.table_state.select(None);
        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_add_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());

        // Act
        app.add_session("Hello".to_string());

        // Assert
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].prompt, "Hello");
        assert_eq!(app.table_state.selected(), Some(0));

        assert_eq!(app.sessions[0].agent, "gemini");

        // Check filesystem
        let session_dir = &app.sessions[0].folder;
        assert!(session_dir.exists());
        assert!(session_dir.join("prompt.txt").exists());
        assert!(session_dir.join("output.txt").exists());
        assert_eq!(
            std::fs::read_to_string(session_dir.join("agent.txt")).expect("agent.txt"),
            "gemini"
        );
    }

    #[test]
    fn test_reply() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());
        app.add_session("Initial".to_string());

        // Act
        app.reply(0, "Reply");

        // Assert
        let session = &app.sessions[0];
        let output = session.output.lock().expect("failed to lock output");
        assert!(output.contains("Reply"));
    }

    #[test]
    fn test_selected_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());
        app.add_session("Test".to_string());

        // Act & Assert
        assert!(app.selected_session().is_some());

        app.table_state.select(None);
        assert!(app.selected_session().is_none());
    }

    #[test]
    fn test_delete_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());
        app.add_session("A".to_string());

        // Act
        app.delete_selected_session();

        // Assert
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }

    #[test]
    fn test_delete_selected_session_edge_cases() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());
        app.add_session("1".to_string());
        app.add_session("2".to_string());

        // Act & Assert — index out of bounds
        app.table_state.select(Some(99));
        app.delete_selected_session();
        assert_eq!(app.sessions.len(), 2);

        // Act & Assert — None selected
        app.table_state.select(None);
        app.delete_selected_session();
        assert_eq!(app.sessions.len(), 2);
    }

    #[test]
    fn test_delete_last_session_update_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());
        app.add_session("1".to_string());
        app.add_session("2".to_string());

        // Act & Assert — delete last item
        app.table_state.select(Some(1));
        app.delete_selected_session();
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.table_state.selected(), Some(0));

        // Act & Assert — delete remaining item
        app.delete_selected_session();
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn test_load_existing_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let session_dir = dir.path().join("12345678");
        std::fs::create_dir(&session_dir).expect("failed to create session dir");
        std::fs::write(session_dir.join("prompt.txt"), "Existing").expect("failed to write prompt");
        std::fs::write(session_dir.join("output.txt"), "Output").expect("failed to write output");
        std::fs::write(session_dir.join("agent.txt"), "claude").expect("failed to write agent");

        // Add some garbage files to test filter logic
        std::fs::write(dir.path().join("ignored_file.txt"), "")
            .expect("failed to write ignored file");
        let ignored_dir = dir.path().join("ignored_dir");
        std::fs::create_dir(&ignored_dir).expect("failed to create ignored dir");

        // Act
        let app = new_test_app(dir.path().to_path_buf());

        // Assert
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].name, "12345678");
        assert_eq!(app.sessions[0].prompt, "Existing");
        assert_eq!(app.sessions[0].agent, "claude");
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_load_sessions_invalid_path() {
        // Arrange
        let path = PathBuf::from("/invalid/path/that/does/not/exist");

        // Act
        let app = new_test_app(path);

        // Assert
        assert!(app.sessions.is_empty());
    }

    #[test]
    fn test_load_session_without_agent_txt_defaults_to_unknown() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let session_dir = dir.path().join("noagent01");
        std::fs::create_dir(&session_dir).expect("failed to create session dir");
        std::fs::write(session_dir.join("prompt.txt"), "Test").expect("failed to write prompt");

        // Act
        let app = new_test_app(dir.path().to_path_buf());

        // Assert
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].agent, "unknown");
    }

    #[test]
    fn test_spawn_integration() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut mock = MockAgentBackend::new();
        mock.expect_setup().returning(|_| {});
        mock.expect_build_start_command()
            .returning(|folder, prompt| {
                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("--model")
                    .arg("gemini-3-flash-preview")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        mock.expect_build_resume_command()
            .returning(|folder, prompt| {
                let mut cmd = Command::new("echo");
                cmd.arg("--prompt")
                    .arg(prompt)
                    .arg("--model")
                    .arg("gemini-3-flash-preview")
                    .arg("--resume")
                    .arg("latest")
                    .current_dir(folder)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                cmd
            });
        let mut app = App::new(dir.path().to_path_buf(), AgentKind::Gemini, Box::new(mock));

        // Act — add session (start command)
        app.add_session("SpawnInit".to_string());
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Assert
        {
            let session = &app.sessions[0];
            let output = session
                .output
                .lock()
                .expect("failed to lock output")
                .clone();
            assert!(output.contains("--prompt"));
            assert!(output.contains("SpawnInit"));
            assert!(!output.contains("--resume"));
        }

        // Act — reply (resume command)
        app.reply(0, "SpawnReply");
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Assert
        {
            let session = &app.sessions[0];
            let output = session
                .output
                .lock()
                .expect("failed to lock output")
                .clone();
            assert!(output.contains("SpawnReply"));
            assert!(output.contains("--resume"));
            assert!(output.contains("latest"));
        }
    }

    #[test]
    fn test_process_output_sync() {
        // Arrange
        let output = Arc::new(Mutex::new(String::new()));
        let mut file = None;
        let source = "Line 1\nLine 2".as_bytes();

        // Act
        App::process_output(source, &mut file, &output);

        // Assert
        let out = output.lock().expect("failed to lock output").clone();
        assert!(out.contains("Line 1"));
        assert!(out.contains("Line 2"));

        // Arrange — with file
        let dir = tempdir().expect("failed to create temp dir");
        let file_path = dir.path().join("out.txt");
        let mut file = Some(std::fs::File::create(&file_path).expect("failed to create file"));
        let source_file = "File Line".as_bytes();

        // Act
        App::process_output(source_file, &mut file, &output);

        // Assert
        drop(file);
        let content = std::fs::read_to_string(file_path).expect("failed to read file");
        assert!(content.contains("File Line"));
    }

    #[test]
    fn test_next_tab() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf());

        // Act & Assert
        assert_eq!(app.current_tab, Tab::Sessions);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Roadmap);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Sessions);
    }
}
