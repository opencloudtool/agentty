use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::acp::AcpSessionHandle;
use crate::agent::{AgentKind, AgentModel};
use crate::app::App;
use crate::db::Database;
use crate::git;
use crate::model::{AppMode, Project, SESSION_DATA_DIR, Session, SessionStats, Status};

pub(super) const SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const COMMIT_SUMMARY_BEGIN: &str = "BEGIN_COMMIT_MESSAGE";
const COMMIT_SUMMARY_END: &str = "END_COMMIT_MESSAGE";
static COMMIT_SUMMARY_PROMPT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "\
Review the current git changes and write a commit title/body that follows AGENTS.md rules.
Rules:
- The title is concise and in present simple tense.
- Use a blank line between title and body.
- Use `-` bullet points in the body.
Respond with this exact format only:
{COMMIT_SUMMARY_BEGIN}
TITLE: <title>
BODY:
- <body bullet 1>
- <body bullet 2>
{COMMIT_SUMMARY_END}
Do not output anything before or after this block."
    )
});
type SessionHandles = (
    Option<Arc<AcpSessionHandle>>,
    Arc<Mutex<String>>,
    Arc<Mutex<Status>>,
    Arc<Mutex<i64>>,
);

/// Returns the folder path for a session under the given base directory.
fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    let len = session_id.len().min(8);
    base.join(&session_id[..len])
}

