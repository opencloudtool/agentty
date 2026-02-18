//! Session task execution helpers for process running, output capture, and
//! status persistence.

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncRead};
use tokio::sync::mpsc;

use crate::agent::AgentKind;
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, effective_permission_mode,
    format_detail_lines, run_agent_assist,
};
use crate::app::{AppEvent, SessionManager};
use crate::db::Database;
use crate::git;
use crate::model::{PermissionMode, Status};

const AUTO_COMMIT_ASSIST_PROMPT_TEMPLATE: &str =
    include_str!("../../resources/auto_commit_assist_prompt.md");
const AUTO_COMMIT_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 10,
    max_identical_failure_streak: 3,
};

/// Stateless helpers for background tasks and session process output handling.
pub(super) struct TaskService;

/// Inputs needed to execute one session command.
pub(super) struct RunSessionTaskInput {
    pub(super) agent: AgentKind,
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    pub(super) cmd: Command,
    pub(super) db: Database,
    pub(super) folder: PathBuf,
    pub(super) id: String,
    pub(super) output: Arc<Mutex<String>>,
    pub(super) permission_mode: PermissionMode,
    pub(super) session_model: String,
    pub(super) status: Arc<Mutex<Status>>,
}

/// Inputs needed to execute an agent-assisted edit task.
pub(super) struct RunAgentAssistTaskInput {
    pub(super) agent: AgentKind,
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) cmd: Command,
    pub(super) db: Database,
    pub(super) id: String,
    pub(super) output: Arc<Mutex<String>>,
    pub(super) permission_mode: PermissionMode,
}

/// Shared context for streaming incremental agent output as it arrives.
#[derive(Clone)]
pub(super) struct StreamOutputContext {
    agent: AgentKind,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    db: Database,
    id: String,
    output: Arc<Mutex<String>>,
    streamed_response_seen: Arc<AtomicBool>,
}

