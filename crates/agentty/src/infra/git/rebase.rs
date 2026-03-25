use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use tokio::task::spawn_blocking;

use super::error::GitError;
use super::repo::{
    command_output_detail, resolve_git_dir, run_git_command_output_sync,
    run_git_command_output_with_env_sync, run_git_command_sync,
};
use crate::{Sleeper, ThreadSleeper};

const GIT_INDEX_LOCK_RETRY_ATTEMPTS: usize = 5;
const GIT_INDEX_LOCK_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Executes git commands for rebase operations.
#[cfg_attr(test, mockall::automock)]
trait GitCommandRunner: Send + Sync {
    /// Runs a git command in `repo_path` with environment overrides.
    fn run_git_command_output_with_env<'argument>(
        &self,
        repo_path: &Path,
        args: &[&'argument str],
        environment: &[(&'argument str, &'argument str)],
    ) -> Result<Output, GitError>;
}

/// Git command runner backed by process execution.
struct ProcessGitCommandRunner;

impl GitCommandRunner for ProcessGitCommandRunner {
    fn run_git_command_output_with_env<'argument>(
        &self,
        repo_path: &Path,
        args: &[&'argument str],
        environment: &[(&'argument str, &'argument str)],
    ) -> Result<Output, GitError> {
        run_git_command_output_with_env_sync(repo_path, args, environment)
            .map_err(GitError::OutputParse)
    }
}

/// Result of attempting a rebase step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebaseStepResult {
    /// Rebase step completed successfully.
    Completed,
    /// Rebase step stopped because of merge conflicts.
    Conflict { detail: String },
}

/// Rebases the current branch onto `target_branch`.
///
/// If the rebase fails due to conflict, this function aborts it immediately so
/// the repository does not remain in an in-progress rebase state.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `target_branch` - Branch to rebase onto (e.g., `main`)
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if rebase fails, or aborting a conflicted rebase
/// also fails.
pub async fn rebase(repo_path: PathBuf, target_branch: String) -> Result<(), GitError> {
    match rebase_start(repo_path.clone(), target_branch.clone()).await? {
        RebaseStepResult::Completed => Ok(()),
        RebaseStepResult::Conflict { detail } => {
            let abort_suffix = match abort_rebase(repo_path).await {
                Ok(()) => String::new(),
                Err(error) => format!(" {error}"),
            };

            Err(GitError::CommandFailed {
                command: "git rebase".to_string(),
                stderr: format!("Failed to rebase onto {target_branch}: {detail}.{abort_suffix}"),
            })
        }
    }
}

/// Rebases the current branch onto `target_branch`.
///
/// Returns a conflict outcome when the rebase stops for manual resolution.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `target_branch` - Branch to rebase onto (e.g., `main`)
///
/// # Returns
/// A [`RebaseStepResult`] describing whether the rebase completed or
/// encountered conflicts.
///
/// # Errors
/// Returns a [`GitError`] for non-conflict git failures.
pub async fn rebase_start(
    repo_path: PathBuf,
    target_branch: String,
) -> Result<RebaseStepResult, GitError> {
    spawn_blocking(move || {
        let rebase_args = ["rebase", target_branch.as_str()];
        let output = run_git_command_with_index_lock_retry(&repo_path, &rebase_args, &[])?;

        if output.status.success() {
            return Ok(RebaseStepResult::Completed);
        }

        let detail = command_output_detail(&output.stdout, &output.stderr);
        if is_rebase_conflict(&detail) {
            return Ok(RebaseStepResult::Conflict { detail });
        }

        Err(GitError::CommandFailed {
            command: "git rebase".to_string(),
            stderr: format!("Failed to rebase onto {target_branch}: {detail}."),
        })
    })
    .await?
}