/// Returns the worktree branch name for a session.
pub(crate) fn session_branch(session_id: &str) -> String {
    let len = session_id.len().min(8);
    format!("agentty/{}", &session_id[..len])
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitMessage {
    body: String,
    title: String,
}

#[derive(Clone)]
struct CommitSessionContext {
    agent: AgentKind,
    folder: PathBuf,
    id: String,
    model: AgentModel,
    output: Arc<Mutex<String>>,
    prompt: String,
}

impl CommitMessage {
    fn as_git_message(&self) -> String {
        if self.body.trim().is_empty() {
            return self.title.clone();
        }

        format!("{}\n\n{}", self.title, self.body)
    }

    fn body_for_log(&self) -> &str {
        if self.body.trim().is_empty() {
            return "(empty)";
        }

        &self.body
    }
}

impl App {
    pub async fn refresh_sessions_if_needed(&mut self) {
        if !self.is_session_refresh_due() {
            return;
        }

        self.session_state.refresh_deadline = Instant::now() + SESSION_REFRESH_INTERVAL;

        let Ok((sessions_row_count, sessions_updated_at_max)) =
            self.db.load_sessions_metadata().await
        else {
            return;
        };
        if sessions_row_count == self.session_state.row_count
            && sessions_updated_at_max == self.session_state.updated_at_max
        {
            return;
        }

        let selected_index = self.session_state.table_state.selected();
        let selected_session_id = selected_index
            .and_then(|index| self.session_state.sessions.get(index))
            .map(|session| session.id.clone());

        let existing_sessions = std::mem::take(&mut self.session_state.sessions);
        self.session_state.sessions = Self::load_sessions(
            &self.base_path,
            &self.db,
            &self.projects,
            &existing_sessions,
        )
        .await;
        self.start_pr_polling_for_pull_request_sessions();
        self.restore_table_selection(selected_session_id.as_deref(), selected_index);
        self.ensure_mode_session_exists();

        self.session_state.row_count = sessions_row_count;
        self.session_state.updated_at_max = sessions_updated_at_max;
    }

    pub fn next_tab(&mut self) {
        self.current_tab = self.current_tab.next();
    }

    pub fn next(&mut self) {
        if self.session_state.sessions.is_empty() {
            return;
        }
        let i = match self.session_state.table_state.selected() {
            Some(i) => {
                if i >= self.session_state.sessions.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.session_state.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.session_state.sessions.is_empty() {
            return;
        }
        let i = match self.session_state.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.session_state.sessions.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.session_state.table_state.select(Some(i));
    }

    /// Creates a blank session with an empty prompt and output.
    ///
    /// Returns the identifier of the newly created session.
    /// The session is created with `New` status and no agent is started —
    /// call [`start_session`] to submit a prompt and launch the agent.
    ///
    /// # Errors
    /// Returns an error if the worktree, session files, or database record
    /// cannot be created.
    pub async fn create_session(&mut self) -> Result<String, String> {
        let base_branch = self
            .git_branch
            .as_deref()
            .ok_or_else(|| "Git branch is required to create a session".to_string())?;

        let session_id = Uuid::new_v4().to_string();
        let folder = session_folder(&self.base_path, &session_id);
        if folder.exists() {
            return Err(format!("Session folder {session_id} already exists"));
        }

        // Create git worktree
        let worktree_branch = session_branch(&session_id);
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

        let data_dir = folder.join(SESSION_DATA_DIR);
        if let Err(err) = std::fs::create_dir_all(&data_dir) {
            self.rollback_failed_session_creation(
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!(
                "Failed to create session metadata directory: {err}"
            ));
        }

        if let Err(err) = self
            .db
            .insert_session(
                &session_id,
                &AgentKind::Gemini.to_string(),
                AgentKind::Gemini.default_model().as_str(),
                base_branch,
                &Status::New.to_string(),
                self.active_project_id,
            )
            .await
        {
            self.rollback_failed_session_creation(
                &folder,
                &repo_root,
                &session_id,
                &worktree_branch,
                false,
            )
            .await;

            return Err(format!("Failed to save session metadata: {err}"));
        }

        let existing_sessions = std::mem::take(&mut self.session_state.sessions);
        self.session_state.sessions = Self::load_sessions(
            &self.base_path,
            &self.db,
            &self.projects,
            &existing_sessions,
        )
        .await;
        self.update_sessions_metadata_cache().await;

        let index = self
            .session_state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .unwrap_or(0);
        self.session_state.table_state.select(Some(index));

        Ok(session_id)
    }

    /// Submits the first prompt for a blank session and starts the agent.
    ///
    /// # Errors
    /// Returns an error if the session is missing or prompt persistence fails.
    pub async fn start_session(&mut self, session_id: &str, prompt: String) -> Result<(), String> {
        let session_index = self
            .session_index_for_id(session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        let session = self
            .session_state
            .sessions
            .get_mut(session_index)
            .ok_or_else(|| "Session not found".to_string())?;

        session.prompt.clone_from(&prompt);

        let title = Self::summarize_title(&prompt);
        session.title = Some(title.clone());
        let _ = self.db.update_session_title(&session.id, &title).await;
        let _ = self.db.update_session_prompt(&session.id, &prompt).await;

        let initial_output = format!(" › {prompt}\n\n");
        Self::append_session_output(
            &session.output,
            &session.folder,
            &self.db,
            &session.id,
            &initial_output,
        )
        .await;

        let output = Arc::clone(&session.output);
        let status = Arc::clone(&session.status);
        let id = session.id.clone();
        let db = self.db.clone();
        let (session_agent, session_model) = Self::resolve_session_agent_and_model(session);
        let folder = session.folder.clone();
        let agent_tx = self.agent_tx.clone();

        let _ = Self::update_status(&status, &db, &id, Status::InProgress).await;

        // Spawn ACP initialization and first prompt in the background so the
        // UI event loop is not blocked while the agent process starts.
        tokio::spawn(async move {
            match AcpSessionHandle::spawn(
                session_agent,
                folder,
                session_model.as_str(),
                agent_tx.clone(),
            )
            .await
            {
                Ok(handle) => {
                    let handle = Arc::new(handle);
                    Self::spawn_acp_prompt_task(handle, prompt, agent_tx, id);
                }
                Err(error) => {
                    let message = format!("\n[Error] Failed to start agent: {error}\n");
                    if let Ok(mut buf) = output.lock() {
                        buf.push_str(&message);
                    }

                    Self::update_status(&status, &db, &id, Status::Review).await;
                }
            }
        });

        Ok(())
    }

    /// Submits a follow-up prompt to an existing session.
    pub fn reply(&mut self, session_id: &str, prompt: &str) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.session_state.sessions.get_mut(session_index) else {
            return;
        };

        // If the session was persisted with a blank prompt (e.g. app closed
        // before first message), treat the first reply as the initial start.
        let is_first_message = session.prompt.is_empty();
        let allowed = session.status() == Status::Review
            || (is_first_message && session.status() == Status::New);
        if !allowed {
            session.append_output("\n[Reply Error] Session must be in review status\n");
            let db = self.db.clone();
            let id = session.id.clone();
            tokio::spawn(async move {
                let _ = db
                    .append_session_output(
                        &id,
                        "\n[Reply Error] Session must be in review status\n",
                    )
                    .await;
            });

            return;
        }
        if is_first_message {
            session.prompt = prompt.to_string();
            let title = Self::summarize_title(prompt);
            session.title = Some(title.clone());
            let db = self.db.clone();
            let id = session.id.clone();
            let prompt = prompt.to_string();
            tokio::spawn(async move {
                let _ = db.update_session_title(&id, &title).await;
                let _ = db.update_session_prompt(&id, &prompt).await;
            });
        }

        let reply_line = format!("\n › {prompt}\n\n");
        session.append_output(&reply_line);
        {
            let db = self.db.clone();
            let id = session.id.clone();
            tokio::spawn(async move {
                let _ = db.append_session_output(&id, &reply_line).await;
            });
        }

        let output = Arc::clone(&session.output);
        let status = Arc::clone(&session.status);
        let id = session.id.clone();
        let db = self.db.clone();
        let (session_agent, session_model) = Self::resolve_session_agent_and_model(session);
        let existing_handle = session.acp_handle.clone();
        let folder = session.folder.clone();
        let prompt = prompt.to_string();
        let agent_tx = self.agent_tx.clone();

        tokio::spawn(async move {
            let handle = if let Some(existing) = existing_handle {
                existing
            } else {
                match AcpSessionHandle::spawn(
                    session_agent,
                    folder,
                    session_model.as_str(),
                    agent_tx.clone(),
                )
                .await
                {
                    Ok(new_handle) => Arc::new(new_handle),
                    Err(error) => {
                        let message = format!("\n[Error] Failed to start agent: {error}\n");
                        if let Ok(mut buf) = output.lock() {
                            buf.push_str(&message);
                        }

                        return;
                    }
                }
            };

            Self::update_status(&status, &db, &id, Status::InProgress).await;

            Self::spawn_acp_prompt_task(handle, prompt, agent_tx, id);
        });
    }

    /// Updates and persists the agent/model pair for a single session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or the model does not belong
    /// to the selected agent.
    pub fn set_session_agent_and_model(
        &mut self,
        session_id: &str,
        session_agent: AgentKind,
        session_model: AgentModel,
    ) -> Result<(), String> {
        if session_model.kind() != session_agent {
            return Err("Model does not belong to selected agent".to_string());
        }

        let Some(session_index) = self.session_index_for_id(session_id) else {
            return Err("Session not found".to_string());
        };
        let Some(session) = self.session_state.sessions.get_mut(session_index) else {
            return Err("Session not found".to_string());
        };

        let agent = session_agent.to_string();
        let model = session_model.as_str().to_string();

        // Invalidate ACP handle when agent kind changes
        if session.agent != agent
            && let Some(handle) = session.acp_handle.take()
        {
            tokio::spawn(async move { handle.shutdown().await });
        }

        session.agent.clone_from(&agent);
        session.model.clone_from(&model);

        let db = self.db.clone();
        let id = session_id.to_string();
        tokio::spawn(async move {
            let _ = db.update_session_agent_and_model(&id, &agent, &model).await;
        });

        Ok(())
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.session_state
            .table_state
            .selected()
            .and_then(|i| self.session_state.sessions.get(i))
    }

    /// Returns the session identifier for the given list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<String> {
        self.session_state
            .sessions
            .get(session_index)
            .map(|session| session.id.clone())
    }

    /// Resolves a stable session identifier to the current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.session_state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
    }

    pub async fn delete_selected_session(&mut self) {
        let Some(i) = self.session_state.table_state.selected() else {
            return;
        };
        if i >= self.session_state.sessions.len() {
            return;
        }
        let mut session = self.session_state.sessions.remove(i);

        // Shut down ACP connection before cleanup
        if let Some(handle) = session.acp_handle.take() {
            tokio::spawn(async move { handle.shutdown().await });
        }

        let _ = self.db.delete_session(&session.id).await;
        self.cancel_pr_polling_for_session(&session.id);
        self.clear_pr_creation_in_flight(&session.id);

        // Remove git worktree and branch if in a git repo
        if self.git_branch.is_some() {
            let branch_name = session_branch(&session.id);

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
        if self.session_state.sessions.is_empty() {
            self.session_state.table_state.select(None);
        } else if i >= self.session_state.sessions.len() {
            self.session_state
                .table_state
                .select(Some(self.session_state.sessions.len() - 1));
        }
        self.update_sessions_metadata_cache().await;
    }

    /// Spawns a background commit for all changes in a session worktree.
    ///
    /// # Errors
    /// Returns an error if the session has no worktree or the git commit
    /// operation fails.
    pub fn spawn_commit_session(&self, session_id: &str) -> Result<(), String> {
        let context = self.build_commit_session_context(session_id)?;

        let session = self
            .session_state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        let status = Arc::clone(&session.status);
        let commit_count = Arc::clone(&session.commit_count);
        let id = session.id.clone();

        let db = self.db.clone();
        let output = Arc::clone(&context.output);
        let folder = context.folder.clone();

        tokio::spawn(async move {
            Self::update_status(&status, &db, &id, Status::Committing).await;

            let result_message = match Self::commit_session_with_context(db.clone(), context).await
            {
                Ok(message) => {
                    if let Ok(mut count) = commit_count.lock() {
                        *count += 1;
                    }
                    let _ = db.increment_commit_count(&id).await;

                    format!("\n[Commit] {message}\n")
                }
                Err(error) => format!("\n[Commit Error] {error}\n"),
            };

            Self::append_session_output(&output, &folder, &db, &id, &result_message).await;

            Self::update_status(&status, &db, &id, Status::Review).await;
        });

        Ok(())
    }

    /// Commits all changes in a session worktree and waits for completion.
    ///
    /// # Errors
    /// Returns an error if the session has no worktree or the git commit
    /// operation fails.
    pub async fn commit_session(&self, session_id: &str) -> Result<String, String> {
        let context = self.build_commit_session_context(session_id)?;

        Self::commit_session_with_context(self.db.clone(), context).await
    }

    /// Squash-merges a reviewed session branch into its base branch.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for merge, required git
    /// metadata is missing, or the merge/cleanup steps fail.
    pub async fn merge_session(&self, session_id: &str) -> Result<String, String> {
        let session = self
            .session_state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        if !matches!(session.status(), Status::Review | Status::PullRequest) {
            return Err("Session must be in review or pull request status".to_string());
        }

        // Read base branch from DB
        let base_branch = self
            .db
            .get_base_branch(&session.id)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        // Find repo root
        let repo_root = git::find_git_repo_root(&self.working_dir)
            .ok_or_else(|| "Failed to find git repository root".to_string())?;

        // Build source branch name
        let source_branch = session_branch(&session.id);

        // Build commit message from session prompt
        let commit_message = format!("Merge session: {}", session.prompt);

        // Perform squash merge
        {
            let repo_root = repo_root.clone();
            let source_branch = source_branch.clone();
            let base_branch = base_branch.clone();
            let commit_message = commit_message.clone();
            tokio::task::spawn_blocking(move || {
                git::squash_merge(&repo_root, &source_branch, &base_branch, &commit_message)
            })
            .await
            .map_err(|e| format!("Join error: {e}"))?
            .map_err(|err| format!("Failed to merge: {err}"))?;
        }

        if !Self::update_status(&session.status, &self.db, &session.id, Status::Done).await {
            return Err("Invalid status transition to Done".to_string());
        }

        self.cancel_pr_polling_for_session(&session.id);
        self.clear_pr_creation_in_flight(&session.id);
        Self::cleanup_merged_session_worktree(
            session.folder.clone(),
            source_branch.clone(),
            Some(repo_root),
        )
        .await
        .map_err(|error| format!("Merged successfully but failed to remove worktree: {error}"))?;

        Ok(format!(
            "Successfully merged {source_branch} into {base_branch}"
        ))
    }

    /// Creates a pull request for a reviewed session branch.
    ///
    /// # Errors
    /// Returns an error if the session is not eligible for PR creation or git
    /// metadata for the worktree is unavailable.
    pub(super) async fn cleanup_merged_session_worktree(
        folder: PathBuf,
        source_branch: String,
        repo_root: Option<PathBuf>,
    ) -> Result<(), String> {
        tokio::task::spawn_blocking(move || {
            let repo_root = repo_root.or_else(|| Self::resolve_repo_root_from_worktree(&folder));

            git::remove_worktree(&folder)?;

            if let Some(repo_root) = repo_root {
                git::delete_branch(&repo_root, &source_branch)?;
            }

            let _ = std::fs::remove_dir_all(&folder);

            Ok(())
        })
        .await
        .map_err(|error| format!("Join error: {error}"))?
    }

    pub(super) fn resolve_repo_root_from_worktree(worktree_path: &Path) -> Option<PathBuf> {
        let git_path = worktree_path.join(".git");
        if git_path.is_dir() {
            return Some(worktree_path.to_path_buf());
        }

        if !git_path.is_file() {
            return None;
        }

        let git_file = std::fs::read_to_string(git_path).ok()?;
        let git_dir_line = git_file.lines().find(|line| line.starts_with("gitdir:"))?;
        let git_dir = PathBuf::from(git_dir_line.trim_start_matches("gitdir:").trim());
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            worktree_path.join(git_dir)
        };

        git_dir.parent()?.parent()?.parent().map(Path::to_path_buf)
    }

    async fn build_commit_message(
        session_folder: PathBuf,
        session_prompt: String,
        session_agent: AgentKind,
        session_model: AgentModel,
    ) -> CommitMessage {
        match Self::summarize_commit_message_via_agent(session_folder, session_agent, session_model)
            .await
        {
            Ok(message) => message,
            Err(_) => Self::fallback_commit_message(&session_prompt),
        }
    }

    async fn summarize_commit_message_via_agent(
        session_folder: PathBuf,
        session_agent: AgentKind,
        session_model: AgentModel,
    ) -> Result<CommitMessage, String> {
        let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = AcpSessionHandle::spawn(
            session_agent,
            session_folder,
            session_model.as_str(),
            agent_tx,
        )
        .await?;

        handle.prompt(&COMMIT_SUMMARY_PROMPT).await?;
        handle.shutdown().await;

        let mut text = String::new();
        while let Some(event) = agent_rx.recv().await {
            if let crate::app::AgentEvent::Output { text: chunk, .. } = event {
                text.push_str(&chunk);
            }
        }

        Self::parse_agent_commit_message(&text)
            .ok_or_else(|| "Failed to parse commit title and body from agent output".to_string())
    }

    fn parse_agent_commit_message(output: &str) -> Option<CommitMessage> {
        let block = Self::extract_commit_message_block(output)?;
        let lines = block.lines().collect::<Vec<_>>();
        let title_index = lines
            .iter()
            .position(|line| line.trim_start().to_ascii_lowercase().starts_with("title:"))?;
        let title = lines
            .get(title_index)?
            .trim_start()
            .split_once(':')
            .map(|(_, value)| value.trim())
            .unwrap_or_default()
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if title.is_empty() {
            return None;
        }

        let body = if let Some(body_index) = lines[title_index + 1..]
            .iter()
            .position(|line| line.trim_start().to_ascii_lowercase().starts_with("body:"))
            .map(|offset| title_index + 1 + offset)
        {
            let remainder = lines[body_index]
                .trim_start()
                .split_once(':')
                .map(|(_, value)| value.trim())
                .unwrap_or_default()
                .to_string();
            let trailing_lines = lines[body_index + 1..]
                .iter()
                .map(|line| line.trim_end())
                .collect::<Vec<_>>();
            let trailing_body = trailing_lines.join("\n").trim().to_string();
            if trailing_body.is_empty() {
                remainder
            } else if remainder.is_empty() {
                trailing_body
            } else {
                format!("{remainder}\n{trailing_body}")
            }
        } else {
            String::new()
        };

        Some(CommitMessage { body, title })
    }

    fn extract_commit_message_block(output: &str) -> Option<String> {
        let start_index = output.find(COMMIT_SUMMARY_BEGIN)?;
        let after_start = &output[start_index + COMMIT_SUMMARY_BEGIN.len()..];
        let end_index = after_start.find(COMMIT_SUMMARY_END)?;
        let block = after_start[..end_index].trim().to_string();

        if block.is_empty() {
            return None;
        }

        Some(block)
    }

    fn fallback_commit_message(session_prompt: &str) -> CommitMessage {
        let prompt_title = Self::summarize_title(session_prompt);
        let title = if prompt_title.is_empty() {
            "Update session worktree".to_string()
        } else {
            format!("Update {}", prompt_title.trim())
        };

        let body = "- Commit current session worktree changes.".to_string();

        CommitMessage { body, title }
    }

    fn build_commit_session_context(
        &self,
        session_id: &str,
    ) -> Result<CommitSessionContext, String> {
        let session = self
            .session_state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        let (agent, model) = Self::resolve_session_agent_and_model(session);

        Ok(CommitSessionContext {
            agent,
            folder: session.folder.clone(),
            id: session.id.clone(),
            model,
            output: Arc::clone(&session.output),
            prompt: session.prompt.clone(),
        })
    }

    async fn commit_session_with_context(
        db: Database,
        context: CommitSessionContext,
    ) -> Result<String, String> {
        // Verify this session has a git worktree via DB
        if db.get_base_branch(&context.id).await?.is_none() {
            return Err("No git worktree for this session".to_string());
        }

        let commit_message = Self::build_commit_message(
            context.folder.clone(),
            context.prompt,
            context.agent,
            context.model,
        )
        .await;

        let git_commit_message = commit_message.as_git_message();

        // Commit all changes in the worktree and capture the resulting hash.
        let folder = context.folder;
        let commit_hash = tokio::task::spawn_blocking(move || {
            git::commit_all(&folder, &git_commit_message)?;

            git::head_short_hash(&folder)
        })
        .await
        .map_err(|error| format!("Join error: {error}"))?
        .map_err(|error| format!("Failed to commit: {error}"))?;

        Ok(format!(
            "committed with hash `{commit_hash}`\ntitle: `{}`\nbody:\n{}",
            commit_message.title,
            commit_message.body_for_log()
        ))
    }

    fn reply_with_backend(
        &mut self,
        session_id: &str,
        prompt: &str,
        backend: &dyn AgentBackend,
        model: &str,
    ) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.session_state.sessions.get_mut(session_index) else {
            return;
        };

        // If the session was persisted with a blank prompt (e.g. app closed
        // before first message), treat the first reply as the initial start.
        let is_first_message = session.prompt.is_empty();
        let allowed = session.status() == Status::Review
            || (is_first_message && session.status() == Status::New);
        if !allowed {
            session.append_output("\n[Reply Error] Session must be in review status\n");
            let db = self.db.clone();
            let id = session.id.clone();
            tokio::spawn(async move {
                let _ = db
                    .append_session_output(
                        &id,
                        "\n[Reply Error] Session must be in review status\n",
                    )
                    .await;
            });

            return;
        }
        if is_first_message {
            session.prompt = prompt.to_string();
            let title = Self::summarize_title(prompt);
            session.title = Some(title.clone());
            let db = self.db.clone();
            let id = session.id.clone();
            let prompt = prompt.to_string();
            tokio::spawn(async move {
                let _ = db.update_session_title(&id, &title).await;
                let _ = db.update_session_prompt(&id, &prompt).await;
            });
        }

        let reply_line = format!("\n › {prompt}\n\n");
        session.append_output(&reply_line);
        {
            let db = self.db.clone();
            let id = session.id.clone();
            tokio::spawn(async move {
                let _ = db.append_session_output(&id, &reply_line).await;
            });
        }

        let folder = session.folder.clone();
        let output = Arc::clone(&session.output);
        let status = Arc::clone(&session.status);
        let id = session.id.clone();
        let db = self.db.clone();
        let agent = session
            .agent
            .parse::<AgentKind>()
            .unwrap_or(AgentKind::Gemini);

        {
            let status = Arc::clone(&status);
            let db = db.clone();
            let id = id.clone();
            tokio::spawn(async move {
                Self::update_status(&status, &db, &id, Status::InProgress).await;
            });
        }

        let cmd = if is_first_message {
            backend.build_start_command(&folder, prompt, model)
        } else {
            backend.build_resume_command(&folder, prompt, model)
        };
        Self::spawn_session_task(folder, cmd, output, status, db, id, agent);
    }

    async fn rollback_failed_session_creation(
        &self,
        folder: &Path,
        repo_root: &Path,
        session_id: &str,
        worktree_branch: &str,
        session_saved: bool,
    ) {
        if session_saved {
            let _ = self.db.delete_session(session_id).await;
        }

        {
            let folder = folder.to_path_buf();
            let repo_root = repo_root.to_path_buf();
            let worktree_branch = worktree_branch.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                let _ = git::remove_worktree(&folder);
                let _ = git::delete_branch(&repo_root, &worktree_branch);
            })
            .await;
        }

        let _ = std::fs::remove_dir_all(folder);
    }

    fn is_session_refresh_due(&self) -> bool {
        Instant::now() >= self.session_state.refresh_deadline
    }

    fn restore_table_selection(
        &mut self,
        selected_session_id: Option<&str>,
        selected_index: Option<usize>,
    ) {
        if self.session_state.sessions.is_empty() {
            self.session_state.table_state.select(None);

            return;
        }

        if let Some(session_id) = selected_session_id
            && let Some(index) = self
                .session_state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
        {
            self.session_state.table_state.select(Some(index));

            return;
        }

        let restored_index =
            selected_index.map(|index| index.min(self.session_state.sessions.len() - 1));
        self.session_state.table_state.select(restored_index);
    }

    fn ensure_mode_session_exists(&mut self) {
        let mode_session_id = match &self.mode {
            AppMode::Prompt { session_id, .. }
            | AppMode::View { session_id, .. }
            | AppMode::Diff { session_id, .. } => Some(session_id),
            _ => None,
        };
        let Some(session_id) = mode_session_id else {
            return;
        };
        if self.session_index_for_id(session_id).is_none() {
            self.mode = AppMode::List;
        }
    }

    pub(super) async fn update_sessions_metadata_cache(&mut self) {
        if let Ok((sessions_row_count, sessions_updated_at_max)) =
            self.db.load_sessions_metadata().await
        {
            self.session_state.row_count = sessions_row_count;
            self.session_state.updated_at_max = sessions_updated_at_max;
        }
    }

    pub(crate) async fn append_output_for_session(&self, session_id: &str, output: &str) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.session_state.sessions.get(session_index) else {
            return;
        };

        Self::append_session_output(
            &session.output,
            &session.folder,
            &self.db,
            &session.id,
            output,
        )
        .await;
    }

    pub(crate) async fn finish_session_turn(
        &self,
        session_id: &str,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    ) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.session_state.sessions.get(session_index) else {
            return;
        };

        if input_tokens.is_some() || output_tokens.is_some() {
            let stats = SessionStats {
                input_tokens,
                output_tokens,
            };
            let _ = self.db.update_session_stats(&session.id, &stats).await;
        }

        let snapshot = session
            .output
            .lock()
            .map(|buf| buf.clone())
            .unwrap_or_default();
        let _ = self.db.replace_session_output(&session.id, &snapshot).await;

        Self::update_status(&session.status, &self.db, &session.id, Status::Review).await;
    }

    pub(crate) async fn fail_session_turn(&self, session_id: &str, error: &str) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.session_state.sessions.get(session_index) else {
            return;
        };

        let message = format!("\n[Error] {error}\n");
        Self::append_session_output(
            &session.output,
            &session.folder,
            &self.db,
            &session.id,
            &message,
        )
        .await;

        Self::update_status(&session.status, &self.db, &session.id, Status::Review).await;
    }

    pub(super) async fn load_sessions(
        base: &Path,
        db: &Database,
        projects: &[Project],
        existing_sessions: &[Session],
    ) -> Vec<Session> {
        let project_names: HashMap<i64, String> = projects
            .iter()
            .filter_map(|project| {
                let name = project.path.file_name()?.to_string_lossy().to_string();
                Some((project.id, name))
            })
            .collect();
        let existing_sessions_by_name: HashMap<String, SessionHandles> = existing_sessions
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    (
                        session.acp_handle.clone(),
                        Arc::clone(&session.output),
                        Arc::clone(&session.status),
                        Arc::clone(&session.commit_count),
                    ),
                )
            })
            .collect();

        let db_rows = db.load_sessions().await.unwrap_or_default();
        let sessions: Vec<Session> = db_rows
            .into_iter()
            .filter_map(|row| {
                let folder = session_folder(base, &row.id);
                let status = row.status.parse::<Status>().unwrap_or(Status::Done);
                let keep_without_folder = matches!(status, Status::Done | Status::Canceled);
                if !folder.is_dir() && !keep_without_folder {
                    return None;
                }

                let session_agent = row.agent.parse::<AgentKind>().unwrap_or(AgentKind::Gemini);
                let session_model = session_agent
                    .parse_model(&row.model)
                    .unwrap_or_else(|| session_agent.default_model())
                    .as_str()
                    .to_string();
                let project_name = row
                    .project_id
                    .and_then(|id| project_names.get(&id))
                    .cloned()
                    .unwrap_or_default();
                let (acp_handle, output, status, commit_count) = if let Some((
                    existing_handle,
                    existing_output,
                    existing_status,
                    existing_commit_count,
                )) =
                    existing_sessions_by_name.get(&row.id)
                {
                    if let Ok(mut output_buffer) = existing_output.lock() {
                        output_buffer.clone_from(&row.output);
                    }
                    if let Ok(mut status_value) = existing_status.lock() {
                        *status_value = status;
                    }
                    if let Ok(mut count) = existing_commit_count.lock() {
                        *count = row.commit_count;
                    }

                    (
                        existing_handle.clone(),
                        Arc::clone(existing_output),
                        Arc::clone(existing_status),
                        Arc::clone(existing_commit_count),
                    )
                } else {
                    (
                        None,
                        Arc::new(Mutex::new(row.output.clone())),
                        Arc::new(Mutex::new(status)),
                        Arc::new(Mutex::new(row.commit_count)),
                    )
                };

                Some(Session {
                    acp_handle,
                    agent: row.agent,
                    commit_count,
                    folder,
                    id: row.id,
                    model: session_model,
                    output,
                    project_name,
                    prompt: row.prompt,
                    stats: SessionStats {
                        input_tokens: row.input_tokens,
                        output_tokens: row.output_tokens,
                    },
                    status,
                    title: row.title,
                })
            })
            .collect();

        sessions
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::*;
    use crate::model::Tab;

    async fn new_test_app(path: PathBuf) -> App {
        let working_dir = PathBuf::from("/tmp/test");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        App::new(path, working_dir, None, db).await
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
            db,
        )
        .await
    }

    fn add_manual_session(app: &mut App, base_path: &Path, id: &str, prompt: &str) {
        let folder = session_folder(base_path, id);
        let data_dir = folder.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        app.session_state.sessions.push(Session {
            acp_handle: None,
            agent: "gemini".to_string(),
            commit_count: Arc::new(Mutex::new(0)),
            folder,
            id: id.to_string(),
            model: "gemini-3-flash-preview".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            project_name: String::new(),
            prompt: prompt.to_string(),
            stats: SessionStats::default(),
            status: Arc::new(Mutex::new(Status::Review)),
            title: Some(App::summarize_title(prompt)),
        });
        if app.session_state.table_state.selected().is_none() {
            app.session_state.table_state.select(Some(0));
        }
    }

    /// Helper: creates a session with a prompt populated (simulates a started
    /// session without requiring an actual ACP agent binary).
    async fn create_and_start_session(app: &mut App, prompt: &str) {
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing session index");
        let session = &mut app.session_state.sessions[session_index];
        session.prompt = prompt.to_string();
        session.title = Some(App::summarize_title(prompt));
        let initial_output = format!(" › {prompt}\n\n");
        session.append_output(&initial_output);
        if let Ok(mut status) = session.status.lock() {
            *status = Status::Review;
        }
        let _ = app.db.update_session_prompt(&session_id, prompt).await;
        let _ = app
            .db
            .append_session_output(&session_id, &initial_output)
            .await;
    }

    async fn wait_for_status(session: &Session, expected: Status) {
        for _ in 0..40 {
            if session.status() == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        panic!("session did not reach status {expected}");
    }

    #[tokio::test]
    async fn test_new_app_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
        assert_eq!(app.session_state.table_state.selected(), None);
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
    async fn test_navigation() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        create_and_start_session(&mut app, "B").await;

        // Act & Assert (Next)
        app.session_state.table_state.select(Some(0));
        app.next();
        assert_eq!(app.session_state.table_state.selected(), Some(1));
        app.next();
        assert_eq!(app.session_state.table_state.selected(), Some(0)); // Loop back

        // Act & Assert (Previous)
        app.previous();
        assert_eq!(app.session_state.table_state.selected(), Some(1)); // Loop back
        app.previous();
        assert_eq!(app.session_state.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_navigation_empty() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        app.next();
        assert_eq!(app.session_state.table_state.selected(), None);

        app.previous();
        assert_eq!(app.session_state.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_navigation_recovery() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;

        // Act & Assert — next recovers from None
        app.session_state.table_state.select(None);
        app.next();
        assert_eq!(app.session_state.table_state.selected(), Some(0));

        // Act & Assert — previous recovers from None
        app.session_state.table_state.select(None);
        app.previous();
        assert_eq!(app.session_state.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_create_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;

        // Act
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Assert — blank session
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(session_id, app.session_state.sessions[0].id);
        assert!(app.session_state.sessions[0].prompt.is_empty());
        assert_eq!(app.session_state.sessions[0].title, None);
        assert_eq!(app.session_state.sessions[0].display_title(), "No title");
        assert_eq!(app.session_state.sessions[0].status(), Status::New);
        assert_eq!(app.session_state.table_state.selected(), Some(0));
        assert_eq!(app.session_state.sessions[0].agent, "gemini");

        // Check filesystem
        let session_dir = &app.session_state.sessions[0].folder;
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        assert!(session_dir.exists());
        assert!(data_dir.is_dir());

        // Check DB
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert_eq!(db_sessions.len(), 1);
        assert_eq!(db_sessions[0].agent, "gemini");
        assert_eq!(db_sessions[0].base_branch, "main");
        assert_eq!(db_sessions[0].status, "New");
    }

    #[tokio::test]
    async fn test_start_session_persists_metadata() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Hello").await;

        // Assert
        assert_eq!(app.session_state.sessions[0].prompt, "Hello");
        assert_eq!(
            app.session_state.sessions[0].title,
            Some("Hello".to_string())
        );
        let output = app.session_state.sessions[0]
            .output
            .lock()
            .expect("failed to lock output")
            .clone();
        assert!(output.contains("Hello"));
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert_eq!(db_sessions[0].prompt, "Hello");
        assert_eq!(db_sessions[0].output, " › Hello\n\n");
    }

    #[tokio::test]
    async fn test_esc_deletes_blank_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing session index");
        let session_folder = app.session_state.sessions[session_index].folder.clone();
        assert!(session_folder.exists());

        // Act — simulate Esc: delete the blank session
        app.delete_selected_session().await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
        assert!(!session_folder.exists());
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert!(db_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_reply() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Initial").await;
        let session_id = app.session_state.sessions[0].id.clone();

        // Act
        app.reply(&session_id, "Reply");

        // Assert
        let session = &app.session_state.sessions[0];
        let output = session.output.lock().expect("failed to lock output");
        assert!(output.contains("Reply"));
    }

    #[tokio::test]
    async fn test_selected_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Test").await;

        // Act & Assert
        assert!(app.selected_session().is_some());

        app.session_state.table_state.select(None);
        assert!(app.selected_session().is_none());
    }

    #[tokio::test]
    async fn test_delete_session() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "A").await;
        let session_folder = app.session_state.sessions[0].folder.clone();

        // Act
        app.delete_selected_session().await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
        assert_eq!(app.session_state.table_state.selected(), None);
        assert!(!session_folder.exists());
        let db_sessions = app.db.load_sessions().await.expect("failed to load");
        assert!(db_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_delete_selected_session_edge_cases() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "1").await;
        create_and_start_session(&mut app, "2").await;

        // Act & Assert — index out of bounds
        app.session_state.table_state.select(Some(99));
        app.delete_selected_session().await;
        assert_eq!(app.session_state.sessions.len(), 2);

        // Act & Assert — None selected
        app.session_state.table_state.select(None);
        app.delete_selected_session().await;
        assert_eq!(app.session_state.sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_last_session_update_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "1").await;
        create_and_start_session(&mut app, "2").await;

        // Act & Assert — delete last item
        app.session_state.table_state.select(Some(1));
        app.delete_selected_session().await;
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(app.session_state.table_state.selected(), Some(0));

        // Act & Assert — delete remaining item
        app.delete_selected_session().await;
        assert!(app.session_state.sessions.is_empty());
        assert_eq!(app.session_state.table_state.selected(), None);
    }

    #[tokio::test]
    async fn test_load_existing_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "12345678",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        let session_dir = dir.path().join("12345678");
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir(&session_dir).expect("failed to create session dir");
        std::fs::create_dir(&data_dir).expect("failed to create data dir");
        db.update_session_prompt("12345678", "Existing")
            .await
            .expect("failed to update prompt");
        db.append_session_output("12345678", "Output")
            .await
            .expect("failed to update output");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(app.session_state.sessions[0].id, "12345678");
        assert_eq!(app.session_state.sessions[0].prompt, "Existing");
        let output = app.session_state.sessions[0]
            .output
            .lock()
            .expect("failed to lock output")
            .clone();
        assert_eq!(output, "Output");
        assert_eq!(app.session_state.sessions[0].agent, "claude");
        assert_eq!(app.session_state.table_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn test_load_existing_sessions_ordered_by_updated_at_desc() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha000",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
        db.insert_session(
            "beta0000",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta0000");

        sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(1_i64)
        .bind("alpha000")
        .execute(db.pool())
        .await
        .expect("failed to update alpha000 timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(2_i64)
        .bind("beta0000")
        .execute(db.pool())
        .await
        .expect("failed to update beta0000 timestamp");

        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert
        let session_names: Vec<&str> = app
            .session_state
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        assert_eq!(session_names, vec!["beta0000", "alpha000"]);
    }

    #[tokio::test]
    async fn test_refresh_sessions_if_needed_reloads_and_preserves_selection() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha000",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
        db.insert_session(
            "beta0000",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta0000");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 1
WHERE id = 'alpha000'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha000 timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 2
WHERE id = 'beta0000'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta0000 timestamp");
        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;
        app.session_state.table_state.select(Some(1));

        // Act
        tokio::time::sleep(Duration::from_secs(1)).await;
        app.db
            .update_session_status("alpha000", "Done")
            .await
            .expect("failed to update session status");
        tokio::time::sleep(Duration::from_millis(600)).await;
        app.refresh_sessions_if_needed().await;

        // Assert
        assert_eq!(app.session_state.sessions[0].id, "alpha000");
        let selected_index = app
            .session_state
            .table_state
            .selected()
            .expect("missing selection");
        assert_eq!(app.session_state.sessions[selected_index].id, "alpha000");
    }

    #[tokio::test]
    async fn test_refresh_sessions_if_needed_remaps_view_mode_index() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "alpha000",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
        db.insert_session(
            "beta0000",
            "claude",
            "claude-opus-4-6",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta0000");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 1
WHERE id = 'alpha000'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set alpha000 timestamp");
        sqlx::query(
            r"
UPDATE session
SET updated_at = 2
WHERE id = 'beta0000'
",
        )
        .execute(db.pool())
        .await
        .expect("failed to set beta0000 timestamp");
        for session_id in ["alpha000", "beta0000"] {
            let session_dir = session_folder(dir.path(), session_id);
            let data_dir = session_dir.join(SESSION_DATA_DIR);
            std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        }
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;
        let selected_session_id = app.session_state.sessions[1].id.clone();
        app.mode = AppMode::View {
            session_id: selected_session_id.clone(),
            scroll_offset: None,
        };

        // Act
        tokio::time::sleep(Duration::from_secs(1)).await;
        app.db
            .update_session_status("alpha000", "Done")
            .await
            .expect("failed to update session status");
        tokio::time::sleep(Duration::from_millis(600)).await;
        app.refresh_sessions_if_needed().await;

        // Assert
        assert_eq!(app.session_state.sessions[0].id, "alpha000");
        match app.mode {
            AppMode::View { session_id, .. } => assert_eq!(session_id, selected_session_id),
            _ => panic!("expected view mode"),
        }
    }

    #[tokio::test]
    async fn test_load_sessions_invalid_path() {
        // Arrange
        let path = PathBuf::from("/invalid/path/that/does/not/exist");

        // Act
        let app = new_test_app(path).await;

        // Assert
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_load_done_session_without_folder_kept() {
        // Arrange — DB has a terminal row but no matching folder on disk
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "missing01",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert — terminal session is kept even after folder cleanup
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(app.session_state.sessions[0].id, "missing01");
        assert_eq!(app.session_state.sessions[0].status(), Status::Done);
    }

    #[tokio::test]
    async fn test_load_in_progress_session_without_folder_skipped() {
        // Arrange — DB has a non-terminal row but no matching folder on disk
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "missing02",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert");

        // Act
        let app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            None,
            db,
        )
        .await;

        // Assert — non-terminal session is skipped because folder doesn't exist
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_reply_rejects_non_review_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");
        if let Ok(mut status) = app.session_state.sessions[0].status.lock() {
            *status = Status::InProgress;
        }

        // Act
        app.reply("manual01", "Follow up");

        // Assert — error appended to output
        let output = app.session_state.sessions[0]
            .output
            .lock()
            .expect("failed to lock output")
            .clone();
        assert!(output.contains("[Reply Error]"));
    }

    #[tokio::test]
    async fn test_next_tab() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert_eq!(app.current_tab, Tab::Sessions);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Stats);
        app.next_tab();
        assert_eq!(app.current_tab, Tab::Sessions);
    }

    #[tokio::test]
    async fn test_create_session_without_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.create_session().await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Git branch is required")
        );
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_create_session_with_git_no_actual_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let result = app.create_session().await;

        // Assert - should fail because git repo doesn't actually exist
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("git repository root")
        );
    }

    #[tokio::test]
    async fn test_create_session_cleans_up_on_error() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/test"),
            Some("main".to_string()),
            db,
        )
        .await;

        // Act
        let result = app.create_session().await;

        // Assert - session should not be created
        assert!(result.is_err());
        assert_eq!(app.session_state.sessions.len(), 0);

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
        assert_eq!(app.session_state.sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_commit_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.commit_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_commit_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.commit_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[test]
    fn test_parse_agent_commit_message_reads_title_and_body_markers() {
        // Arrange
        let output = "BEGIN_COMMIT_MESSAGE\nTITLE: Update session chat commit flow\nBODY:\n- Ask \
                      current agent for commit title/body.\n- Keep chat logs \
                      high-level.\nEND_COMMIT_MESSAGE";

        // Act
        let message = App::parse_agent_commit_message(output).expect("should parse");

        // Assert
        assert_eq!(message.title, "Update session chat commit flow");
        assert_eq!(
            message.body,
            "- Ask current agent for commit title/body.\n- Keep chat logs high-level."
        );
    }

    #[test]
    fn test_parse_agent_commit_message_returns_none_when_title_is_empty() {
        // Arrange
        let output = "BEGIN_COMMIT_MESSAGE\nTITLE:\nBODY:\n- Item\nEND_COMMIT_MESSAGE";

        // Act
        let message = App::parse_agent_commit_message(output);

        // Assert
        assert!(message.is_none());
    }

    #[test]
    fn test_parse_agent_commit_message_ignores_noise_outside_block() {
        // Arrange
        let output = "Loaded cached credentials.\nBEGIN_COMMIT_MESSAGE\nTITLE: Improve commit \
                      summary parsing\nBODY:\n- Parse only the commit message block.\n- Ignore \
                      CLI noise outside markers.\nEND_COMMIT_MESSAGE\nError executing tool \
                      run_shell_command: Tool not found.";

        // Act
        let message = App::parse_agent_commit_message(output).expect("should parse");

        // Assert
        assert_eq!(message.title, "Improve commit summary parsing");
        assert_eq!(
            message.body,
            "- Parse only the commit message block.\n- Ignore CLI noise outside markers."
        );
    }

    #[test]
    fn test_fallback_commit_message_uses_prompt_summary() {
        // Arrange
        let prompt = "Implement commit flow updates";

        // Act
        let message = App::fallback_commit_message(prompt);

        // Assert
        assert!(message.title.contains("Implement commit flow updates"));
        assert!(
            message
                .body
                .contains("Commit current session worktree changes")
        );
    }

    #[tokio::test]
    async fn test_merge_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.merge_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_merge_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.merge_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_merge_session_removes_worktree_and_branch_after_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Local merge cleanup").await;
        wait_for_status(&app.session_state.sessions[0], Status::Review).await;
        let session_id = app.session_state.sessions[0].id.clone();
        let session_folder = app.session_state.sessions[0].folder.clone();
        std::fs::write(session_folder.join("session-change.txt"), "change")
            .expect("failed to write worktree change");
        git::commit_all(&session_folder, "Test merge commit")
            .expect("failed to commit session changes");
        let branch_name = session_branch(&session_id);

        // Act
        let result = app.merge_session(&session_id).await;

        // Assert
        assert!(result.is_ok(), "merge should succeed: {:?}", result.err());
        assert_eq!(app.session_state.sessions[0].status(), Status::Done);
        assert!(!session_folder.exists(), "worktree should be removed");

        let branch_output = Command::new("git")
            .args(["branch", "--list", &branch_name])
            .current_dir(dir.path())
            .output()
            .expect("failed to list branches");
        let branches = String::from_utf8_lossy(&branch_output.stdout);
        assert!(
            branches.trim().is_empty(),
            "branch should be removed after merge"
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_no_git() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");

        // Act
        let result = app.create_pr_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("No git worktree")
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_requires_review_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        add_manual_session(&mut app, dir.path(), "manual01", "Test");
        if let Ok(mut status) = app.session_state.sessions[0].status.lock() {
            *status = Status::Done;
        }

        // Act
        let result = app.create_pr_session("manual01").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("must be in review")
        );
    }

    #[tokio::test]
    async fn test_create_pr_session_invalid_id() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.create_pr_session("missing").await;

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Session not found")
        );
    }

    #[tokio::test]
    async fn test_cleanup_merged_session_worktree_without_repo_hint() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let worktree_folder = dir.path().join("merged-worktree");
        let branch_name = "agentty/cleanup123";
        git::create_worktree(dir.path(), &worktree_folder, branch_name, "main")
            .expect("failed to create worktree");
        assert!(
            worktree_folder.exists(),
            "worktree should exist before cleanup"
        );

        // Act
        let result = App::cleanup_merged_session_worktree(
            worktree_folder.clone(),
            branch_name.to_string(),
            None,
        )
        .await;

        // Assert
        assert!(result.is_ok(), "cleanup should succeed: {:?}", result.err());
        assert!(
            !worktree_folder.exists(),
            "worktree should be removed after cleanup"
        );

        let branch_output = Command::new("git")
            .args(["branch", "--list", branch_name])
            .current_dir(dir.path())
            .output()
            .expect("failed to list branches");
        let branches = String::from_utf8_lossy(&branch_output.stdout);
        assert!(
            branches.trim().is_empty(),
            "branch should be removed after cleanup"
        );
    }

    #[tokio::test]
    async fn test_active_project_id_getter() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert
        assert!(app.active_project_id() > 0);
    }

    #[tokio::test]
    async fn test_projects_auto_registered() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let app = new_test_app(dir.path().to_path_buf()).await;

        // Act & Assert — cwd auto-registered as a project
        assert!(
            app.projects
                .iter()
                .any(|project| project.path == Path::new("/tmp/test"))
        );
    }

    #[tokio::test]
    async fn test_switch_project() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        let other_id = app
            .db
            .upsert_project("/tmp/other", Some("develop"))
            .await
            .expect("failed to upsert");

        // Act
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert
        assert_eq!(app.active_project_id(), other_id);
        assert_eq!(app.working_dir(), &PathBuf::from("/tmp/other"));
        assert_eq!(app.git_branch(), Some("develop"));
        assert!(app.session_state.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_switch_project_keeps_existing_pr_polling_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;
        let other_id = app
            .db
            .upsert_project("/tmp/other", None)
            .await
            .expect("failed to upsert");
        if let Ok(mut polling) = app.pr_poll_cancel.lock() {
            polling.insert("manual01".to_string(), Arc::new(AtomicBool::new(false)));
        }

        // Act
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert
        let polling = app.pr_poll_cancel.lock().expect("failed to lock polling");
        assert!(polling.contains_key("manual01"));
    }

    #[tokio::test]
    async fn test_switch_project_not_found() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app(dir.path().to_path_buf()).await;

        // Act
        let result = app.switch_project(999).await;

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("expected missing project error");
        assert!(error.contains("Project not found"));
    }

    #[tokio::test]
    async fn test_switch_project_shows_all_sessions() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        create_and_start_session(&mut app, "Session A").await;
        assert_eq!(app.session_state.sessions.len(), 1);

        let other_id = app
            .db
            .upsert_project("/tmp/other", None)
            .await
            .expect("failed to upsert");

        // Act — switch to other project
        app.switch_project(other_id)
            .await
            .expect("failed to switch");

        // Assert — all sessions still visible after switching projects
        assert_eq!(app.session_state.sessions.len(), 1);
        assert_eq!(app.active_project_id(), other_id);
    }

    #[tokio::test]
    async fn test_create_session_scoped_to_project() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_git(dir.path()).await;
        let project_id = app.active_project_id();

        // Act
        app.create_session()
            .await
            .expect("failed to create session");

        // Assert — session belongs to the active project
        let sessions = app.db.load_sessions().await.expect("failed to load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_discover_sibling_projects() {
        // Arrange — create a parent dir with two git repo subdirectories
        let parent = tempdir().expect("failed to create temp dir");
        let repo_a = parent.path().join("repo_a");
        let repo_b = parent.path().join("repo_b");
        let not_repo = parent.path().join("plain_dir");
        std::fs::create_dir(&repo_a).expect("failed to create repo_a");
        std::fs::create_dir(&repo_b).expect("failed to create repo_b");
        std::fs::create_dir(&not_repo).expect("failed to create plain_dir");
        setup_test_git_repo(&repo_a);
        setup_test_git_repo(&repo_b);

        // Act — launch app from repo_a
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            parent.path().to_path_buf(),
            repo_a.clone(),
            Some("main".to_string()),
            db,
        )
        .await;

        // Assert — repo_a (cwd) and repo_b (sibling) are discovered, plain_dir is not
        assert_eq!(app.projects.len(), 2);
        let paths: Vec<&Path> = app.projects.iter().map(|p| p.path.as_path()).collect();
        assert!(paths.contains(&repo_a.as_path()));
        assert!(paths.contains(&repo_b.as_path()));
    }

    #[test]
    fn test_summarize_title_short() {
        // Arrange & Act & Assert
        assert_eq!(App::summarize_title("Fix bug"), "Fix bug");
    }

    #[test]
    fn test_summarize_title_exact_30() {
        // Arrange
        let prompt = "a23456789012345678901234567890"; // exactly 30 chars

        // Act & Assert
        assert_eq!(prompt.len(), 30);
        assert_eq!(App::summarize_title(prompt), prompt);
    }

    #[test]
    fn test_summarize_title_long_with_space() {
        // Arrange
        let prompt = "Fix the authentication bug in the login flow";

        // Act
        let title = App::summarize_title(prompt);

        // Assert
        assert_eq!(title, "Fix the authentication bug in…");
        assert!(title.len() <= 34); // 30 chars + ellipsis (3 bytes)
    }

    #[test]
    fn test_summarize_title_long_without_spaces() {
        // Arrange
        let prompt = "abcdefghijklmnopqrstuvwxyz1234567890";

        // Act
        let title = App::summarize_title(prompt);

        // Assert
        assert_eq!(title, "abcdefghijklmnopqrstuvwxyz1234…");
    }

    #[test]
    fn test_summarize_title_multiline() {
        // Arrange
        let prompt = "First line\nSecond line\nThird line";

        // Act
        let title = App::summarize_title(prompt);

        // Assert
        assert_eq!(title, "First line");
    }

    #[test]
    fn test_summarize_title_empty() {
        // Arrange & Act & Assert
        assert_eq!(App::summarize_title(""), "");
    }

    // --- session_folder / session_branch ---

    #[test]
    fn test_session_folder_uses_first_8_chars() {
        // Arrange
        let base = Path::new("/home/user/.agentty/wt");
        let session_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        // Act
        let folder = session_folder(base, session_id);

        // Assert
        assert_eq!(folder, PathBuf::from("/home/user/.agentty/wt/a1b2c3d4"));
    }

    #[test]
    fn test_session_branch_uses_first_8_chars() {
        // Arrange
        let session_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        // Act
        let branch = session_branch(session_id);

        // Assert
        assert_eq!(branch, "agentty/a1b2c3d4");
    }
}
