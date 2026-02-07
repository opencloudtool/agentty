use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;
use tokio::io::{AsyncBufReadExt as _, AsyncRead};

use crate::agent::{AgentBackend, AgentKind};
use crate::db::Database;
use crate::git;
use crate::health::{self, HealthEntry};
use crate::model::{AppMode, Session, Tab};

pub const AGENTTY_WORKSPACE: &str = "/var/tmp/.agentty";
pub const SESSION_DATA_DIR: &str = ".agentty";

pub struct App {
    pub current_tab: Tab,
    pub mode: AppMode,
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    agent_kind: AgentKind,
    backend: Box<dyn AgentBackend>,
    base_path: PathBuf,
    db: Database,
    git_branch: Option<String>,
    git_status: Arc<Mutex<Option<(u32, u32)>>>,
    health_checks: Arc<Mutex<Vec<HealthEntry>>>,
    working_dir: PathBuf,
}

impl App {
    pub async fn new(
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        agent_kind: AgentKind,
        backend: Box<dyn AgentBackend>,
        db: Database,
    ) -> Self {
        let mut table_state = TableState::default();
        let sessions = Self::load_sessions(&base_path, &db).await;
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }

        let git_status = Arc::new(Mutex::new(None));

        if git_branch.is_some() {
            let status_clone = Arc::clone(&git_status);
            let dir_clone = working_dir.clone();

            tokio::spawn(async move {
                let repo_root = git::find_git_repo_root(&dir_clone).unwrap_or(dir_clone);

                loop {
                    let _ = git::fetch_remote(&repo_root);

                    let status = git::get_ahead_behind(&repo_root).ok();
                    if let Ok(mut lock) = status_clone.lock() {
                        *lock = status;
                    }

                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
            });
        }

        Self {
            current_tab: Tab::Sessions,
            mode: AppMode::List,
            sessions,
            table_state,
            agent_kind,
            backend,
            base_path,
            db,
            git_branch,
            git_status,
            health_checks: Arc::new(Mutex::new(Vec::new())),
            working_dir,
        }
    }

    pub fn agent_kind(&self) -> AgentKind {
        self.agent_kind
    }

    pub fn set_agent_kind(&mut self, agent_kind: AgentKind) {
        self.agent_kind = agent_kind;
        self.backend = agent_kind.create_backend();
    }

    pub fn working_dir(&self) -> &PathBuf {
        &self.working_dir
    }

