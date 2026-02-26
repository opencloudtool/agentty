//! Session task execution helpers for process running, output capture, and
//! status persistence.

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncRead};
use tokio::sync::mpsc;

use super::COMMIT_MESSAGE;
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, format_detail_lines,
    run_agent_assist,
};
use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::session::Status;
use crate::infra::db::Database;
use crate::infra::git::GitClient;

const AUTO_COMMIT_ASSIST_PROMPT_TEMPLATE: &str =
    include_str!("../../../resources/auto_commit_assist_prompt.md");
const AUTO_COMMIT_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 10,
    max_identical_failure_streak: 3,
};
/// Maximum wall-clock delay before buffered output is flushed.
const OUTPUT_BATCH_INTERVAL: Duration = Duration::from_millis(50);
/// Maximum buffered output size before a flush is triggered.
const OUTPUT_BATCH_SIZE: usize = 1024; // 1KB
/// Stateless helpers for session process execution and output handling.
pub(crate) struct SessionTaskService;

/// Inputs needed to execute one session command.
pub(super) struct RunSessionTaskInput {
    /// App event sender used for progress and status updates.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Shared child pid slot for cancellation and status views.
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    /// Prepared agent command to execute.
    pub(super) cmd: Command,
    /// Database handle used for output/status/stat persistence.
    pub(super) db: Database,
    /// Session worktree folder where the command runs.
    pub(super) folder: PathBuf,
    /// Git boundary used for post-run size/commit operations.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Session identifier for persisted updates.
    pub(super) id: String,
    /// Shared output buffer for streamed and parsed content.
    pub(super) output: Arc<Mutex<String>>,
    /// Session model used for command parsing and follow-up operations.
    pub(super) session_model: AgentModel,
    /// Shared mutable lifecycle status.
    pub(super) status: Arc<Mutex<Status>>,
}

/// Inputs needed to execute an agent-assisted edit task.
pub(crate) struct RunAgentAssistTaskInput {
    /// Agent family used for response parsing.
    pub(crate) agent: AgentKind,
    /// App event sender used for progress and status updates.
    pub(crate) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Prepared assist command to execute.
    pub(crate) cmd: Command,
    /// Database handle used for output/status persistence.
    pub(crate) db: Database,
    /// Session identifier for persisted updates.
    pub(crate) id: String,
    /// Shared output buffer receiving incremental output.
    pub(crate) output: Arc<Mutex<String>>,
    /// Session model used for agent metadata and parsing.
    pub(crate) session_model: AgentModel,
}

/// Shared context for streaming incremental agent output as it arrives.
#[derive(Clone)]
pub(super) struct StreamOutputContext {
    /// Latest progress line currently shown in the UI, if any.
    active_progress_message: Arc<Mutex<Option<String>>>,
    /// Agent family used for stream parsing.
    agent: AgentKind,
    /// App event sender used to publish output/progress updates.
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Database handle used for persisted output chunks.
    db: Database,
    /// Session identifier for output updates.
    id: String,
    /// Tracks whether non-response stream output was seen.
    non_response_stream_output_seen: Arc<AtomicBool>,
    /// Shared in-memory output buffer.
    output: Arc<Mutex<String>>,
    /// Tracks whether streamed response chunks were seen.
    streamed_response_seen: Arc<AtomicBool>,
}

/// Captured terminal output from one child process run.
struct CapturedOutput {
    /// Full stderr text captured while the child process ran.
    stderr_text: String,
    /// Whether response content was already emitted incrementally from stream.
    streamed_response_seen: bool,
    /// Full stdout text captured while the child process ran.
    stdout_text: String,
}

enum ActiveProgressMessageUpdate {
    NoChange,
    Updated,
}