/// Continues an in-progress rebase.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// A [`RebaseStepResult`] describing whether the rebase completed or
/// encountered conflicts.
///
/// # Errors
/// Returns a [`GitError`] for non-conflict git failures.
pub async fn rebase_continue(repo_path: PathBuf) -> Result<RebaseStepResult, GitError> {
    spawn_blocking(move || {
        let output = run_git_command_with_index_lock_retry(
            &repo_path,
            &["rebase", "--continue"],
            &[("GIT_EDITOR", ":"), ("GIT_SEQUENCE_EDITOR", ":")],
        )?;

        if output.status.success() {
            return Ok(RebaseStepResult::Completed);
        }

        let detail = command_output_detail(&output.stdout, &output.stderr);
        if is_rebase_conflict(&detail) {
            return Ok(RebaseStepResult::Conflict { detail });
        }

        Err(GitError::CommandFailed {
            command: "git rebase --continue".to_string(),
            stderr: format!("Failed to continue rebase: {detail}."),
        })
    })
    .await?
}

/// Aborts an in-progress rebase.
///
/// When git reports stale or inconsistent rebase metadata and abort cannot
/// complete normally, this helper removes stale `rebase-merge`/`rebase-apply`
/// paths as a recovery fallback.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] when `git rebase --abort` cannot be executed.
pub async fn abort_rebase(repo_path: PathBuf) -> Result<(), GitError> {
    spawn_blocking(move || {
        let output =
            run_git_command_with_index_lock_retry(&repo_path, &["rebase", "--abort"], &[])?;

        if !output.status.success() {
            let detail = command_output_detail(&output.stdout, &output.stderr);
            if !is_stale_or_inactive_rebase_error(&detail) {
                return Err(GitError::CommandFailed {
                    command: "git rebase --abort".to_string(),
                    stderr: format!("Failed to abort rebase: {detail}."),
                });
            }

            let cleaned_stale_metadata = clean_stale_rebase_metadata(&repo_path)?;
            if cleaned_stale_metadata {
                return Ok(());
            }

            return Err(GitError::CommandFailed {
                command: "git rebase --abort".to_string(),
                stderr: format!("Failed to abort rebase: {detail}."),
            });
        }

        Ok(())
    })
    .await?
}

/// Returns whether a rebase is currently in progress in the repository or
/// worktree.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// `true` when `.git/rebase-merge` or `.git/rebase-apply` exists, `false`
/// otherwise.
///
/// # Errors
/// Returns a [`GitError`] when the git directory cannot be resolved.
pub async fn is_rebase_in_progress(repo_path: PathBuf) -> Result<bool, GitError> {
    spawn_blocking(move || -> Result<bool, GitError> {
        let git_dir = resolve_git_dir(&repo_path)
            .ok_or_else(|| GitError::OutputParse("Failed to resolve git directory".to_string()))?;
        let rebase_merge = git_dir.join("rebase-merge");
        let rebase_apply = git_dir.join("rebase-apply");

        Ok(rebase_merge.exists() || rebase_apply.exists())
    })
    .await?
}

/// Returns whether unresolved paths still exist in the index.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// `true` when unresolved paths exist, `false` otherwise.
///
/// # Errors
/// Returns a [`GitError`] when conflicted files cannot be queried.
pub async fn has_unmerged_paths(repo_path: PathBuf) -> Result<bool, GitError> {
    let conflicted_files = list_conflicted_files(repo_path).await?;

    Ok(!conflicted_files.is_empty())
}