struct CapturedOutput {
    stderr_text: String,
    streamed_response_seen: bool,
    stdout_text: String,
}
impl TaskService {
    /// Spawns a background loop that periodically refreshes ahead/behind info.
    ///
    /// The task emits [`AppEvent::GitStatusUpdated`] snapshots instead of
    /// mutating app state directly.
    pub(super) fn spawn_git_status_task(
        working_dir: &Path,
        cancel: Arc<AtomicBool>,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let dir = working_dir.to_path_buf();
        tokio::spawn(async move {
            let repo_root = git::find_git_repo_root(&dir).unwrap_or(dir);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                {
                    let root = repo_root.clone();
                    let _ = tokio::task::spawn_blocking(move || git::fetch_remote(&root)).await;
                }
                let status = {
                    let root = repo_root.clone();
                    tokio::task::spawn_blocking(move || git::get_ahead_behind(&root))
                        .await
                        .ok()
                        .and_then(std::result::Result::ok)
                };
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = app_event_tx.send(AppEvent::GitStatusUpdated { status });
                for _ in 0..30 {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        });
    }

    /// Executes one agent command, captures output, persists stats, and
    /// commits.
    ///
    /// # Errors
    /// Returns an error when process spawning fails.
    pub(super) async fn run_session_task(input: RunSessionTaskInput) -> Result<(), String> {
        let RunSessionTaskInput {
            agent,
            app_event_tx,
            child_pid,
            cmd,
            db,
            folder,
            id,
            output,
            permission_mode,
            session_model,
            status,
        } = input;

        let mut tokio_cmd = tokio::process::Command::from(cmd);
        // Prevent the child process from inheriting the TUI's terminal on
        // stdin. On macOS the child can otherwise disturb crossterm's raw-mode
        // settings, causing the event reader to stall and the UI to freeze.
        tokio_cmd.stdin(std::process::Stdio::null());

        let mut error: Option<String> = None;

        match tokio_cmd.spawn() {
            Ok(mut child) => {
                if let Some(pid) = child.id()
                    && let Ok(mut guard) = child_pid.lock()
                {
                    *guard = Some(pid);
                }

                let captured =
                    Self::capture_child_output(&mut child, agent, &app_event_tx, &db, &id, &output)
                        .await;
                let exit_status = child.wait().await.ok();

                if let Ok(mut guard) = child_pid.lock() {
                    *guard = None;
                }

                let killed_by_signal = exit_status
                    .as_ref()
                    .is_some_and(|status| status.signal().is_some());

                if killed_by_signal {
                    let message = "\n[Stopped] Agent interrupted by user.\n";
                    Self::append_session_output(&output, &db, &app_event_tx, &id, message).await;
                } else {
                    let parsed = agent.parse_response(
                        &captured.stdout_text,
                        &captured.stderr_text,
                        permission_mode,
                    );
                    if !captured.streamed_response_seen {
                        Self::append_session_output(
                            &output,
                            &db,
                            &app_event_tx,
                            &id,
                            &parsed.content,
                        )
                        .await;
                    }

                    let _ = db.update_session_stats(&id, &parsed.stats).await;
                    Self::handle_auto_commit(AssistContext {
                        agent,
                        app_event_tx: app_event_tx.clone(),
                        db: db.clone(),
                        folder,
                        id: id.clone(),
                        output: Arc::clone(&output),
                        permission_mode,
                        session_model,
                    })
                    .await;
                }
            }
            Err(spawn_error) => {
                let message = format!("Failed to spawn process: {spawn_error}\n");
                Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
                error = Some(message.trim().to_string());
            }
        }

        let _ = Self::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;

        if let Some(error) = error {
            return Err(error);
        }

        Ok(())
    }

    async fn handle_auto_commit(context: AssistContext) {
        match Self::commit_changes_with_assist(&context).await {
            Ok(Some(hash)) => {
                let message = format!("\n[Commit] committed with hash `{hash}`\n");
                Self::append_session_output(
                    &context.output,
                    &context.db,
                    &context.app_event_tx,
                    &context.id,
                    &message,
                )
                .await;
            }
            Ok(None) => {}
            Err(commit_error) => {
                let message = format!("\n[Commit Error] {commit_error}\n");
                Self::append_session_output(
                    &context.output,
                    &context.db,
                    &context.app_event_tx,
                    &context.id,
                    &message,
                )
                .await;
            }
        }
    }

    async fn commit_changes_with_assist(context: &AssistContext) -> Result<Option<String>, String> {
        let mut failure_tracker =
            FailureTracker::new(AUTO_COMMIT_ASSIST_POLICY.max_identical_failure_streak);

        for assist_attempt in 1..=AUTO_COMMIT_ASSIST_POLICY.max_attempts + 1 {
            match SessionManager::commit_changes(&context.folder, false).await {
                Ok(commit_hash) => {
                    return Ok(Some(commit_hash));
                }
                Err(commit_error) if commit_error.contains("Nothing to commit") => {
                    return Ok(None);
                }
                Err(commit_error) => {
                    if !Self::is_commit_error_retryable(&commit_error) {
                        return Err(commit_error);
                    }

                    if failure_tracker.observe(&commit_error) {
                        return Err(format!(
                            "Auto-commit assistance made no progress: repeated identical commit \
                             failure. Last error: {commit_error}"
                        ));
                    }

                    if assist_attempt > AUTO_COMMIT_ASSIST_POLICY.max_attempts {
                        return Err(commit_error);
                    }

                    Self::append_commit_assist_header(context, assist_attempt, &commit_error).await;
                    Self::run_commit_assist_for_error(context, &commit_error).await?;
                }
            }
        }

        Err("Failed to auto-commit after assistance attempts".to_string())
    }

    fn is_commit_error_retryable(commit_error: &str) -> bool {
        let normalized_error = commit_error.to_ascii_lowercase();
        let non_retryable_patterns = [
            "author identity unknown",
            "unable to auto-detect email address",
            "please tell me who you are",
            "gpg failed to sign the data",
            "gpg: signing failed",
            "commit hooks kept modifying files after",
            "failed to execute git",
            "not a git repository",
        ];
        if non_retryable_patterns
            .iter()
            .any(|pattern| normalized_error.contains(pattern))
        {
            return false;
        }

        true
    }

    async fn append_commit_assist_header(
        context: &AssistContext,
        assist_attempt: usize,
        commit_error: &str,
    ) {
        let formatted_error = Self::format_commit_error_for_display(commit_error);
        append_assist_header(
            context,
            "Commit",
            assist_attempt,
            AUTO_COMMIT_ASSIST_POLICY.max_attempts,
            "Resolving auto-commit failure:",
            &formatted_error,
        )
        .await;
    }

    async fn run_commit_assist_for_error(
        context: &AssistContext,
        commit_error: &str,
    ) -> Result<(), String> {
        let prompt = Self::auto_commit_assist_prompt(commit_error);
        let effective_permission_mode =
            Self::auto_commit_assist_permission_mode(context.permission_mode);
        let assist_context = AssistContext {
            agent: context.agent,
            app_event_tx: context.app_event_tx.clone(),
            db: context.db.clone(),
            folder: context.folder.clone(),
            id: context.id.clone(),
            output: Arc::clone(&context.output),
            permission_mode: effective_permission_mode,
            session_model: context.session_model.clone(),
        };

        run_agent_assist(&assist_context, &prompt)
            .await
            .map_err(|error| format!("Commit assistance failed: {error}"))
    }

    fn auto_commit_assist_prompt(commit_error: &str) -> String {
        AUTO_COMMIT_ASSIST_PROMPT_TEMPLATE.replace("{commit_error}", commit_error.trim())
    }

    fn auto_commit_assist_permission_mode(permission_mode: PermissionMode) -> PermissionMode {
        effective_permission_mode(permission_mode)
    }

    fn format_commit_error_for_display(commit_error: &str) -> String {
        format_detail_lines(commit_error)
    }

    /// Executes one agent command for assisted edits without auto-commit.
    ///
    /// # Errors
    /// Returns an error when spawning fails, waiting fails, the process is
    /// interrupted, or the command exits with a non-zero status code.
    pub(super) async fn run_agent_assist_task(
        input: RunAgentAssistTaskInput,
    ) -> Result<(), String> {
        let RunAgentAssistTaskInput {
            agent,
            app_event_tx,
            cmd,
            db,
            id,
            output,
            permission_mode,
        } = input;

        let mut tokio_cmd = tokio::process::Command::from(cmd);
        tokio_cmd.stdin(std::process::Stdio::null());

        let mut child = match tokio_cmd.spawn() {
            Ok(child) => child,
            Err(spawn_error) => {
                let message = format!("Failed to spawn process: {spawn_error}\n");
                Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;

                return Err(message.trim().to_string());
            }
        };

        let captured =
            Self::capture_child_output(&mut child, agent, &app_event_tx, &db, &id, &output).await;

        let exit_status = child
            .wait()
            .await
            .map_err(|error| format!("Failed to wait for agent assistance process: {error}"))?;

        if exit_status.signal().is_some() {
            let message = "\n[Stopped] Agent assistance interrupted.\n";
            Self::append_session_output(&output, &db, &app_event_tx, &id, message).await;

            return Err("Agent assistance interrupted".to_string());
        }

        if !exit_status.success() {
            return Err(Self::format_assist_exit_error(
                exit_status.code(),
                &captured.stdout_text,
                &captured.stderr_text,
            ));
        }

        let parsed = agent.parse_response(
            &captured.stdout_text,
            &captured.stderr_text,
            permission_mode,
        );

        if !captured.streamed_response_seen {
            Self::append_session_output(&output, &db, &app_event_tx, &id, &parsed.content).await;
        }
        let _ = db.update_session_stats(&id, &parsed.stats).await;

        Ok(())
    }

    async fn capture_child_output(
        child: &mut tokio::process::Child,
        agent: AgentKind,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        db: &Database,
        id: &str,
        output: &Arc<Mutex<String>>,
    ) -> CapturedOutput {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let raw_stdout = Arc::new(Mutex::new(String::new()));
        let raw_stderr = Arc::new(Mutex::new(String::new()));
        let mut handles = Vec::new();
        let streamed_response_seen = Arc::new(AtomicBool::new(false));

        if let Some(stdout) = stdout {
            let buffer = Arc::clone(&raw_stdout);
            let stream_context = StreamOutputContext {
                agent,
                app_event_tx: app_event_tx.clone(),
                db: db.clone(),
                id: id.to_string(),
                output: Arc::clone(output),
                streamed_response_seen: Arc::clone(&streamed_response_seen),
            };
            handles.push(tokio::spawn(async move {
                Self::capture_raw_output(stdout, &buffer, Some(stream_context)).await;
            }));
        }

        if let Some(stderr) = stderr {
            let buffer = Arc::clone(&raw_stderr);
            handles.push(tokio::spawn(async move {
                Self::capture_raw_output(stderr, &buffer, None).await;
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        let stderr_text = raw_stderr.lock().map(|buf| buf.clone()).unwrap_or_default();
        let stdout_text = raw_stdout.lock().map(|buf| buf.clone()).unwrap_or_default();

        CapturedOutput {
            stderr_text,
            streamed_response_seen: streamed_response_seen.load(Ordering::Relaxed),
            stdout_text,
        }
    }

    fn format_assist_exit_error(exit_code: Option<i32>, stdout: &str, stderr: &str) -> String {
        let exit_code = exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
        let output_detail = Self::format_assist_output_detail(stdout, stderr);

        format!("Agent assistance failed with exit code {exit_code}: {output_detail}")
    }

    fn format_assist_output_detail(stdout: &str, stderr: &str) -> String {
        let trimmed_stdout = stdout.trim();
        let trimmed_stderr = stderr.trim();
        if !trimmed_stderr.is_empty() && !trimmed_stdout.is_empty() {
            return format!("stderr: {trimmed_stderr}; stdout: {trimmed_stdout}");
        }
        if !trimmed_stderr.is_empty() {
            return format!("stderr: {trimmed_stderr}");
        }
        if !trimmed_stdout.is_empty() {
            return format!("stdout: {trimmed_stdout}");
        }

        "no stdout or stderr output".to_string()
    }

    /// Applies a status transition to memory and database when valid.
    ///
    /// This emits [`AppEvent::SessionUpdated`] for targeted snapshot sync and
    /// emits [`AppEvent::RefreshSessions`] for transitions that require full
    /// list reload.
    pub(super) async fn update_status(
        status: &Mutex<Status>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        new: Status,
    ) -> bool {
        let should_update = if let Ok(mut current) = status.lock() {
            if (*current).can_transition_to(new) {
                *current = new;
                true
            } else {
                false
            }
        } else {
            false
        };
        if !should_update {
            return false;
        }
        let _ = db.update_session_status(id, &new.to_string()).await;
        let session_id = id.to_string();
        let _ = app_event_tx.send(AppEvent::SessionUpdated { session_id });
        if Self::status_requires_full_refresh(new) {
            let _ = app_event_tx.send(AppEvent::RefreshSessions);
        }

        true
    }

    /// Captures raw output from a stream into an in-memory buffer.
    pub(super) async fn capture_raw_output<R: AsyncRead + Unpin>(
        source: R,
        buffer: &Arc<Mutex<String>>,
        stream_context: Option<StreamOutputContext>,
    ) {
        let mut reader = tokio::io::BufReader::new(source).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(mut buf) = buffer.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }

            let Some(stream_context) = &stream_context else {
                continue;
            };
            let Some((stream_text, is_response_content)) =
                stream_context.agent.parse_stream_output_line(&line)
            else {
                continue;
            };
            if stream_text.trim().is_empty() {
                continue;
            }

            let formatted_stream_text = format!("{stream_text}\n");
            Self::append_session_output(
                &stream_context.output,
                &stream_context.db,
                &stream_context.app_event_tx,
                &stream_context.id,
                &formatted_stream_text,
            )
            .await;
            if is_response_content {
                stream_context
                    .streamed_response_seen
                    .store(true, Ordering::Relaxed);
            }
        }
    }

    /// Appends output to the in-memory handle buffer and database.
    pub(super) async fn append_session_output(
        output: &Arc<Mutex<String>>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        message: &str,
    ) {
        if let Ok(mut buf) = output.lock() {
            buf.push_str(message);
        }
        let _ = db.append_session_output(id, message).await;
        let _ = app_event_tx.send(AppEvent::SessionUpdated {
            session_id: id.to_string(),
        });
    }

    fn status_requires_full_refresh(status: Status) -> bool {
        matches!(
            status,
            Status::InProgress
                | Status::Review
                | Status::Merging
                | Status::CreatingPullRequest
                | Status::PullRequest
                | Status::Done
                | Status::Canceled
        )
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;
    use crate::db::Database;

    #[test]
    fn test_status_requires_full_refresh_for_lifecycle_statuses() {
        // Arrange
        let refresh_statuses = [
            Status::InProgress,
            Status::Review,
            Status::Merging,
            Status::CreatingPullRequest,
            Status::PullRequest,
            Status::Done,
            Status::Canceled,
        ];

        // Act & Assert
        for status in refresh_statuses {
            assert!(TaskService::status_requires_full_refresh(status));
        }
        assert!(!TaskService::status_requires_full_refresh(Status::New));
    }

    #[test]
    fn test_auto_commit_assist_permission_mode_plan_uses_auto_edit() {
        // Arrange
        let permission_mode = PermissionMode::Plan;

        // Act
        let effective_mode = TaskService::auto_commit_assist_permission_mode(permission_mode);

        // Assert
        assert_eq!(effective_mode, PermissionMode::AutoEdit);
    }

    #[test]
    fn test_auto_commit_assist_prompt_includes_commit_error() {
        // Arrange
        let commit_error = "Failed to commit: merge conflict remains";

        // Act
        let prompt = TaskService::auto_commit_assist_prompt(commit_error);

        // Assert
        assert!(prompt.contains("Failed to commit: merge conflict remains"));
    }

    #[test]
    fn test_format_commit_error_for_display_returns_bulleted_lines() {
        // Arrange
        let commit_error = "line one\nline two";

        // Act
        let formatted = TaskService::format_commit_error_for_display(commit_error);

        // Assert
        assert_eq!(formatted, "- line one\n- line two");
    }

    #[test]
    fn test_is_commit_error_retryable_returns_false_for_identity_error() {
        // Arrange
        let commit_error = "Failed to commit: Author identity unknown";

        // Act
        let is_retryable = TaskService::is_commit_error_retryable(commit_error);

        // Assert
        assert!(!is_retryable);
    }

    #[test]
    fn test_is_commit_error_retryable_returns_true_for_conflict_error() {
        // Arrange
        let commit_error =
            "Failed to commit: Committing is not possible because you have unmerged files";

        // Act
        let is_retryable = TaskService::is_commit_error_retryable(commit_error);

        // Assert
        assert!(is_retryable);
    }

    #[test]
    fn test_is_commit_error_retryable_returns_true_for_generic_failure() {
        // Arrange
        let commit_error = "Failed to commit: rustfmt failed";

        // Act
        let is_retryable = TaskService::is_commit_error_retryable(commit_error);

        // Assert
        assert!(is_retryable);
    }

    #[tokio::test]
    async fn test_run_agent_assist_task_appends_output_without_committing() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-id",
                "claude",
                "claude-sonnet-4-20250514",
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));

        let mut command = Command::new("sh");
        command
            .args(["-lc", "printf 'assistant output'"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = TaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Claude,
            app_event_tx,
            cmd: command,
            db: database.clone(),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            permission_mode: PermissionMode::AutoEdit,
        })
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(output_text.contains("assistant output"));
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_run_agent_assist_task_streams_codex_output_without_duplication() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-id",
                "codex",
                "gpt-5.3-codex",
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));