    pub fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.git_status.lock().ok().and_then(|s| *s)
    }

    pub fn health_checks(&self) -> &Arc<Mutex<Vec<HealthEntry>>> {
        &self.health_checks
    }

    pub fn start_health_checks(&mut self) {
        self.health_checks =
            health::run_health_checks(self.db.pool().clone(), self.git_branch.clone());
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

    pub async fn add_session(&mut self, prompt: String) -> Result<(), String> {
        let base_branch = self
            .git_branch
            .as_deref()
            .ok_or_else(|| "Git branch is required to create a session".to_string())?;

        let mut hasher = DefaultHasher::new();
        prompt.hash(&mut hasher);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        nanos.hash(&mut hasher);
        let hash = format!("{:016x}", hasher.finish());
        let name = hash.clone();

        let folder = self.base_path.join(&hash);
        if folder.exists() {
            return Err(format!("Session folder {hash} already exists"));
        }

        // Create git worktree
        let worktree_branch = format!("agentty/{hash}");
        let repo_root = git::find_git_repo_root(&self.working_dir)
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        let folder_bg = folder.clone();
        let repo_root_bg = repo_root.clone();
        let branch_bg = worktree_branch.clone();
        let base_bg = base_branch.to_string();
        tokio::task::spawn_blocking(move || {
            git::create_worktree(&repo_root_bg, &folder_bg, &branch_bg, &base_bg)
        })
        .await
        .map_err(|e| format!("Join error: {e}"))?
        .map_err(|err| format!("Failed to create git worktree: {err}"))?;

        // Create .agentty subdirectory for session metadata
        let data_dir = folder.join(SESSION_DATA_DIR);
        let _ = std::fs::create_dir_all(&data_dir);

        let _ = std::fs::write(data_dir.join("prompt.txt"), &prompt);
        self.db
            .insert_session(&name, &self.agent_kind.to_string(), base_branch)
            .await
            .map_err(|err| format!("Failed to save session metadata: {err}"))?;

        let initial_output = format!(" › {prompt}\n\n[Git worktree: agentty/{hash}]\n\n");
        let _ = std::fs::write(data_dir.join("output.txt"), &initial_output);

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
            agent: self.agent_kind.to_string(),
            folder,
            name: name.clone(),
            output,
            prompt,
            running,
            is_creating_pr: Arc::new(AtomicBool::new(false)),
        });
        self.sessions.sort_by(|a, b| a.name.cmp(&b.name));

        if let Some(index) = self.sessions.iter().position(|a| a.name == name) {
            self.table_state.select(Some(index));
        }

        Ok(())
    }

    pub fn reply(&mut self, session_index: usize, prompt: &str) {
        let session_agent = self
            .sessions
            .get(session_index)
            .and_then(|s| s.agent.parse::<AgentKind>().ok())
            .unwrap_or(self.agent_kind);
        let backend = session_agent.create_backend();
        self.reply_with_backend(session_index, prompt, backend.as_ref());
    }

    fn reply_with_backend(
        &mut self,
        session_index: usize,
        prompt: &str,
        backend: &dyn AgentBackend,
    ) {
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
            .open(folder.join(SESSION_DATA_DIR).join("output.txt"))
            .and_then(|mut f| write!(f, "{reply_line}"));

        running.store(true, Ordering::Relaxed);
        let cmd = backend.build_resume_command(&folder, prompt);
        Self::spawn_session_task(folder, cmd, output, running);
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.table_state
            .selected()
            .and_then(|i| self.sessions.get(i))
    }

    pub async fn delete_selected_session(&mut self) {
        let Some(i) = self.table_state.selected() else {
            return;
        };
        if i >= self.sessions.len() {
            return;
        }
        let session = self.sessions.remove(i);

        let _ = self.db.delete_session(&session.name).await;

        // Remove git worktree and branch if in a git repo
        if self.git_branch.is_some() {
            let branch_name = format!("agentty/{}", session.name);

            // Find repo root for branch deletion
            if let Some(repo_root) = git::find_git_repo_root(&self.working_dir) {
                let folder = session.folder.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = git::remove_worktree(&folder);
                    let _ = git::delete_branch(&repo_root, &branch_name);
                })
                .await;
            } else {
                let folder = session.folder.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = git::remove_worktree(&folder);
                })
                .await;
            }
        }

        let _ = std::fs::remove_dir_all(&session.folder);
        if self.sessions.is_empty() {
            self.table_state.select(None);
        } else if i >= self.sessions.len() {
            self.table_state.select(Some(self.sessions.len() - 1));
        }
    }

    pub async fn commit_session(&self, session_index: usize) -> Result<String, String> {
        let session = self
            .sessions
            .get(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        // Verify this session has a git worktree via DB
        if self.db.get_base_branch(&session.name).await?.is_none() {
            return Err("No git worktree for this session".to_string());
        }

        // Commit all changes in the worktree
        let folder = session.folder.clone();
        tokio::task::spawn_blocking(move || git::commit_all(&folder, "TEST COMMIT FROM AGENT"))
            .await
            .map_err(|e| format!("Join error: {e}"))?
            .map_err(|err| format!("Failed to commit: {err}"))?;

        Ok("Successfully committed changes".to_string())
    }

    pub async fn merge_session(&self, session_index: usize) -> Result<String, String> {
        let session = self
            .sessions
            .get(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        // Read base branch from DB
        let base_branch = self
            .db
            .get_base_branch(&session.name)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Find repo root
        let repo_root = git::find_git_repo_root(&self.working_dir)
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        // Build source branch name
        let source_branch = format!("agentty/{}", session.name);

        // Build commit message from session prompt
        let commit_message = format!("Merge session: {}", session.prompt);

        // Perform squash merge
        {
            let source = source_branch.clone();
            let base = base_branch.clone();
            tokio::task::spawn_blocking(move || {
                git::squash_merge(&repo_root, &source, &base, &commit_message)
            })
            .await
            .map_err(|e| format!("Join error: {e}"))?
            .map_err(|err| format!("Failed to merge: {err}"))?;
        }

        Ok(format!(
            "Successfully merged {source_branch} into {base_branch}"
        ))
    }

    pub async fn create_pr_session(&self, session_index: usize) -> Result<(), String> {
        let session = self
            .sessions
            .get(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        if session
            .is_creating_pr
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err("Already processing a request".to_string());
        }

        // Read base branch from DB
        let base_branch = self
            .db
            .get_base_branch(&session.name)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Find repo root
        let repo_root = git::find_git_repo_root(&self.working_dir)
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        // Build source branch name
        let source_branch = format!("agentty/{}", session.name);

        // Build PR title from session prompt (first line only)
        let title = session
            .prompt
            .lines()
            .next()
            .unwrap_or("New Session")
            .to_string();

        let is_creating_pr = Arc::clone(&session.is_creating_pr);
        let output = Arc::clone(&session.output);
        let folder = session.folder.clone();

        is_creating_pr.store(true, std::sync::atomic::Ordering::Relaxed);

        // Perform PR creation in background
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                git::create_pr(&repo_root, &source_branch, &base_branch, &title)
            })
            .await;

            let result_message = match result {
                Ok(Ok(msg)) => format!("\n[PR] {msg}\n"),
                Ok(Err(err)) => format!("\n[PR Error] {err}\n"),
                Err(e) => format!("\n[PR Error] Join error: {e}\n"),
            };

            if let Ok(mut buf) = output.lock() {
                buf.push_str(&result_message);
            }
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(folder.join(SESSION_DATA_DIR).join("output.txt"))
                .and_then(|mut f| {
                    use std::io::Write;
                    write!(f, "{result_message}")
                });

            is_creating_pr.store(false, std::sync::atomic::Ordering::Relaxed);
        });

        Ok(())
    }

    async fn load_sessions(base: &Path, db: &Database) -> Vec<Session> {
        let db_rows = db.load_sessions().await.unwrap_or_default();
        let mut sessions: Vec<Session> = db_rows
            .into_iter()
            .filter_map(|row| {
                let folder = base.join(&row.name);
                if !folder.is_dir() {
                    return None;
                }
                let data_dir = folder.join(SESSION_DATA_DIR);
                let prompt = std::fs::read_to_string(data_dir.join("prompt.txt")).ok()?;
                let output_text =
                    std::fs::read_to_string(data_dir.join("output.txt")).unwrap_or_default();
                Some(Session {
                    agent: row.agent,
                    folder,
                    name: row.name,
                    output: Arc::new(Mutex::new(output_text)),
                    prompt,
                    running: Arc::new(AtomicBool::new(false)),
                    is_creating_pr: Arc::new(AtomicBool::new(false)),
                })
            })
            .collect();
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        sessions
    }

    fn spawn_session_task(
        folder: PathBuf,
        cmd: Command,
        output: Arc<Mutex<String>>,
        running: Arc<AtomicBool>,
    ) {
        let mut tokio_cmd = tokio::process::Command::from(cmd);
        tokio::spawn(async move {
            let file = std::fs::OpenOptions::new()
                .append(true)
                .open(folder.join(SESSION_DATA_DIR).join("output.txt"))
                .ok();
            let file = Arc::new(Mutex::new(file));

            match tokio_cmd.spawn() {
                Ok(mut child) => {
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();

                    let mut handles = Vec::new();

                    if let Some(stdout) = stdout {
                        let out_clone = Arc::clone(&output);
                        let file_clone = Arc::clone(&file);
                        handles.push(tokio::spawn(async move {
                            Self::process_output(stdout, &file_clone, &out_clone).await;
                        }));
                    }
                    if let Some(stderr) = stderr {
                        let out_clone = Arc::clone(&output);
                        let file_clone = Arc::clone(&file);
                        handles.push(tokio::spawn(async move {
                            Self::process_output(stderr, &file_clone, &out_clone).await;
                        }));
                    }

                    for handle in handles {
                        let _ = handle.await;
                    }
                    let _ = child.wait().await;
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

    async fn process_output<R: AsyncRead + Unpin>(
        source: R,
        file: &Arc<Mutex<Option<std::fs::File>>>,
        output: &Arc<Mutex<String>>,
    ) {
        let mut reader = tokio::io::BufReader::new(source).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(mut f_guard) = file.lock() {
                if let Some(f) = f_guard.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
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

    async fn new_test_app(path: PathBuf) -> App {
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        App::new(
            path,
            working_dir,
            None,
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await
    }

    fn setup_test_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        std::fs::write(path.join("README.md"), "test").expect("write failed");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("git commit failed");
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(path)
            .output()
            .expect("git branch failed");
    }

    async fn new_test_app_with_git(path: &Path) -> App {
        setup_test_git_repo(path);
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        App::new(
            path.to_path_buf(),
            path.to_path_buf(),
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await
    }

    fn add_manual_session(app: &mut App, base_path: &Path, name: &str, prompt: &str) {
        let folder = base_path.join(name);
        let data_dir = folder.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        std::fs::write(data_dir.join("prompt.txt"), prompt).expect("failed to write prompt");
        std::fs::write(data_dir.join("output.txt"), "").expect("failed to write output");
        app.sessions.push(Session {
            agent: "gemini".to_string(),
            folder,
            name: name.to_string(),
            output: Arc::new(Mutex::new(String::new())),
            prompt: prompt.to_string(),
            running: Arc::new(AtomicBool::new(false)),
            is_creating_pr: Arc::new(AtomicBool::new(false)),
        });
        if app.table_state.selected().is_none() {
            app.table_state.select(Some(0));
        }
    }

    #[tokio::test]
    async fn test_new_app_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Assert
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_agent_kind_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert_eq!(app.agent_kind(), AgentKind::Gemini);
    }

    #[tokio::test]
    async fn test_working_dir_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let working_dir = app.working_dir();

        // Assert
        assert_eq!(working_dir, &PathBuf::from("/tmp/test"));
    }

    #[tokio::test]
    async fn test_git_branch_getter_with_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            dir.path().to_path_buf(),
            working_dir,
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await;

        // Act
        let branch = app.git_branch();

        // Assert
        assert_eq!(branch, Some("main"));
    }

    #[tokio::test]
    async fn test_git_branch_getter_without_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let branch = app.git_branch();

        // Assert
        assert_eq!(branch, None);
    }

    #[tokio::test]
    async fn test_set_agent_kind() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        assert_eq!(app.agent_kind(), AgentKind::Gemini);

        // Act
        app.set_agent_kind(AgentKind::Claude);

        // Assert
        assert_eq!(app.agent_kind(), AgentKind::Claude);
    }

    #[tokio::test]
    async fn test_navigation() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.add_session("A".to_string())
            .await
            .expect("failed to add session");
        app.add_session("B".to_string())
            .await
            .expect("failed to add session");

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

    #[tokio::test]
    async fn test_navigation_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        app.next();
        assert_eq!(app.table_state.selected(), None);

        app.previous();
        assert_eq!(app.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_navigation_recovery() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.add_session("A".to_string())
            .await
            .expect("failed to add session");

        // Act & Assert — next recovers from None
        app.table_state.select(None);
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));

        // Act & Assert — previous recovers from None
        app.table_state.select(None);
        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_add_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;

        // Act
        app.add_session("Hello".to_string())
            .await
            .expect("failed to add session");

        // Assert
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].prompt, "Hello");
        assert_eq!(app.table_state.selected(), Some(0));
        assert_eq!(app.sessions[0].agent, "gemini");

        // Check filesystem
        let session_dir = &app.sessions[0].folder;
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        assert!(session_dir.exists());
        assert!(data_dir.join("prompt.txt").exists());
        assert!(data_dir.join("output.txt").exists());

        // Check DB
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert_eq!(db_sessions.len(), 1);
        assert_eq!(db_sessions[0].agent, "gemini");
        assert_eq!(db_sessions[0].base_branch, "main");
    }

    #[tokio::test]
    async fn test_reply() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.add_session("Initial".to_string())
            .await
            .expect("failed to add session");

        // Act
        app.reply(0, "Reply");

        // Assert
        let session = &app.sessions[0];
        let output = session.output.lock().expect("failed to lock output");
        assert!(output.contains("Reply"));
    }

    #[tokio::test]
    async fn test_selected_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.add_session("Test".to_string())
            .await
            .expect("failed to add session");

        // Act & Assert
        assert!(app.selected_session().is_some());

        app.table_state.select(None);
        assert!(app.selected_session().is_none());
    }

    #[tokio::test]
    async fn test_delete_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.add_session("A".to_string())
            .await
            .expect("failed to add session");
        let session_folder = app.sessions[0].folder.clone();

        // Act
        app.delete_selected_session().await;

        // Assert
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
        assert!(!session_folder.exists());
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert!(db_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_delete_selected_session_edge_cases() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.add_session("1".to_string())
            .await
            .expect("failed to add session");
        app.add_session("2".to_string())
            .await
            .expect("failed to add session");

        // Act & Assert — index out of bounds
        app.table_state.select(Some(99));
        app.delete_selected_session().await;
        assert_eq!(app.sessions.len(), 2);

        // Act & Assert — None selected
        app.table_state.select(None);
        app.delete_selected_session().await;
        assert_eq!(app.sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_last_session_update_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        app.add_session("1".to_string())
            .await
            .expect("failed to add session");
        app.add_session("2".to_string())
            .await
            .expect("failed to add session");

        // Act & Assert — delete last item
        app.table_state.select(Some(1));
        app.delete_selected_session().await;
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.table_state.selected(), Some(0));

        // Act & Assert — delete remaining item
        app.delete_selected_session().await;
        assert!(app.sessions.is_empty());
        assert_eq!(app.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_load_existing_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        db.insert_session("12345678", "claude", "main")
            .await
            .expect("failed to insert");

        let session_dir = dir.path().join("12345678");
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir(&session_dir).expect("failed to create session dir");
        std::fs::create_dir(&data_dir).expect("failed to create data dir");
        std::fs::write(data_dir.join("prompt.txt"), "Existing").expect("failed to write prompt");
        std::fs::write(data_dir.join("output.txt"), "Output").expect("failed to write output");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await;

        // Assert
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].name, "12345678");
        assert_eq!(app.sessions[0].prompt, "Existing");
        assert_eq!(app.sessions[0].agent, "claude");
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_load_sessions_invalid_path() {
        // Arrange
        let path = PathBuf::from("/invalid/path/that/does/not/exist");

        // Act
        let app = new_test_app(path).await;

        // Assert
        assert!(app.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_load_session_without_folder_skipped() {
        // Arrange — DB has a row but no matching folder on disk
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        db.insert_session("missing01", "gemini", "main")
            .await
            .expect("failed to insert");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            AgentKind::Gemini,
            Box::new(create_mock_backend()),
            db,
        )
        .await;

        // Assert — session is skipped because folder doesn't exist
        assert!(app.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_spawn_integration() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
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
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(mock),
            db,
        )
        .await;

        // Act — add session (start command)
        app.add_session("SpawnInit".to_string())
            .await
            .expect("failed to add session");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

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
        let mut resume_mock = MockAgentBackend::new();
        resume_mock
            .expect_build_resume_command()
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
        app.reply_with_backend(0, "SpawnReply", &resume_mock);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

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

    #[tokio::test]
    async fn test_process_output() {
        // Arrange
        let output = Arc::new(Mutex::new(String::new()));
        let file: Arc<Mutex<Option<std::fs::File>>> = Arc::new(Mutex::new(None));
        let source = "Line 1\nLine 2".as_bytes();

        // Act
        App::process_output(source, &file, &output).await;

        // Assert
        let out = output.lock().expect("failed to lock output").clone();
        assert!(out.contains("Line 1"));
        assert!(out.contains("Line 2"));

        // Arrange — with file
        let dir = tempdir().expect("failed to create temp dir");
        let file_path = dir.path().join("out.txt");
        let f = std::fs::File::create(&file_path).expect("failed to create file");
        let file = Arc::new(Mutex::new(Some(f)));
        let source_file = "File Line".as_bytes();

        // Act
        App::process_output(source_file, &file, &output).await;

        // Assert
        drop(file);
        let content = std::fs::read_to_string(file_path).expect("failed to read file");
        assert!(content.contains("File Line"));
    }

    #[tokio::test]
    async fn test_next_tab() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert_eq!(app.current_tab, Tab::Sessions);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Roadmap);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Sessions);
    }

    #[tokio::test]
    async fn test_add_session_without_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.add_session("Test".to_string()).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Git branch is required")
        );
        assert!(app.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_add_session_with_git_no_actual_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mock = MockAgentBackend::new();
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(mock),
            db,
        )
        .await;

        // Act
        let result = app.add_session("Test".to_string()).await;

        // Assert - should fail because git repo doesn't actually exist
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("git repository root")
        );
    }

    #[tokio::test]
    async fn test_add_session_cleans_up_on_error() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mock = MockAgentBackend::new();
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            AgentKind::Gemini,
            Box::new(mock),
            db,
        )
        .await;

        // Act
        let result = app.add_session("Test".to_string()).await;

        // Assert - session should not be created
        assert!(result.is_err());
        assert_eq!(app.sessions.len(), 0);

        // Verify no session folder was left behind
        let entries = std::fs::read_dir(dir.path())
            .expect("failed to read dir")
            .count();
        assert_eq!(entries, 0, "Session folder should be cleaned up on error");
    }

    #[tokio::test]
    async fn test_delete_session_without_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        app.delete_selected_session().await;

        // Assert
        assert_eq!(app.sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_commit_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.commit_session(0).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_commit_session_invalid_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.commit_session(99).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_merge_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.merge_session(0).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_merge_session_invalid_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.merge_session(99).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.create_pr_session(0).await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }
}