/// Returns which of the given `paths` still contain git conflict markers
/// (`<<<<<<<`) in their staged content.
///
/// Uses `git grep --cached -l` to search indexed content directly, so it
/// detects files that were staged via `git add` while still containing
/// unresolved conflict markers. The search is scoped to `paths` to avoid
/// false positives from files that legitimately contain `<<<<<<<` (e.g.
/// test fixtures or documentation).
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `paths` - Relative file paths to inspect (typically the files that were
///   involved in the current conflict)
///
/// # Returns
/// The subset of `paths` whose staged content contains lines starting with
/// `<<<<<<<`. Returns an empty list when no matches are found or when
/// `paths` is empty.
///
/// # Errors
/// Returns a [`GitError`] if `git grep` cannot be executed or exits with an
/// unexpected error code. An exit code of `1` (no matches) is treated as
/// success with an empty result.
pub async fn list_staged_conflict_marker_files(
    repo_path: PathBuf,
    paths: Vec<String>,
) -> Result<Vec<String>, GitError> {
    if paths.is_empty() {
        return Ok(vec![]);
    }

    spawn_blocking(move || -> Result<Vec<String>, GitError> {
        let mut grep_arguments = vec!["grep", "--cached", "-l", "^<<<<<<<", "--"];
        let path_arguments: Vec<&str> = paths.iter().map(String::as_str).collect();
        grep_arguments.extend(path_arguments);
        let output = run_git_command_output_sync(&repo_path, &grep_arguments)
            .map_err(GitError::OutputParse)?;

        // git grep exits with 1 when no matches are found.
        let exit_code = output.status.code().unwrap_or(2);
        if !output.status.success() && exit_code != 1 {
            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(GitError::CommandFailed {
                command: "git grep".to_string(),
                stderr: format!("Failed to check for staged conflict markers: {detail}"),
            });
        }

        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect();

        Ok(files)
    })
    .await?
}

/// Returns conflicted file paths for the current index.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// A list of relative file paths with unresolved conflicts.
///
/// # Errors
/// Returns a [`GitError`] if invoking `git diff --name-only --diff-filter=U`
/// fails.
pub async fn list_conflicted_files(repo_path: PathBuf) -> Result<Vec<String>, GitError> {
    spawn_blocking(move || -> Result<Vec<String>, GitError> {
        let output = run_git_command_sync(
            &repo_path,
            &["diff", "--name-only", "--diff-filter=U"],
            "Failed to read conflicted files",
        )
        .map_err(GitError::OutputParse)?;
        let files = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect();

        Ok(files)
    })
    .await?
}

/// Runs a git command and retries when `index.lock` contention occurs.
pub(super) fn run_git_command_with_index_lock_retry(
    repo_path: &Path,
    args: &[&str],
    environment: &[(&str, &str)],
) -> Result<Output, GitError> {
    let command_runner = ProcessGitCommandRunner;
    let sleeper = ThreadSleeper;

    run_git_command_with_index_lock_retry_with_dependencies(
        repo_path,
        args,
        environment,
        &command_runner,
        &sleeper,
    )
}

/// Runs a git command with retries using injected command and sleep
/// dependencies.
fn run_git_command_with_index_lock_retry_with_dependencies(
    repo_path: &Path,
    args: &[&str],
    environment: &[(&str, &str)],
    command_runner: &dyn GitCommandRunner,
    sleeper: &dyn Sleeper,
) -> Result<Output, GitError> {
    for attempt in 0..GIT_INDEX_LOCK_RETRY_ATTEMPTS {
        let output =
            command_runner.run_git_command_output_with_env(repo_path, args, environment)?;
        if output.status.success() {
            return Ok(output);
        }

        let detail = command_output_detail(&output.stdout, &output.stderr);
        let is_last_attempt = attempt + 1 == GIT_INDEX_LOCK_RETRY_ATTEMPTS;
        if !is_git_index_lock_error(&detail) || is_last_attempt {
            return Ok(output);
        }

        sleeper.sleep(GIT_INDEX_LOCK_RETRY_DELAY);
    }

    unreachable!("index lock retry loop should always return an output")
}

/// Returns whether git output detail indicates a rebase conflict state.
///
/// Matches all known git messages that signal a conflict requiring manual
/// resolution, including messages emitted when staging partially-resolved
/// files and attempting `git rebase --continue` prematurely.
pub(super) fn is_rebase_conflict(detail: &str) -> bool {
    detail.contains("CONFLICT")
        || detail.contains("Resolve all conflicts manually")
        || detail.contains("could not apply")
        || detail.contains("mark them as resolved")
        || detail.contains("unresolved conflict")
        || detail.contains("Committing is not possible")
}

/// Returns whether abort output indicates stale or inactive rebase metadata.
fn is_stale_or_inactive_rebase_error(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("already a rebase-merge directory")
        || normalized_detail.contains("already a rebase-apply directory")
        || normalized_detail.contains("middle of another rebase")
        || normalized_detail.contains("no rebase in progress")
        || normalized_detail.contains("rebase-merge")
        || normalized_detail.contains("rebase-apply")
}

/// Removes stale rebase metadata directories/files from the git directory.
///
/// Returns `true` when at least one stale metadata path was removed.
///
/// # Errors
/// Returns a [`GitError`] when the git directory cannot be resolved or
/// metadata cleanup fails.
fn clean_stale_rebase_metadata(repo_path: &Path) -> Result<bool, GitError> {
    let git_dir = resolve_git_dir(repo_path)
        .ok_or_else(|| GitError::OutputParse("Failed to resolve git directory".to_string()))?;
    let rebase_merge = git_dir.join("rebase-merge");
    let rebase_apply = git_dir.join("rebase-apply");
    let removed_rebase_merge = remove_stale_rebase_metadata_path(&rebase_merge)?;
    let removed_rebase_apply = remove_stale_rebase_metadata_path(&rebase_apply)?;

    Ok(removed_rebase_merge || removed_rebase_apply)
}

/// Removes one stale rebase metadata path and returns whether anything changed.
///
/// # Errors
/// Returns a [`GitError`] when a stale metadata path exists but cannot be
/// removed.
fn remove_stale_rebase_metadata_path(path: &Path) -> Result<bool, GitError> {
    if !path.exists() {
        return Ok(false);
    }

    if path.is_dir() {
        fs::remove_dir_all(path)?;

        return Ok(true);
    }

    fs::remove_file(path)?;

    Ok(true)
}

/// Returns whether git output indicates transient index lock contention.
fn is_git_index_lock_error(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("index.lock")
        && (normalized_detail.contains("file exists")
            || normalized_detail.contains("unable to create")
            || normalized_detail.contains("another git process"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::{Command, Output};

    use mockall::predicate::eq;
    use tempfile::tempdir;

    use super::*;
    use crate::MockSleeper;

    #[test]
    fn test_run_git_command_with_index_lock_retry_retries_and_sleeps_before_success() {
        // Arrange
        let mut command_runner = MockGitCommandRunner::new();
        let mut sleeper = MockSleeper::new();
        let repo_path = Path::new(".");
        let args = ["rebase", "main"];
        let environment: [(&str, &str); 0] = [];

        command_runner
            .expect_run_git_command_output_with_env()
            .times(1)
            .returning(|_, _, _| Ok(git_index_lock_output()));
        command_runner
            .expect_run_git_command_output_with_env()
            .times(1)
            .returning(|_, _, _| Ok(success_output()));

        sleeper
            .expect_sleep()
            .with(eq(GIT_INDEX_LOCK_RETRY_DELAY))
            .times(1)
            .return_once(|_| {});

        // Act
        let output = run_git_command_with_index_lock_retry_with_dependencies(
            repo_path,
            &args,
            &environment,
            &command_runner,
            &sleeper,
        )
        .expect("retry helper should return command output");

        // Assert
        assert!(output.status.success());
    }

    #[test]
    fn test_run_git_command_with_index_lock_retry_returns_last_lock_failure() {
        // Arrange
        let mut command_runner = MockGitCommandRunner::new();
        let mut sleeper = MockSleeper::new();
        let repo_path = Path::new(".");
        let args = ["rebase", "main"];
        let environment: [(&str, &str); 0] = [];

        command_runner
            .expect_run_git_command_output_with_env()
            .times(GIT_INDEX_LOCK_RETRY_ATTEMPTS)
            .returning(|_, _, _| Ok(git_index_lock_output()));
        sleeper
            .expect_sleep()
            .with(eq(GIT_INDEX_LOCK_RETRY_DELAY))
            .times(GIT_INDEX_LOCK_RETRY_ATTEMPTS - 1)
            .returning(|_| {});

        // Act
        let output = run_git_command_with_index_lock_retry_with_dependencies(
            repo_path,
            &args,
            &environment,
            &command_runner,
            &sleeper,
        )
        .expect("retry helper should return command output");

        // Assert
        assert!(!output.status.success());
        assert!(command_output_detail(&output.stdout, &output.stderr).contains("index.lock"));
    }

    #[test]
    fn test_run_git_command_with_index_lock_retry_returns_command_error_without_sleeping() {
        // Arrange
        let mut command_runner = MockGitCommandRunner::new();
        let mut sleeper = MockSleeper::new();
        let repo_path = Path::new(".");
        let args = ["rebase", "main"];
        let environment: [(&str, &str); 0] = [];

        command_runner
            .expect_run_git_command_output_with_env()
            .times(1)
            .return_once(|_, _, _| {
                Err(GitError::CommandFailed {
                    command: "git".to_string(),
                    stderr: "git execution failed".to_string(),
                })
            });
        sleeper.expect_sleep().times(0);

        // Act
        let error = run_git_command_with_index_lock_retry_with_dependencies(
            repo_path,
            &args,
            &environment,
            &command_runner,
            &sleeper,
        )
        .expect_err("retry helper should surface command execution errors");

        // Assert
        assert_eq!(error.to_string(), "git: git execution failed");
    }

    #[test]
    fn test_run_git_command_with_index_lock_retry_does_not_sleep_for_non_lock_errors() {
        // Arrange
        let mut command_runner = MockGitCommandRunner::new();
        let mut sleeper = MockSleeper::new();
        let repo_path = Path::new(".");
        let args = ["rebase", "main"];
        let environment: [(&str, &str); 0] = [];

        command_runner
            .expect_run_git_command_output_with_env()
            .times(1)
            .returning(|_, _, _| Ok(non_lock_failure_output()));
        sleeper.expect_sleep().times(0);

        // Act
        let output = run_git_command_with_index_lock_retry_with_dependencies(
            repo_path,
            &args,
            &environment,
            &command_runner,
            &sleeper,
        )
        .expect("retry helper should return command output");

        // Assert
        assert!(!output.status.success());
    }

    #[test]
    fn test_is_rebase_conflict_matches_unmerged_files_message() {
        // Arrange
        let detail = "Committing is not possible because you have unmerged files.";

        // Act
        let is_conflict = is_rebase_conflict(detail);

        // Assert
        assert!(is_conflict);
    }

    #[test]
    fn test_is_stale_or_inactive_rebase_error_matches_no_rebase_message() {
        // Arrange
        let detail = "fatal: No rebase in progress?";

        // Act
        let is_stale_metadata_error = is_stale_or_inactive_rebase_error(detail);

        // Assert
        assert!(is_stale_metadata_error);
    }

    #[test]
    fn test_clean_stale_rebase_metadata_removes_existing_paths() {
        // Arrange
        let temp_dir = tempdir().expect("tempdir should be created");
        let git_dir = temp_dir.path().join(".git");
        let rebase_merge = git_dir.join("rebase-merge");
        let rebase_apply = git_dir.join("rebase-apply");

        fs::create_dir(&git_dir).expect("git dir should be created");
        fs::create_dir(&rebase_merge).expect("rebase-merge dir should be created");
        fs::write(&rebase_apply, "apply state").expect("rebase-apply file should be created");

        // Act
        let cleaned = clean_stale_rebase_metadata(temp_dir.path())
            .expect("stale metadata cleanup should succeed");

        // Assert
        assert!(cleaned);
        assert!(!rebase_merge.exists());
        assert!(!rebase_apply.exists());
    }

    /// Returns a successful git command output.
    fn success_output() -> Output {
        Command::new("git")
            .arg("--version")
            .output()
            .expect("failed to run git --version")
    }

    /// Returns a failing git command output that matches index lock contention.
    fn git_index_lock_output() -> Output {
        let mut output = Command::new("git")
            .arg("definitely-invalid-subcommand")
            .output()
            .expect("failed to run git invalid command");
        output.stdout = vec![];
        output.stderr = b"fatal: Unable to create '.git/index.lock': File exists.".to_vec();

        output
    }

    /// Returns a failing git command output that is unrelated to index locking.
    fn non_lock_failure_output() -> Output {
        let mut output = Command::new("git")
            .arg("definitely-invalid-subcommand")
            .output()
            .expect("failed to run git invalid command");
        output.stdout = vec![];
        output.stderr = b"fatal: not a git repository".to_vec();

        output
    }
}
