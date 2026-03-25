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
use crate::domain::session::{SessionSize, Status};
use crate::domain::setting::SettingName;
use crate::infra::agent;
use crate::infra::db::Database;
use crate::infra::git::{self as git, GitClient};

const AUTO_COMMIT_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 10,
    max_identical_failure_streak: 3,
};
const SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER: &str =
    "Co-Authored-By: [Agentty](https://github.com/agentty-xyz/agentty)";

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
    ///
    /// Returns the recomputed bucket when the base-branch lookup and
    /// persistence both succeed.
    pub(crate) async fn refresh_persisted_session_size(
        db: &Database,
        git_client: &dyn GitClient,
        session_id: &str,
        folder: &Path,
    ) -> Option<SessionSize> {
        let base_branch = db
            .get_session_base_branch(session_id)
            .await
            .ok()
            .flatten()?;

        let computed_size =
            SessionManager::session_size_for_folder(git_client, folder, &base_branch).await;
        db.update_session_size(session_id, &computed_size.to_string())
            .await
            .ok()?;

        Some(computed_size)
    }

    /// Commits pending worktree changes and appends user-visible outcomes.
    ///
    /// Successful commit hashes, no-op commit notices, and commit errors are
    /// emitted into session output for user visibility.
    pub(in crate::app) async fn handle_auto_commit(context: AssistContext) {
        match Self::commit_changes_with_assist(&context).await {
            Ok(Some(outcome)) => {
                SessionManager::update_session_title_from_commit_message(
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

    /// Loads the project-scoped toggle that controls whether generated session
    /// commit messages include the Agentty coauthor trailer.
    pub(crate) async fn load_include_coauthored_by_agentty_setting(
        db: &Database,
        session_id: &str,
    ) -> bool {
        let Some(project_id) = db.load_session_project_id(session_id).await.ok().flatten() else {
            return true;
        };

        db.get_project_setting(project_id, SettingName::IncludeCoauthoredByAgentty.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|setting_value| setting_value.parse::<bool>().ok())
            .unwrap_or(true)
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
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Missing session base branch for auto-commit".to_string())?;

        Self::commit_session_changes(
            context.git_client.as_ref(),
            &context.folder,
            &base_branch,
            context.session_model,
            no_verify,
            Self::load_include_coauthored_by_agentty_setting(&context.db, &context.id).await,
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
        let stripped_current_commit_message =
            current_commit_message.map_or_else(String::new, strip_agentty_coauthor_trailer);
        let template = SessionCommitMessagePromptTemplate {
            current_commit_message: stripped_current_commit_message.trim(),
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
        include_coauthored_by_agentty: bool,
    ) -> Result<SessionCommitOutcome, String> {
        if cfg!(test) {
            let folder = folder.to_path_buf();
            if git_client
                .is_worktree_clean(folder.clone())
                .await
                .map_err(|error| error.to_string())?
            {
                return Err("Nothing to commit: no changes detected".to_string());
            }

            let has_session_commit = git_client
                .has_commits_since(folder.clone(), base_branch.to_string())
                .await
                .map_err(|error| error.to_string())?;
            let current_commit_message = if has_session_commit {
                git_client
                    .head_commit_message(folder.clone())
                    .await
                    .map_err(|error| error.to_string())?
            } else {
                None
            };
            let Some(current_commit_message) = current_commit_message.as_deref().map(str::trim)
            else {
                return Err(
                    "Session commit generation requires an existing commit message during tests"
                        .to_string(),
                );
            };
            if current_commit_message.is_empty() {
                return Err(
                    "Session commit generation requires a non-blank existing commit message \
                     during tests"
                        .to_string(),
                );
            }
            let commit_message = append_agentty_coauthor_trailer(
                strip_agentty_coauthor_trailer(current_commit_message).trim(),
                include_coauthored_by_agentty,
            );
            git_client
                .commit_all_preserving_single_commit(
                    folder.clone(),
                    base_branch.to_string(),
                    commit_message.clone(),
                    git::SingleCommitMessageStrategy::Replace,
                    no_verify,
                )
                .await
                .map_err(|error| error.to_string())?;
            let commit_hash = git_client
                .head_short_hash(folder)
                .await
                .map_err(|error| error.to_string())?;

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
            include_coauthored_by_agentty,
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
        include_coauthored_by_agentty: bool,
    ) -> Result<SessionCommitOutcome, String> {
        let folder = folder.to_path_buf();
        if git_client
            .is_worktree_clean(folder.clone())
            .await
            .map_err(|error| error.to_string())?
        {
            return Err("Nothing to commit: no changes detected".to_string());
        }

        let diff = git_client
            .diff(folder.clone(), base_branch.to_string())
            .await
            .map_err(|error| error.to_string())?;
        let has_session_commit = git_client
            .has_commits_since(folder.clone(), base_branch.to_string())
            .await
            .map_err(|error| error.to_string())?;
        let current_commit_message = if has_session_commit {
            git_client
                .head_commit_message(folder.clone())
                .await
                .map_err(|error| error.to_string())?
        } else {
            None
        };
        let generated_commit_message = Self::generate_session_commit_message_with_backend(
            folder.as_path(),
            session_model,
            diff.as_str(),
            current_commit_message.as_deref(),
            backend,
            include_coauthored_by_agentty,
        )
        .await?;

        git_client
            .commit_all_preserving_single_commit(
                folder.clone(),
                base_branch.to_string(),
                generated_commit_message.clone(),
                git::SingleCommitMessageStrategy::Replace,
                no_verify,
            )
            .await
            .map_err(|error| error.to_string())?;

        let commit_hash = git_client
            .head_short_hash(folder)
            .await
            .map_err(|error| error.to_string())?;

        Ok(SessionCommitOutcome {
            commit_hash,
            commit_message: generated_commit_message,
        })
    }

    /// Renders the session commit-message prompt, submits it to the injected
    /// backend, validates the returned text, and appends the optional
    /// coauthor trailer in code.
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
        include_coauthored_by_agentty: bool,
    ) -> Result<String, String> {
        let prompt = Self::session_commit_message_prompt(diff, current_commit_message)?;
        let submission = Self::submit_utility_prompt_with_backend(
            session_model,
            backend,
            agent::OneShotRequest {
                child_pid: None,
                folder,
                model: session_model,
                prompt: &prompt,
                request_kind: crate::infra::channel::AgentRequestKind::UtilityPrompt,
                reasoning_level: crate::domain::agent::ReasoningLevel::default(),
            },
        )
        .await?;
        let answer_text = submission.response.to_answer_display_text();
        let trimmed_answer_text = answer_text.trim();
        if trimmed_answer_text.is_empty() {
            return Err("Session commit message model returned blank answer text".to_string());
        }
        validate_generated_commit_message(trimmed_answer_text)?;

        Ok(append_agentty_coauthor_trailer(
            trimmed_answer_text,
            include_coauthored_by_agentty,
        ))
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
        let assist_submission = Self::submit_utility_prompt_with_backend(
            session_model,
            backend,
            agent::OneShotRequest {
                child_pid: Some(child_pid.as_ref()),
                folder: &folder,
                model: session_model,
                prompt: &prompt,
                request_kind: crate::infra::channel::AgentRequestKind::UtilityPrompt,
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

    /// Executes one isolated utility prompt, routing app-server-backed models
    /// through the shared app-server client while preserving backend injection
    /// for direct CLI providers in tests and production.
    ///
    /// # Errors
    /// Returns an error when the one-shot prompt fails or the response does
    /// not satisfy the structured protocol schema.
    async fn submit_utility_prompt_with_backend(
        session_model: AgentModel,
        backend: &dyn agent::AgentBackend,
        request: agent::OneShotRequest<'_>,
    ) -> Result<agent::OneShotSubmission, String> {
        if agent::transport_mode(session_model.kind()).uses_app_server() {
            let app_server_client = agent::create_app_server_client(session_model.kind(), None)
                .ok_or_else(|| {
                    format!(
                        "{} provider did not provide an app-server client",
                        session_model.kind()
                    )
                })?;

            return agent::submit_one_shot_with_app_server_client(
                app_server_client.as_ref(),
                request,
            )
            .await;
        }

        agent::submit_one_shot_with_backend(backend, request).await
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

/// Removes the Agentty coauthor trailer from one commit message so prompt
/// continuity and test-mode reuse operate on body/title content only.
fn strip_agentty_coauthor_trailer(commit_message: &str) -> String {
    commit_message
        .lines()
        .filter(|line| line.trim() != SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Validates generated commit-message output before git commit creation.
///
/// # Errors
/// Returns an error when the generated message already contains the Agentty
/// coauthor trailer, which is appended by code instead of model output.
fn validate_generated_commit_message(commit_message: &str) -> Result<(), String> {
    if commit_message
        .lines()
        .any(|line| line.trim() == SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER)
    {
        return Err(
            "Session commit message model must not emit the Agentty coauthor trailer".to_string(),
        );
    }

    Ok(())
}

/// Appends the Agentty coauthor trailer when the project setting enables it.
fn append_agentty_coauthor_trailer(
    commit_message: &str,
    include_coauthored_by_agentty: bool,
) -> String {
    let trimmed_commit_message = commit_message.trim().to_string();

    if !include_coauthored_by_agentty || trimmed_commit_message.is_empty() {
        return trimmed_commit_message;
    }

    format!("{trimmed_commit_message}\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;
    use crate::db::Database;
    use crate::infra::agent::tests::MockAgentBackend;
    use crate::infra::channel::AgentRequestKind;
    use crate::infra::git::{GitError, MockGitClient};

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
        assert!(prompt.contains("return the required protocol JSON object"));
        assert!(prompt.contains("`answer` field only"));
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
        assert!(prompt.contains("required protocol JSON object"));
        assert!(!prompt.contains("Return one plain-text commit message"));
        assert!(!prompt.contains(SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER));
    }

    #[test]
    /// Verifies prompt rendering strips the Agentty trailer from existing
    /// commit-message continuity before sending it back to the model.
    fn test_session_commit_message_prompt_strips_coauthor_trailer_from_continuity() {
        // Arrange
        let diff = "diff --git a/a.rs b/a.rs";
        let current_commit_message = format!(
            "Keep session commit accurate\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}"
        );

        // Act
        let prompt = SessionTaskService::session_commit_message_prompt(
            diff,
            Some(current_commit_message.as_str()),
        )
        .expect("prompt should render");

        // Assert
        assert!(!prompt.contains(SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER));
        assert!(prompt.contains("Keep session commit accurate"));
    }

    #[tokio::test]
    /// Verifies plain-text one-shot output is rejected for session commit
    /// message generation.
    async fn test_generate_session_commit_message_with_backend_rejects_plain_text_output() {
        // Arrange — use a CLI-backed model so the mock backend is exercised.
        // App-server-backed models (Codex, Gemini) bypass `build_command`
        // entirely and route through the shared app-server client.
        let temp_directory = tempfile::tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));
                assert!(
                    request
                        .prompt
                        .contains("Generate the canonical session commit message")
                );

                Ok(mock_shell_command(
                    "Refactor agent prompt and protocol handling",
                    "",
                    0,
                ))
            });

        // Act
        let error = SessionTaskService::generate_session_commit_message_with_backend(
            temp_directory.path(),
            AgentModel::ClaudeSonnet46,
            "diff --git a/a.rs b/a.rs",
            None,
            &backend,
            false,
        )
        .await
        .expect_err("plain-text one-shot commit message should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\nRefactor agent prompt and protocol handling"));
    }

    #[test]
    /// Verifies append-only handling adds the coauthor trailer once
    /// when the setting is enabled.
    fn test_append_agentty_coauthor_trailer_appends_trailer_once() {
        // Arrange
        let commit_message = "Refine settings page";

        // Act
        let appended_commit_message = append_agentty_coauthor_trailer(commit_message, true);

        // Assert
        assert_eq!(
            appended_commit_message,
            format!("Refine settings page\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}")
        );
    }

    #[test]
    /// Verifies append-only handling leaves the generated message unchanged
    /// when the setting is disabled.
    fn test_append_agentty_coauthor_trailer_leaves_message_unchanged_when_disabled() {
        // Arrange
        let commit_message = "Refine settings page";

        // Act
        let appended_commit_message = append_agentty_coauthor_trailer(commit_message, false);

        // Assert
        assert_eq!(appended_commit_message, "Refine settings page");
    }

    #[test]
    /// Verifies generated commit-message validation rejects model output that
    /// already includes the Agentty trailer.
    fn test_validate_generated_commit_message_rejects_agentty_trailer() {
        // Arrange
        let commit_message =
            format!("Refine settings page\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}");

        // Act
        let error = validate_generated_commit_message(&commit_message)
            .expect_err("generated trailer should fail validation");

        // Assert
        assert_eq!(
            error,
            "Session commit message model must not emit the Agentty coauthor trailer"
        );
    }

    #[test]
    /// Verifies trailer stripping removes the Agentty trailer from reused
    /// commit-message continuity.
    fn test_strip_agentty_coauthor_trailer_removes_trailer_line() {
        // Arrange
        let commit_message =
            format!("Refine settings page\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}");

        // Act
        let stripped_commit_message = strip_agentty_coauthor_trailer(&commit_message);

        // Assert
        assert_eq!(stripped_commit_message, "Refine settings page\n");
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
            .returning(|_| {
                Box::pin(async { Err(GitError::OutputParse("commit failed".to_string())) })
            });
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
            .returning(|_| Box::pin(async { Ok::<_, GitError>(true) }));
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
    /// Verifies successful auto-commit updates the title while preserving the
    /// persisted agent session summary text.
    async fn test_handle_auto_commit_preserves_agent_session_summary() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok::<_, GitError>(false) }));
        mock_git_client
            .expect_has_commits_since()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok::<_, GitError>(true) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok::<_, GitError>(Some(
                        "Refine README updates\n\n- Keep title aligned with commit".to_string(),
                    ))
                })
            });
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .returning(|_, _, _, _, _| Box::pin(async { Ok::<_, GitError>(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .returning(|_| Box::pin(async { Ok::<_, GitError>("abc1234".to_string()) }));
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        insert_review_session(&database, AgentModel::Gpt53Codex.as_str()).await;
        let summary_payload = "- Session branch updates README formatting.".to_string();
        database
            .update_session_summary("session-id", &summary_payload)
            .await
            .expect("failed to persist summary text");
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let context = AssistContext {
            app_event_tx,
            child_pid: Arc::new(Mutex::new(None)),
            db: database.clone(),
            folder: PathBuf::from("/tmp/project"),
            git_client: Arc::new(mock_git_client),
            id: "session-id".to_string(),
            output: Arc::clone(&output),
            session_model: AgentModel::Gpt53Codex,
        };

        // Act
        SessionTaskService::handle_auto_commit(context).await;
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(sessions[0].title.as_deref(), Some("Refine README updates"));
        assert_eq!(
            sessions[0].summary.as_deref(),
            Some("- Session branch updates README formatting.")
        );
        let output_text = output
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default();
        assert!(output_text.contains("[Commit] committed with hash `abc1234`"));
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
                request.request_kind,
                AgentRequestKind::UtilityPrompt
            ));
            assert_eq!(request.prompt, "Resolve conflict");

            Ok(mock_shell_command(
                r#"{"result":"{\"answer\":\"Resolved the rebase conflict.\",\"questions\":[],\"summary\":null}","usage":{"input_tokens":11,"output_tokens":7}}"#,
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
        assert!(!output_text.contains(r#"{"answer""#));
        assert_eq!(*child_pid.lock().expect("failed to lock child pid"), None);
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, 11);
        assert_eq!(sessions[0].output_tokens, 7);
    }

    #[tokio::test]
    /// Verifies assist tasks reject plain-text one-shot output that does not
    /// satisfy the shared protocol schema.
    async fn test_run_agent_assist_task_rejects_plain_text_output() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        insert_review_session(&database, AgentModel::ClaudeOpus46.as_str()).await;
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Mutex::new(String::new()));
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let mut backend = MockAgentBackend::new();
        backend
            .expect_build_command()
            .times(1)
            .returning(|request| {
                assert!(matches!(
                    request.request_kind,
                    AgentRequestKind::UtilityPrompt
                ));
                assert_eq!(request.prompt, "Resolve conflict");

                Ok(mock_shell_command(
                    r#"{"result":"plain text","usage":{"input_tokens":2,"output_tokens":1}}"#,
                    "",
                    0,
                ))
            });

        // Act
        let error = SessionTaskService::run_agent_assist_task_with_backend(
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
        .await
        .expect_err("plain-text utility output should fail");

        // Assert
        assert!(error.contains("did not match the required JSON schema"));
        assert!(error.contains("response:\nplain text"));
        let output_text = output.lock().map(|buf| buf.clone()).unwrap_or_default();
        assert!(output_text.is_empty());
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(sessions[0].input_tokens, 0);
        assert_eq!(sessions[0].output_tokens, 0);
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