        let mut command = Command::new("sh");
        command
            .args([
                "-lc",
                "printf '%s\\n' \
                 '{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\"}}' \
                 '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"\
                 Final answer\"}}' \
                 '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":11,\"output_tokens\":\
                 7}}'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = TaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Codex,
            app_event_tx,
            cmd: command,
            db: database.clone(),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            permission_mode: PermissionMode::AutoEdit,
        })
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(output_text.contains("[command_execution] in progress..."));
        assert_eq!(output_text.matches("Final answer").count(), 1);
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, Some(11));
        assert_eq!(sessions[0].output_tokens, Some(7));
    }

    #[tokio::test]
    async fn test_run_agent_assist_task_returns_error_for_non_zero_exit_status() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-id",
                "claude",
                "claude-sonnet-4-20250514",
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));

        let mut command = Command::new("sh");
        command
            .args(["-lc", "printf 'assist failed' >&2; exit 7"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = TaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Claude,
            app_event_tx,
            cmd: command,
            db: database,
            id: "session-id".to_string(),
            output,
            permission_mode: PermissionMode::AutoEdit,
        })
        .await;

        // Assert
        assert!(result.is_err());
        let error_text = result.expect_err("expected non-zero exit to fail");
        assert!(error_text.contains("exit code 7"));
        assert!(error_text.contains("assist failed"));
    }
}