impl SessionTaskService {
    /// Executes one agent command, captures output, persists stats, and
    /// commits.
    ///
    /// # Errors
    /// Returns an error when process spawning fails.
    pub(super) async fn run_session_task(input: RunSessionTaskInput) -> Result<(), String> {
        let RunSessionTaskInput {
            app_event_tx,
            child_pid,
            cmd,
            db,
            folder,
            git_client,
            id,
            output,
            session_model,
            status,
        } = input;
        let agent = session_model.kind();

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
                Self::clear_session_progress(&app_event_tx, &id);

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
                    let parsed = crate::infra::agent::parse_response(
                        agent,
                        &captured.stdout_text,
                        &captured.stderr_text,
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
                    let _ = db
                        .upsert_session_usage(&id, session_model.as_str(), &parsed.stats)
                        .await;
                    {
                        let folder = folder.clone();
                        Self::handle_auto_commit(AssistContext {
                            app_event_tx: app_event_tx.clone(),
                            db: db.clone(),
                            folder,
                            git_client: Arc::clone(&git_client),
                            id: id.clone(),
                            output: Arc::clone(&output),
                            session_model,
                        })
                        .await;
                    }
                }
            }
            Err(spawn_error) => {
                let message = format!("Failed to spawn process: {spawn_error}\n");
                Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
                Self::clear_session_progress(&app_event_tx, &id);
                error = Some(message.trim().to_string());
            }
        }

        Self::refresh_persisted_session_size(&db, git_client.as_ref(), &id, &folder).await;
        let _ = Self::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;

        if let Some(error) = error {
            return Err(error);
        }

        Ok(())
    }

    /// Recomputes and persists one session size using the session worktree
    /// diff.
    pub(crate) async fn refresh_persisted_session_size(
        db: &Database,
        git_client: &dyn GitClient,
        session_id: &str,
        folder: &Path,
    ) {
        let Some(base_branch) = db.get_session_base_branch(session_id).await.ok().flatten() else {
            return;
        };
        let computed_size =
            SessionManager::session_size_for_folder(git_client, folder, &base_branch).await;
        let _ = db
            .update_session_size(session_id, &computed_size.to_string())
            .await;
    }

    /// Commits pending worktree changes and appends the outcome to session
    /// output.
    pub(in crate::app) async fn handle_auto_commit(context: AssistContext) {
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
        // Test repos do not install hooks deterministically; skip hook
        // execution in tests to keep auto-commit behavior stable.
        let skip_verify_hooks = cfg!(test);

        for assist_attempt in 1..=AUTO_COMMIT_ASSIST_POLICY.max_attempts + 1 {
            match Self::commit_changes_with_git_client(context, skip_verify_hooks).await {
                Ok(commit_hash) => {
                    return Ok(Some(commit_hash));
                }
                Err(commit_error) if commit_error.contains("Nothing to commit") => {
                    return Ok(None);
                }
                Err(commit_error) => {
                    // Keep test execution deterministic and offline by skipping
                    // model-assisted commit retries.
                    if cfg!(test) {
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

    /// Commits all worktree changes and returns the current `HEAD` short hash.
    ///
    /// Pass `no_verify` to skip commit hooks (used in tests for deterministic
    /// execution without pre-commit setup).
    ///
    /// # Errors
    /// Returns an error if staging/commit fails or `HEAD` cannot be resolved.
    async fn commit_changes_with_git_client(
        context: &AssistContext,
        no_verify: bool,
    ) -> Result<String, String> {
        let folder = context.folder.clone();
        context
            .git_client
            .commit_all_preserving_single_commit(
                folder.clone(),
                COMMIT_MESSAGE.to_string(),
                no_verify,
            )
            .await?;

        context.git_client.head_short_hash(folder).await
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
        let assist_context = AssistContext {
            app_event_tx: context.app_event_tx.clone(),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            id: context.id.clone(),
            output: Arc::clone(&context.output),
            session_model: context.session_model,
        };

        run_agent_assist(&assist_context, &prompt)
            .await
            .map_err(|error| format!("Commit assistance failed: {error}"))
    }

    fn auto_commit_assist_prompt(commit_error: &str) -> String {
        AUTO_COMMIT_ASSIST_PROMPT_TEMPLATE.replace("{commit_error}", commit_error.trim())
    }

    fn format_commit_error_for_display(commit_error: &str) -> String {
        format_detail_lines(commit_error)
    }

    /// Executes one agent command for assisted edits without auto-commit.
    ///
    /// # Errors
    /// Returns an error when spawning fails, waiting fails, the process is
    /// interrupted, or the command exits with a non-zero status code.
    pub(crate) async fn run_agent_assist_task(
        input: RunAgentAssistTaskInput,
    ) -> Result<(), String> {
        let RunAgentAssistTaskInput {
            agent,
            app_event_tx,
            cmd,
            db,
            id,
            output,
            session_model,
        } = input;

        let mut tokio_cmd = tokio::process::Command::from(cmd);
        tokio_cmd.stdin(std::process::Stdio::null());

        let mut child = match tokio_cmd.spawn() {
            Ok(child) => child,
            Err(spawn_error) => {
                let message = format!("Failed to spawn process: {spawn_error}\n");
                Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
                Self::clear_session_progress(&app_event_tx, &id);

                return Err(message.trim().to_string());
            }
        };

        let captured =
            Self::capture_child_output(&mut child, agent, &app_event_tx, &db, &id, &output).await;

        let exit_status = child
            .wait()
            .await
            .map_err(|error| format!("Failed to wait for agent assistance process: {error}"))?;
        Self::clear_session_progress(&app_event_tx, &id);

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

        let parsed = crate::infra::agent::parse_response(
            agent,
            &captured.stdout_text,
            &captured.stderr_text,
        );

        if !captured.streamed_response_seen {
            Self::append_session_output(&output, &db, &app_event_tx, &id, &parsed.content).await;
        }
        let _ = db.update_session_stats(&id, &parsed.stats).await;
        let _ = db
            .upsert_session_usage(&id, session_model.as_str(), &parsed.stats)
            .await;

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
        let active_progress_message = Arc::new(Mutex::new(None));
        let non_response_stream_output_seen = Arc::new(AtomicBool::new(false));
        let streamed_response_seen = Arc::new(AtomicBool::new(false));

        if let Some(stdout) = stdout {
            let buffer = Arc::clone(&raw_stdout);
            let stream_context = StreamOutputContext {
                active_progress_message: Arc::clone(&active_progress_message),
                agent,
                app_event_tx: app_event_tx.clone(),
                db: db.clone(),
                id: id.to_string(),
                non_response_stream_output_seen: Arc::clone(&non_response_stream_output_seen),
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
    pub(crate) async fn update_status(
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
        let mut raw_buffer_batch = String::new();
        let mut session_output_batch = String::new();
        let mut last_flush = std::time::Instant::now();

        while let Ok(Some(line)) = reader.next_line().await {
            raw_buffer_batch.push_str(&line);
            raw_buffer_batch.push('\n');

            let should_flush = raw_buffer_batch.len() >= OUTPUT_BATCH_SIZE
                || session_output_batch.len() >= OUTPUT_BATCH_SIZE
                || last_flush.elapsed() >= OUTPUT_BATCH_INTERVAL;

            Self::flush_raw_buffer_if_needed(buffer, &mut raw_buffer_batch, should_flush);

            let Some(stream_context) = &stream_context else {
                if should_flush {
                    last_flush = std::time::Instant::now();
                }
                continue;
            };

            let Some((stream_text, is_response_content)) =
                crate::infra::agent::parse_stream_output_line(stream_context.agent, &line)
            else {
                // Flush session output batch if any, even if this line was skipped
                Self::flush_session_output_if_needed(
                    stream_context,
                    &mut session_output_batch,
                    should_flush,
                    &mut last_flush,
                )
                .await;
                continue;
            };

            if stream_text.trim().is_empty() {
                Self::flush_session_output_if_needed(
                    stream_context,
                    &mut session_output_batch,
                    should_flush,
                    &mut last_flush,
                )
                .await;
                continue;
            }

            if is_response_content {
                Self::handle_response_content_line(
                    stream_context,
                    &stream_text,
                    &mut session_output_batch,
                    should_flush,
                    &mut last_flush,
                )
                .await;

                continue;
            }

            Self::handle_progress_content_line(
                stream_context,
                stream_text,
                &mut session_output_batch,
                should_flush,
                &mut last_flush,
            )
            .await;
        }

        // Final flush
        if !raw_buffer_batch.is_empty()
            && let Ok(mut buf) = buffer.lock()
        {
            buf.push_str(&raw_buffer_batch);
        }

        if let Some(stream_context) = stream_context
            && !session_output_batch.is_empty()
        {
            Self::append_session_output(
                &stream_context.output,
                &stream_context.db,
                &stream_context.app_event_tx,
                &stream_context.id,
                &session_output_batch,
            )
            .await;
        }
    }

    /// Appends output to the in-memory handle buffer and database.
    pub(crate) async fn append_session_output(
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

    /// Flushes raw stream text into the shared in-memory output buffer.
    fn flush_raw_buffer_if_needed(
        buffer: &Arc<Mutex<String>>,
        raw_buffer_batch: &mut String,
        should_flush: bool,
    ) {
        if should_flush && !raw_buffer_batch.is_empty() {
            if let Ok(mut buf) = buffer.lock() {
                buf.push_str(raw_buffer_batch);
            }

            raw_buffer_batch.clear();
        }
    }

    /// Flushes session-visible output when the caller indicates a flush point.
    async fn flush_session_output_if_needed(
        stream_context: &StreamOutputContext,
        session_output_batch: &mut String,
        should_flush: bool,
        last_flush: &mut std::time::Instant,
    ) {
        if !should_flush {
            return;
        }

        Self::flush_session_output_batch(stream_context, session_output_batch).await;
        *last_flush = std::time::Instant::now();
    }

    /// Handles parsed response content and reconciles progress state
    /// transitions.
    ///
    /// Streamed response entries are separated by a blank line to improve
    /// readability in chat output. Progress labels are cleared from UI state
    /// without emitting synthetic completion lines into chat output.
    async fn handle_response_content_line(
        stream_context: &StreamOutputContext,
        stream_text: &str,
        session_output_batch: &mut String,
        should_flush: bool,
        last_flush: &mut std::time::Instant,
    ) {
        let should_prefix_blank_line = !stream_context
            .streamed_response_seen
            .load(Ordering::Relaxed)
            && stream_context
                .non_response_stream_output_seen
                .load(Ordering::Relaxed);

        if should_prefix_blank_line {
            session_output_batch.push('\n');
        }

        let normalized_stream_text = stream_text.trim_end_matches('\n');
        session_output_batch.push_str(normalized_stream_text);
        session_output_batch.push_str("\n\n");

        if should_flush {
            Self::flush_session_output_batch(stream_context, session_output_batch).await;
            *last_flush = std::time::Instant::now();
        }

        stream_context
            .streamed_response_seen
            .store(true, Ordering::Relaxed);

        let previous_progress_message =
            Self::take_active_progress_message(&stream_context.active_progress_message);

        if previous_progress_message.is_some() {
            // Flush pending output before clearing the progress indicator.
            Self::flush_session_output_batch(stream_context, session_output_batch).await;
            Self::set_session_progress(&stream_context.app_event_tx, &stream_context.id, None);
        }
    }

    /// Handles parsed non-response progress content and publishes progress
    /// updates without appending progress text into chat output.
    async fn handle_progress_content_line(
        stream_context: &StreamOutputContext,
        stream_text: String,
        session_output_batch: &mut String,
        should_flush: bool,
        last_flush: &mut std::time::Instant,
    ) {
        stream_context
            .non_response_stream_output_seen
            .store(true, Ordering::Relaxed);

        match Self::replace_active_progress_message_if_changed(
            &stream_context.active_progress_message,
            &stream_text,
        ) {
            ActiveProgressMessageUpdate::NoChange => {
                Self::flush_session_output_if_needed(
                    stream_context,
                    session_output_batch,
                    should_flush,
                    last_flush,
                )
                .await;

                return;
            }
            ActiveProgressMessageUpdate::Updated => {}
        }

        // Flush pending output before publishing progress updates.
        Self::flush_session_output_batch(stream_context, session_output_batch).await;
        Self::set_session_progress(
            &stream_context.app_event_tx,
            &stream_context.id,
            Some(stream_text),
        );

        // Reset flush timer as we just did a potential write (progress update events
        // are immediate)
        *last_flush = std::time::Instant::now();
    }

    /// Persists and clears the session output batch when it contains data.
    async fn flush_session_output_batch(
        stream_context: &StreamOutputContext,
        session_output_batch: &mut String,
    ) {
        if !session_output_batch.is_empty() {
            Self::append_session_output(
                &stream_context.output,
                &stream_context.db,
                &stream_context.app_event_tx,
                &stream_context.id,
                session_output_batch,
            )
            .await;
            session_output_batch.clear();
        }
    }

    fn take_active_progress_message(
        active_progress_message: &Arc<Mutex<Option<String>>>,
    ) -> Option<String> {
        let Ok(mut active_progress_message) = active_progress_message.lock() else {
            return None;
        };

        active_progress_message.take()
    }

    /// Clears the transient progress message for one session.
    pub(crate) fn clear_session_progress(app_event_tx: &mpsc::UnboundedSender<AppEvent>, id: &str) {
        Self::set_session_progress(app_event_tx, id, None);
    }

    fn replace_active_progress_message_if_changed(
        active_progress_message: &Arc<Mutex<Option<String>>>,
        stream_text: &str,
    ) -> ActiveProgressMessageUpdate {
        let Ok(mut active_progress_message) = active_progress_message.lock() else {
            return ActiveProgressMessageUpdate::NoChange;
        };
        if active_progress_message.as_deref() == Some(stream_text) {
            return ActiveProgressMessageUpdate::NoChange;
        }
        active_progress_message.replace(stream_text.to_string());

        ActiveProgressMessageUpdate::Updated
    }

    /// Emits a transient progress message update for one session.
    pub(crate) fn set_session_progress(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        progress_message: Option<String>,
    ) {
        let _ = app_event_tx.send(AppEvent::SessionProgressUpdated {
            progress_message,
            session_id: id.to_string(),
        });
    }

    fn status_requires_full_refresh(status: Status) -> bool {
        matches!(
            status,
            Status::InProgress | Status::Review | Status::Merging | Status::Done | Status::Canceled
        )
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;
    use crate::db::Database;
    use crate::infra::git::MockGitClient;

    #[test]
    /// Verifies lifecycle statuses that require full list refreshes are
    /// enumerated correctly.
    fn test_status_requires_full_refresh_for_lifecycle_statuses() {
        // Arrange
        let refresh_statuses = [
            Status::InProgress,
            Status::Review,
            Status::Merging,
            Status::Done,
            Status::Canceled,
        ];

        // Act & Assert
        for status in refresh_statuses {
            assert!(SessionTaskService::status_requires_full_refresh(status));
        }
        assert!(!SessionTaskService::status_requires_full_refresh(
            Status::New
        ));
    }

    #[test]
    /// Ensures commit assistance prompts include the raw git failure details.
    fn test_auto_commit_assist_prompt_includes_commit_error() {
        // Arrange
        let commit_error = "Failed to commit: merge conflict remains";

        // Act
        let prompt = SessionTaskService::auto_commit_assist_prompt(commit_error);

        // Assert
        assert!(prompt.contains("Failed to commit: merge conflict remains"));
    }

    #[test]
    /// Ensures commit error formatting normalizes output as bullet lines.
    fn test_format_commit_error_for_display_returns_bulleted_lines() {
        // Arrange
        let commit_error = "line one\nline two";

        // Act
        let formatted = SessionTaskService::format_commit_error_for_display(commit_error);

        // Assert
        assert_eq!(formatted, "- line one\n- line two");
    }

    #[tokio::test]
    /// Verifies commit helper success uses the injected git trait boundary and
    /// returns the generated commit hash.
    async fn test_commit_changes_with_assist_returns_commit_hash_from_mock_git_client() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let context = AssistContext {
            app_event_tx,
            db: Database::open_in_memory()
                .await
                .expect("failed to open in-memory db"),
            folder: PathBuf::from("/tmp/project"),
            git_client: Arc::new(mock_git_client),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::Gpt53Codex,
        };

        // Act
        let result = SessionTaskService::commit_changes_with_assist(&context).await;

        // Assert
        assert_eq!(result, Ok(Some("abc1234".to_string())));
        let output_text = output
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default();
        assert!(output_text.is_empty());
    }

    #[tokio::test]
    /// Verifies commit helper failure appends a commit error message without
    /// invoking real git or agent subprocesses.
    async fn test_handle_auto_commit_appends_commit_error_from_mock_git_client() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Err("commit failed".to_string()) }));
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let context = AssistContext {
            app_event_tx,
            db: Database::open_in_memory()
                .await
                .expect("failed to open in-memory db"),
            folder: PathBuf::from("/tmp/project"),
            git_client: Arc::new(mock_git_client),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::Gpt53Codex,
        };

        // Act
        SessionTaskService::handle_auto_commit(context).await;

        // Assert
        let output_text = output
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default();
        assert!(output_text.contains("[Commit Error] commit failed"));
    }

    #[tokio::test]
    /// Ensures assist command output is appended without auto-commit side
    /// effects for assistant-only runs.
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
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Claude,
            app_event_tx,
            cmd: command,
            db: database.clone(),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::ClaudeOpus46,
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
    /// Verifies Codex stream parsing drops command-progress JSON while keeping
    /// final assistant output.
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
            .insert_session("session-id", "gpt-5.3-codex", "main", "Review", project_id)
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
                 Command completed\"}}' \
                 '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"\
                 Final answer\"}}' \
                 '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":11,\"output_tokens\":\
                 7}}'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Codex,
            app_event_tx,
            cmd: command,
            db: database.clone(),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::Gpt53Codex,
        })
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(!output_text.contains("Command completed"));
        assert!(!output_text.contains("Running a command"));
        assert!(!output_text.contains("command_execution"));
        assert_eq!(output_text.matches("Final answer").count(), 1);
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, 11);
        assert_eq!(sessions[0].output_tokens, 7);
    }

    #[tokio::test]
    /// Verifies stream parsing keeps spacing between distinct assistant
    /// messages.
    async fn test_run_agent_assist_task_streams_codex_output_with_spacing_between_messages() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-id", "gpt-5.3-codex", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));

        let mut command = Command::new("sh");
        command
            .args([
                "-lc",
                "printf '%s\\n' \
                 '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"\
                 First message\"}}' \
                 '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"\
                 Final answer\"}}' \
                 '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":11,\"output_tokens\":\
                 7}}'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Codex,
            app_event_tx,
            cmd: command,
            db: database,
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::Gpt53Codex,
        })
        .await;
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        assert!(output_text.contains("First message\n\nFinal answer"));
        assert_eq!(output_text.matches("Final answer").count(), 1);
    }

    #[tokio::test]
    /// Verifies Claude progress events stay compact while final response and
    /// token stats are persisted.
    async fn test_run_agent_assist_task_streams_claude_output_with_compact_progress() {
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
            .args([
                "-lc",
                "printf '%s\\n' '{\"type\":\"tool_use\",\"tool_name\":\"Bash\"}' \
                 '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"Final \
                 answer\",\"usage\":{\"input_tokens\":11,\"output_tokens\":7}}'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Claude,
            app_event_tx,
            cmd: command,
            db: database.clone(),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::ClaudeOpus46,
        })
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(!output_text.contains("Running a command"));
        assert!(!output_text.contains("tool_use"));
        assert_eq!(output_text.matches("Final answer").count(), 1);
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, 11);
        assert_eq!(sessions[0].output_tokens, 7);
    }

    #[tokio::test]
    /// Verifies Gemini progress events stay compact while final response and
    /// token stats are persisted.
    async fn test_run_agent_assist_task_streams_gemini_output_with_compact_progress() {
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
                "gemini-3-flash-preview",
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
                 '{\"type\":\"tool_use\",\"tool_name\":\"google_search\",\"tool_id\":\"tool-1\",\"\
                 parameters\":{}}' \
                 '{\"type\":\"message\",\"role\":\"assistant\",\"content\":\"Final \
                 answer\",\"delta\":true}' \
                 '{\"type\":\"result\",\"status\":\"success\",\"stats\":{\"input_tokens\":11,\"\
                 output_tokens\":7}}'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Gemini,
            app_event_tx,
            cmd: command,
            db: database.clone(),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::Gemini3FlashPreview,
        })
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(!output_text.contains("Searching the web"));
        assert!(!output_text.contains("google_search"));
        assert_eq!(output_text.matches("Final answer").count(), 1);
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, 11);
        assert_eq!(sessions[0].output_tokens, 7);
    }

    #[tokio::test]
    /// Ensures repeated progress updates are de-duplicated in emitted app
    /// events.
    async fn test_run_agent_assist_task_deduplicates_repeated_progress_updates() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-id", "gpt-5.3-codex", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));

        let mut command = Command::new("sh");
        command
            .args([
                "-lc",
                "printf '%s\\n' '{\"type\":\"item.started\",\"item\":{\"type\":\"web_search\"}}' \
                 '{\"type\":\"item.started\",\"item\":{\"type\":\"web_search\"}}' \
                 '{\"type\":\"item.started\",\"item\":{\"type\":\"web_search\"}}' \
                 '{\"type\":\"item.started\",\"item\":{\"type\":\"reasoning\"}}' \
                 '{\"type\":\"item.started\",\"item\":{\"type\":\"reasoning\"}}' \
                 '{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\"}}' \
                 '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"\
                 Final answer\"}}' \
                 '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":11,\"output_tokens\":\
                 7}}'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Act
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Codex,
            app_event_tx,
            cmd: command,
            db: database,
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::Gpt53Codex,
        })
        .await;
        let mut progress_updates = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } = event
                && session_id == "session-id"
            {
                progress_updates.push(progress_message);
            }
        }

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        assert_eq!(
            progress_updates
                .iter()
                .filter(|entry| entry.as_deref() == Some("Searching the web"))
                .count(),
            1
        );
        assert_eq!(
            progress_updates
                .iter()
                .filter(|entry| entry.as_deref() == Some("Thinking"))
                .count(),
            1
        );
        assert_eq!(
            progress_updates
                .iter()
                .filter(|entry| entry.as_deref() == Some("Running a command"))
                .count(),
            1
        );
        assert_eq!(progress_updates.last(), Some(&None));
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(!output_text.contains("Web search completed"));
        assert!(!output_text.contains("Thinking completed"));
        assert!(!output_text.contains("Command completed"));
        assert!(!output_text.contains("Searching the web"));
        assert!(!output_text.contains("Running a command"));
    }

    #[tokio::test]
    /// Verifies non-zero child exits surface both exit code and stderr text.
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
        let result = SessionTaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: AgentKind::Claude,
            app_event_tx,
            cmd: command,
            db: database,
            id: "session-id".to_string(),
            output,
            session_model: AgentModel::ClaudeOpus46,
        })
        .await;

        // Assert
        assert!(result.is_err());
        let error_text = result.expect_err("expected non-zero exit to fail");
        assert!(error_text.contains("exit code 7"));
        assert!(error_text.contains("assist failed"));
    }
}
