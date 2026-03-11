//! Session task execution helpers for process running, output capture, and
//! status persistence.

use std::path::Path;
use std::sync::{Arc, Mutex};

use askama::Template;
use tokio::sync::mpsc;

use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, format_detail_lines,
    run_agent_assist,
};
use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::AgentModel;
use crate::domain::session::Status;
use crate::infra::agent;
use crate::infra::db::Database;
use crate::infra::git::{self as git, GitClient};

const AUTO_COMMIT_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 10,
    max_identical_failure_streak: 3,
};
const FALLBACK_SESSION_COMMIT_MESSAGE: &str = "Apply session updates";

/// Askama view model for rendering auto-commit recovery prompts.
#[derive(Template)]
#[template(path = "auto_commit_assist_prompt.md", escape = "none")]
struct AutoCommitAssistPromptTemplate<'a> {
    commit_error: &'a str,
}

/// Askama view model for rendering session commit-message generation prompts.
#[derive(Template)]
#[template(path = "session_commit_message_prompt.md", escape = "none")]
struct SessionCommitMessagePromptTemplate<'a> {
    current_commit_message: &'a str,
    diff: &'a str,
}

/// Stateless helpers for session process execution and output handling.
pub(crate) struct SessionTaskService;

/// Generated session commit details for one successful auto-commit run.
pub(crate) struct SessionCommitOutcome {
    /// Short hash of the rewritten or created `HEAD` commit.
    pub(crate) commit_hash: String,
    /// Canonical commit title/body stored on the session branch `HEAD`.
    pub(crate) commit_message: String,
}

/// Inputs needed to execute an agent-assisted edit task.
pub(crate) struct RunAgentAssistTaskInput {
    /// App event sender used for progress and status updates.
    pub(crate) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Shared process identifier slot used for cancellation.
    pub(crate) child_pid: Arc<Mutex<Option<u32>>>,
    /// Database handle used for output/status persistence.
    pub(crate) db: Database,
    /// Session worktree folder where the assist prompt runs.
    pub(crate) folder: std::path::PathBuf,
    /// Session identifier for persisted updates.
    pub(crate) id: String,
    /// Shared output buffer receiving incremental output.
    pub(crate) output: Arc<Mutex<String>>,
    /// One-shot assist prompt submitted to the agent.
    pub(crate) prompt: String,
    /// Session model used for agent metadata and parsing.
    pub(crate) session_model: AgentModel,
}

impl SessionTaskService {
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

    /// Commits pending worktree changes and appends user-visible outcomes.
    ///
    /// Successful commit hashes, no-op commit notices, and commit errors are
    /// emitted into session output for user visibility.
    pub(in crate::app) async fn handle_auto_commit(context: AssistContext) {
        match Self::commit_changes_with_assist(&context).await {
            Ok(Some(outcome)) => {
                SessionManager::update_session_title_and_summary_from_commit_message(
                    &context.db,
                    &context.id,
                    &outcome.commit_message,
                    &context.app_event_tx,
                )
                .await;

                let message = format!("\n[Commit] committed with hash `{}`\n", outcome.commit_hash);
                Self::append_session_output(
                    &context.output,
                    &context.db,
                    &context.app_event_tx,
                    &context.id,
                    &message,
                )
                .await;
            }
            Ok(None) => {
                let message = "\n[Commit] No changes to commit.\n";
                Self::append_session_output(
                    &context.output,
                    &context.db,
                    &context.app_event_tx,
                    &context.id,
                    message,
                )
                .await;
            }
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

    async fn commit_changes_with_assist(
        context: &AssistContext,
    ) -> Result<Option<SessionCommitOutcome>, String> {
        let mut failure_tracker =
            FailureTracker::new(AUTO_COMMIT_ASSIST_POLICY.max_identical_failure_streak);
        // Test repos do not install hooks deterministically; skip hook
        // execution in tests to keep auto-commit behavior stable.
        let skip_verify_hooks = cfg!(test);

        for assist_attempt in 1..=AUTO_COMMIT_ASSIST_POLICY.max_attempts + 1 {
            match Self::commit_changes_with_git_client(context, skip_verify_hooks).await {
                Ok(commit_outcome) => {
                    return Ok(Some(commit_outcome));
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
    /// Returns an error if commit-message generation, staging/commit, or
    /// `HEAD` resolution fails.
    async fn commit_changes_with_git_client(
        context: &AssistContext,
        no_verify: bool,
    ) -> Result<SessionCommitOutcome, String> {
        let base_branch = context
            .db
            .get_session_base_branch(&context.id)
            .await?
            .ok_or_else(|| "Missing session base branch for auto-commit".to_string())?;

        Self::commit_session_changes(
            context.git_client.as_ref(),
            &context.folder,
            &base_branch,
            context.session_model,
            no_verify,
        )
        .await
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
        let prompt = Self::auto_commit_assist_prompt(commit_error)?;
        let assist_context = AssistContext {
            app_event_tx: context.app_event_tx.clone(),
            child_pid: Arc::clone(&context.child_pid),
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

    /// Renders the commit-assistance prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error if Askama template rendering fails.
    fn auto_commit_assist_prompt(commit_error: &str) -> Result<String, String> {
        let commit_error = commit_error.trim();
        let template = AutoCommitAssistPromptTemplate { commit_error };

        template
            .render()
            .map_err(|error| format!("Failed to render `auto_commit_assist_prompt.md`: {error}"))
    }

    fn format_commit_error_for_display(commit_error: &str) -> String {
        format_detail_lines(commit_error)
    }

    /// Renders the commit-message generation prompt from the markdown
    /// template.
    ///
    /// # Errors
    /// Returns an error if Askama template rendering fails.
    fn session_commit_message_prompt(
        diff: &str,
        current_commit_message: Option<&str>,
    ) -> Result<String, String> {
        let template = SessionCommitMessagePromptTemplate {
            current_commit_message: current_commit_message.unwrap_or("").trim(),
            diff,
        };

        template.render().map_err(|error| {
            format!("Failed to render `session_commit_message_prompt.md`: {error}")
        })
    }

    /// Generates the canonical session commit message, commits the current
    /// worktree state, and returns the rewritten `HEAD` details.
    ///
    /// # Errors
    /// Returns an error if the worktree is clean, the cumulative session diff
    /// cannot be generated, commit-message generation fails, or the git commit
    /// cannot be created/amended.
    pub(crate) async fn commit_session_changes(
        git_client: &dyn GitClient,
        folder: &Path,
        base_branch: &str,
        session_model: AgentModel,
        no_verify: bool,
    ) -> Result<SessionCommitOutcome, String> {
        if cfg!(test) {
            let folder = folder.to_path_buf();
            if git_client.is_worktree_clean(folder.clone()).await? {
                return Err("Nothing to commit: no changes detected".to_string());
            }

            let has_session_commit = git_client
                .has_commits_since(folder.clone(), base_branch.to_string())
                .await?;
            let current_commit_message = if has_session_commit {
                git_client.head_commit_message(folder.clone()).await?
            } else {
                None
            };
            let commit_message =
                Self::fallback_session_commit_message(current_commit_message.as_deref());
            git_client
                .commit_all_preserving_single_commit(
                    folder.clone(),
                    base_branch.to_string(),
                    commit_message.clone(),
                    git::SingleCommitMessageStrategy::Replace,
                    no_verify,
                )
                .await?;
            let commit_hash = git_client.head_short_hash(folder).await?;

            return Ok(SessionCommitOutcome {
                commit_hash,
                commit_message,
            });
        }

        let backend = agent::create_backend(session_model.kind());

        Self::commit_session_changes_with_backend(
            git_client,
            folder,
            base_branch,
            session_model,
            backend.as_ref(),
            no_verify,
        )
        .await
    }

    /// Testable variant of [`SessionTaskService::commit_session_changes`] that
    /// accepts an injected backend for deterministic prompt generation.
    ///
    /// # Errors
    /// Returns an error if the worktree is clean, the cumulative session diff
    /// cannot be generated, commit-message generation fails, or the git commit
    /// cannot be created/amended.
    async fn commit_session_changes_with_backend(
        git_client: &dyn GitClient,
        folder: &Path,
        base_branch: &str,
        session_model: AgentModel,
        backend: &dyn agent::AgentBackend,
        no_verify: bool,
    ) -> Result<SessionCommitOutcome, String> {
        let folder = folder.to_path_buf();
        if git_client.is_worktree_clean(folder.clone()).await? {
            return Err("Nothing to commit: no changes detected".to_string());
        }

        let diff = git_client
            .diff(folder.clone(), base_branch.to_string())
            .await?;
        let has_session_commit = git_client
            .has_commits_since(folder.clone(), base_branch.to_string())
            .await?;
        let current_commit_message = if has_session_commit {
            git_client.head_commit_message(folder.clone()).await?
        } else {
            None
        };
        let generated_commit_message = Self::generate_session_commit_message_with_backend(
            folder.as_path(),
            session_model,
            diff.as_str(),
            current_commit_message.as_deref(),
            backend,
        )
        .await
        .unwrap_or_else(|_| {
            Self::fallback_session_commit_message(current_commit_message.as_deref())
        });

        git_client
            .commit_all_preserving_single_commit(
                folder.clone(),
                base_branch.to_string(),
                generated_commit_message.clone(),
                git::SingleCommitMessageStrategy::Replace,
                no_verify,
            )
            .await?;

        let commit_hash = git_client.head_short_hash(folder).await?;

        Ok(SessionCommitOutcome {
            commit_hash,
            commit_message: generated_commit_message,
        })
    }

    /// Renders the session commit-message prompt, submits it to the injected
    /// backend, and returns the normalized commit message text.
    ///
    /// # Errors
    /// Returns an error when prompt rendering fails, the one-shot agent call
    /// fails, or the returned `answer` text is blank.
    async fn generate_session_commit_message_with_backend(
        folder: &Path,
        session_model: AgentModel,
        diff: &str,
        current_commit_message: Option<&str>,
        backend: &dyn agent::AgentBackend,
    ) -> Result<String, String> {
        let prompt = Self::session_commit_message_prompt(diff, current_commit_message)?;
        let submission = agent::submit_one_shot_with_backend(
            backend,
            agent::OneShotRequest {
                child_pid: None,
                folder,
                model: session_model,
                prompt: &prompt,
                reasoning_level: crate::domain::agent::ReasoningLevel::default(),
            },
        )
        .await?;
        let answer_text = submission.response.to_answer_display_text();
        let trimmed_answer_text = answer_text.trim();
        if trimmed_answer_text.is_empty() {
            return Err("Session commit message model returned blank answer text".to_string());
        }

        Ok(trimmed_answer_text.to_string())
    }

    /// Returns the last good session commit message or a stable fallback when
    /// generation fails.
    fn fallback_session_commit_message(current_commit_message: Option<&str>) -> String {
        let Some(current_commit_message) = current_commit_message.map(str::trim) else {
            return FALLBACK_SESSION_COMMIT_MESSAGE.to_string();
        };
        if current_commit_message.is_empty() {
            return FALLBACK_SESSION_COMMIT_MESSAGE.to_string();
        }

        current_commit_message.to_string()
    }

    /// Executes one isolated assist prompt and appends the normalized answer
    /// text to the session transcript.
    ///
    /// # Errors
    /// Returns an error when the one-shot prompt fails or returns invalid
    /// protocol output.
    pub(crate) async fn run_agent_assist_task(
        input: RunAgentAssistTaskInput,
    ) -> Result<(), String> {
        let backend = agent::create_backend(input.session_model.kind());

        Self::run_agent_assist_task_with_backend(input, backend.as_ref()).await
    }

    /// Executes one isolated assist prompt using the provided backend.
    ///
    /// # Errors
    /// Returns an error when the one-shot prompt fails or returns invalid
    /// protocol output.
    async fn run_agent_assist_task_with_backend(
        input: RunAgentAssistTaskInput,
        backend: &dyn agent::AgentBackend,
    ) -> Result<(), String> {
        let RunAgentAssistTaskInput {
            app_event_tx,
            child_pid,
            db,
            folder,
            id,
            output,
            prompt,
            session_model,
        } = input;
        let assist_submission = agent::submit_one_shot_with_backend(
            backend,
            agent::OneShotRequest {
                child_pid: Some(child_pid.as_ref()),
                folder: &folder,
                model: session_model,
                prompt: &prompt,
                reasoning_level: crate::domain::agent::ReasoningLevel::default(),
            },
        )
        .await
        .inspect_err(|_error| {
            Self::clear_session_progress(&app_event_tx, &id);
        })?;

        let answer_text = assist_submission.response.to_answer_display_text();
        if !answer_text.trim().is_empty() {
            Self::append_session_output(&output, &db, &app_event_tx, &id, &answer_text).await;
        }

        let _ = db.update_session_stats(&id, &assist_submission.stats).await;
        let _ = db
            .upsert_session_usage(&id, session_model.as_str(), &assist_submission.stats)
            .await;

        Self::clear_session_progress(&app_event_tx, &id);

        Ok(())
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

    /// Clears the transient progress message for one session.
    pub(crate) fn clear_session_progress(app_event_tx: &mpsc::UnboundedSender<AppEvent>, id: &str) {
        Self::set_session_progress(app_event_tx, id, None);
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
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;
    use crate::db::Database;
    use crate::infra::agent::AgentCommandMode;
    use crate::infra::agent::tests::MockAgentBackend;
    use crate::infra::git::MockGitClient;

    /// Builds one deterministic shell command used by mocked backends.
    fn mock_shell_command(stdout: &str, stderr: &str, exit_code: i32) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "printf '%s' \"$ASSIST_STDOUT\"; printf '%s' \"$ASSIST_STDERR\" >&2; exit \
             \"$ASSIST_EXIT\"",
        );
        command.env("ASSIST_STDOUT", stdout);
        command.env("ASSIST_STDERR", stderr);
        command.env("ASSIST_EXIT", exit_code.to_string());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        command
    }

    /// Inserts one review session used by assist-task tests.
    async fn insert_review_session(database: &Database, model: &str) {
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-id", model, "main", "Review", project_id)
            .await
            .expect("failed to insert session");
    }

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
        let prompt = SessionTaskService::auto_commit_assist_prompt(commit_error)
            .expect("auto commit assist prompt should render");

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

    #[test]
    /// Verifies session commit-message prompts include both the continuity
    /// input and the cumulative diff.
    fn test_session_commit_message_prompt_includes_continuity_and_diff() {
        // Arrange
        let diff = "diff --git a/a.rs b/a.rs";
        let current_commit_message = Some("Keep session commit accurate");

        // Act
        let prompt =
            SessionTaskService::session_commit_message_prompt(diff, current_commit_message)
                .expect("prompt should render");

        // Assert
        assert!(prompt.contains("Keep session commit accurate"));
        assert!(prompt.contains(diff));
    }

    #[tokio::test]
    /// Verifies commit helper failure appends a commit error message without
    /// invoking real git or agent subprocesses.
    async fn test_handle_auto_commit_appends_commit_error_from_mock_git_client() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Err("commit failed".to_string()) }));
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        insert_review_session(&database, AgentModel::Gpt53Codex.as_str()).await;
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let context = AssistContext {
            app_event_tx,
            child_pid: Arc::new(Mutex::new(None)),
            db: database,
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
    /// Verifies auto-commit reports clean-worktree no-op commits in the
    /// session output.
    async fn test_handle_auto_commit_reports_when_no_changes_exist() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        insert_review_session(&database, AgentModel::Gpt53Codex.as_str()).await;
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let context = AssistContext {
            app_event_tx,
            child_pid: Arc::new(Mutex::new(None)),
            db: database,
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
        assert!(output_text.contains("[Commit] No changes to commit."));
    }

    #[tokio::test]
    /// Verifies one-shot assist output unwraps structured protocol answers
    /// before persistence and session usage updates.
    async fn test_run_agent_assist_task_unwraps_one_shot_answer_without_raw_json() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        insert_review_session(&database, AgentModel::ClaudeOpus46.as_str()).await;
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let child_pid = Arc::new(Mutex::new(None));
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().times(1).returning(|request| {
            assert!(matches!(
                request.mode,
                AgentCommandMode::OneShot {
                    prompt: "Resolve conflict",
                }
            ));

            Ok(mock_shell_command(
                r#"{"result":"{\"messages\":[{\"type\":\"answer\",\"text\":\"Resolved the rebase conflict.\"}]}","usage":{"input_tokens":11,"output_tokens":7}}"#,
                "",
                0,
            ))
        });

        // Act
        let result = SessionTaskService::run_agent_assist_task_with_backend(
            RunAgentAssistTaskInput {
                app_event_tx,
                child_pid: Arc::clone(&child_pid),
                db: database.clone(),
                folder: temp_dir.path().to_path_buf(),
                id: "session-id".to_string(),
                output: Arc::clone(&output),
                prompt: "Resolve conflict".to_string(),
                session_model: AgentModel::ClaudeOpus46,
            },
            &backend,
        )
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed: {:?}",
            result.err()
        );
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(output_text.contains("Resolved the rebase conflict."));
        assert!(!output_text.contains(r#"{"messages""#));
        assert_eq!(*child_pid.lock().expect("failed to lock child pid"), None);
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, 11);
        assert_eq!(sessions[0].output_tokens, 7);
    }

    #[tokio::test]
    /// Verifies assist-task usage accounting includes the initial attempt and
    /// any protocol-repair turns.
    async fn test_run_agent_assist_task_accumulates_stats_across_protocol_repair_attempts() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        insert_review_session(&database, AgentModel::ClaudeOpus46.as_str()).await;
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let build_call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().times(2).returning({
            let build_call_count = Arc::clone(&build_call_count);

            move |request| {
                let attempt =
                    build_call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                match attempt {
                    0 => {
                        assert!(matches!(
                            request.mode,
                            AgentCommandMode::OneShot {
                                prompt: "Resolve conflict",
                            }
                        ));

                        Ok(mock_shell_command(
                            r#"{"result":"plain text","usage":{"input_tokens":2,"output_tokens":1}}"#,
                            "",
                            0,
                        ))
                    }
                    1 => {
                        let prompt = match request.mode {
                            AgentCommandMode::OneShot { prompt } => prompt,
                            _ => "",
                        };
                        assert!(
                            prompt.contains(
                                "Your previous response did not match the required JSON schema."
                            ),
                            "repair prompt should explain the protocol failure"
                        );

                        Ok(mock_shell_command(
                            r#"{"result":"{\"messages\":[{\"type\":\"answer\",\"text\":\"Recovered conflict resolution.\"}]}","usage":{"input_tokens":3,"output_tokens":2}}"#,
                            "",
                            0,
                        ))
                    }
                    _ => unreachable!("unexpected extra backend call"),
                }
            }
        });

        // Act
        let result = SessionTaskService::run_agent_assist_task_with_backend(
            RunAgentAssistTaskInput {
                app_event_tx,
                child_pid: Arc::new(Mutex::new(None)),
                db: database.clone(),
                folder: temp_dir.path().to_path_buf(),
                id: "session-id".to_string(),
                output: Arc::clone(&output),
                prompt: "Resolve conflict".to_string(),
                session_model: AgentModel::ClaudeOpus46,
            },
            &backend,
        )
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "assist task should succeed after repair: {:?}",
            result.err()
        );
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(output_text.contains("Recovered conflict resolution."));
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, 5);
        assert_eq!(sessions[0].output_tokens, 3);
    }

    #[tokio::test]
    /// Verifies non-zero assist subprocess exits surface the one-shot command
    /// error details.
    async fn test_run_agent_assist_task_returns_error_for_non_zero_exit_status() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        insert_review_session(&database, AgentModel::ClaudeOpus46.as_str()).await;
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|_| Ok(mock_shell_command("", "assist failed", 7)));

        // Act
        let result = SessionTaskService::run_agent_assist_task_with_backend(
            RunAgentAssistTaskInput {
                app_event_tx,
                child_pid: Arc::new(Mutex::new(None)),
                db: database,
                folder: temp_dir.path().to_path_buf(),
                id: "session-id".to_string(),
                output: Arc::new(Mutex::new(String::new())),
                prompt: "Resolve conflict".to_string(),
                session_model: AgentModel::ClaudeOpus46,
            },
            &backend,
        )
        .await;

        // Assert
        assert!(result.is_err());
        let error_text = result.expect_err("expected non-zero exit to fail");
        assert!(error_text.contains("exit code 7"));
        assert!(error_text.contains("assist failed"));
    }
}
