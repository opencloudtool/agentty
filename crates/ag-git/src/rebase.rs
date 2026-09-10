use std::fs;
use std::io::ErrorKind;
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

/// Allow five seconds of waiting for an in-flight index writer to finish.
pub(super) const GIT_INDEX_LOCK_RETRY_ATTEMPTS: usize = 21;
pub(super) const GIT_INDEX_LOCK_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Executes git commands for rebase operations.
#[cfg_attr(test, mockall::automock)]
trait GitCommandRunner: Send + Sync {
    /// Runs a git command in `repo_path` with environment overrides.
    fn run_git_command_output_with_env(
        &self,
        repo_path: &Path,
        args: &[String],
        environment: &[(String, String)],
    ) -> Result<Output, GitError>;
}

/// Removes stale rebase metadata through an injectable filesystem boundary.
#[cfg_attr(test, mockall::automock)]
trait RebaseMetadataCleaner: Send + Sync {
    /// Removes exact rebase metadata entries under the resolved git directory.
    fn clean_stale_metadata(&self, repo_path: &Path) -> Result<bool, GitError>;
}

/// Rebase metadata cleaner backed by the local filesystem.
struct FilesystemRebaseMetadataCleaner;

impl RebaseMetadataCleaner for FilesystemRebaseMetadataCleaner {
    fn clean_stale_metadata(&self, repo_path: &Path) -> Result<bool, GitError> {
        clean_stale_rebase_metadata(repo_path)
    }
}

/// Git command runner backed by process execution.
struct ProcessGitCommandRunner;

impl GitCommandRunner for ProcessGitCommandRunner {
    fn run_git_command_output_with_env(
        &self,
        repo_path: &Path,
        args: &[String],
        environment: &[(String, String)],
    ) -> Result<Output, GitError> {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let environment = environment
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();

        run_git_command_output_with_env_sync(repo_path, &args, &environment)
    }
}

/// Result of attempting a rebase step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebaseStepResult {
    /// Rebase step completed successfully.
    Completed,
    /// Rebase step stopped because of merge conflicts.
    Conflict {
        /// Git diagnostic describing the conflict state.
        detail: String,
    },
}

/// Git operation metadata that marks a worktree as unsafe for branch pushes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InProgressGitOperation {
    /// A cherry-pick is in progress.
    CherryPick,
    /// A merge is in progress.
    Merge,
    /// A rebase is in progress.
    Rebase,
    /// A revert is in progress.
    Revert,
}

impl InProgressGitOperation {
    /// Returns an indefinite article plus the operation name for user-facing
    /// status text.
    pub fn article_name(self) -> &'static str {
        match self {
            Self::CherryPick => "a cherry-pick",
            Self::Merge => "a merge",
            Self::Rebase => "a rebase",
            Self::Revert => "a revert",
        }
    }

    /// Returns the operation name for user-facing status text.
    pub fn name(self) -> &'static str {
        match self {
            Self::CherryPick => "cherry-pick",
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::Revert => "revert",
        }
    }
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
pub(crate) async fn rebase(repo_path: PathBuf, target_branch: String) -> Result<(), GitError> {
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
pub(crate) async fn rebase_start(
    repo_path: PathBuf,
    target_branch: String,
) -> Result<RebaseStepResult, GitError> {
    spawn_blocking(move || {
        let rebase_args = ["rebase", target_branch.as_str()];
        run_rebase_step(&repo_path, &rebase_args, "git rebase", |detail| {
            format!("Failed to rebase onto {target_branch}: {detail}.")
        })
    })
    .await?
}

/// Starts a rebase that moves commits after `old_base` onto `new_base`.
///
/// This is used for stacked sessions to drop commits that came from a parent
/// branch after that parent has moved or squash-merged into its own base.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree.
/// * `new_base` - Ref that should become the new base of replayed commits.
/// * `old_base` - Commit/ref whose ancestors should be left behind.
///
/// # Returns
/// A [`RebaseStepResult`] describing whether the rebase completed or
/// encountered conflicts.
///
/// # Errors
/// Returns a [`GitError`] for non-conflict git failures.
pub(crate) async fn rebase_onto_start(
    repo_path: PathBuf,
    new_base: String,
    old_base: String,
) -> Result<RebaseStepResult, GitError> {
    spawn_blocking(move || {
        let rebase_args = ["rebase", "--onto", new_base.as_str(), old_base.as_str()];
        run_rebase_step(&repo_path, &rebase_args, "git rebase --onto", |detail| {
            format!("Failed to rebase onto {new_base} after {old_base}: {detail}.")
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
pub(crate) async fn rebase_continue(repo_path: PathBuf) -> Result<RebaseStepResult, GitError> {
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
/// When Git reports a known stale or inactive rebase state, this removes only
/// `rebase-merge` and `rebase-apply` under the resolved git directory. Other
/// failures are returned unchanged with their command output.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] when `git rebase --abort` cannot be executed.
pub(crate) async fn abort_rebase(repo_path: PathBuf) -> Result<(), GitError> {
    spawn_blocking(move || {
        let command_runner = ProcessGitCommandRunner;
        let metadata_cleaner = FilesystemRebaseMetadataCleaner;
        let sleeper = ThreadSleeper;

        abort_rebase_with_dependencies(&repo_path, &command_runner, &sleeper, &metadata_cleaner)
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
/// Returns [`GitError::RepositoryUnavailable`] when the repository folder is
/// missing, or another [`GitError`] when its git directory cannot be resolved.
pub(crate) async fn is_rebase_in_progress(repo_path: PathBuf) -> Result<bool, GitError> {
    spawn_blocking(move || -> Result<bool, GitError> {
        match fs::metadata(&repo_path) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(GitError::RepositoryUnavailable {
                    detail: format!("Repository folder is missing: {}", repo_path.display()),
                });
            }
            Err(error) => return Err(error.into()),
        }
        let git_dir = resolve_git_dir(&repo_path).ok_or_else(|| {
            GitError::OutputParse(format!(
                "Failed to resolve git directory for `{}`",
                repo_path.display()
            ))
        })?;

        Ok(has_rebase_metadata(&git_dir))
    })
    .await?
}

/// Returns the first detected in-progress git operation in `repo_path`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// An operation when rebase, merge, cherry-pick, or revert metadata exists.
///
/// # Errors
/// Returns a [`GitError`] when the git directory cannot be resolved.
pub(crate) async fn in_progress_operation(
    repo_path: PathBuf,
) -> Result<Option<InProgressGitOperation>, GitError> {
    spawn_blocking(move || in_progress_operation_sync(&repo_path)).await?
}

fn in_progress_operation_sync(
    repo_path: &Path,
) -> Result<Option<InProgressGitOperation>, GitError> {
    let git_dir = resolve_git_dir(repo_path)
        .ok_or_else(|| GitError::OutputParse("Failed to resolve git directory".to_string()))?;
    if has_rebase_metadata(&git_dir) {
        return Ok(Some(InProgressGitOperation::Rebase));
    }
    if git_dir.join("MERGE_HEAD").exists() {
        return Ok(Some(InProgressGitOperation::Merge));
    }
    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return Ok(Some(InProgressGitOperation::CherryPick));
    }
    if git_dir.join("REVERT_HEAD").exists() {
        return Ok(Some(InProgressGitOperation::Revert));
    }

    Ok(None)
}

fn has_rebase_metadata(git_dir: &Path) -> bool {
    let rebase_merge = git_dir.join("rebase-merge");
    let rebase_apply = git_dir.join("rebase-apply");

    rebase_merge.exists() || rebase_apply.exists()
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
pub(crate) async fn has_unmerged_paths(repo_path: PathBuf) -> Result<bool, GitError> {
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
pub(crate) async fn list_staged_conflict_marker_files(
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
        let output = run_git_command_output_sync(&repo_path, &grep_arguments)?;

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
pub(crate) async fn list_conflicted_files(repo_path: PathBuf) -> Result<Vec<String>, GitError> {
    spawn_blocking(move || -> Result<Vec<String>, GitError> {
        let output = run_git_command_sync(
            &repo_path,
            &["diff", "--name-only", "--diff-filter=U"],
            "Failed to read conflicted files",
        )?;
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

/// Runs one rebase command and maps git output to a step result.
fn run_rebase_step(
    repo_path: &Path,
    args: &[&str],
    command: &str,
    failure_message: impl FnOnce(&str) -> String,
) -> Result<RebaseStepResult, GitError> {
    let output = run_git_command_with_index_lock_retry(repo_path, args, &[])?;

    if output.status.success() {
        return Ok(RebaseStepResult::Completed);
    }

    let detail = command_output_detail(&output.stdout, &output.stderr);
    if is_rebase_conflict(&detail) {
        return Ok(RebaseStepResult::Conflict { detail });
    }

    Err(GitError::CommandFailed {
        command: command.to_string(),
        stderr: failure_message(&detail),
    })
}

/// Aborts one rebase through injected process and retry boundaries.
fn abort_rebase_with_dependencies(
    repo_path: &Path,
    command_runner: &dyn GitCommandRunner,
    sleeper: &dyn Sleeper,
    metadata_cleaner: &dyn RebaseMetadataCleaner,
) -> Result<(), GitError> {
    let output = run_git_command_with_index_lock_retry_with_dependencies(
        repo_path,
        &["rebase", "--abort"],
        &[],
        command_runner,
        sleeper,
    )?;
    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);
        if is_stale_or_inactive_rebase_error(&detail) {
            match metadata_cleaner.clean_stale_metadata(repo_path) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(cleanup_error) => {
                    return Err(GitError::CommandFailed {
                        command: "git rebase --abort".to_string(),
                        stderr: format!(
                            "Failed to abort rebase: {detail}. Stale rebase metadata cleanup \
                             failed: {cleanup_error}."
                        ),
                    });
                }
            }
        }

        return Err(GitError::CommandFailed {
            command: "git rebase --abort".to_string(),
            stderr: format!("Failed to abort rebase: {detail}."),
        });
    }

    Ok(())
}

/// Returns whether abort output identifies a known stale or inactive rebase.
fn is_stale_or_inactive_rebase_error(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("no rebase in progress")
        || normalized_detail.contains("already a rebase-merge directory")
        || normalized_detail.contains("already a rebase-apply directory")
        || normalized_detail.contains("middle of another rebase")
}

/// Removes exact stale rebase metadata entries from the resolved git directory.
fn clean_stale_rebase_metadata(repo_path: &Path) -> Result<bool, GitError> {
    let git_dir = resolve_git_dir(repo_path)
        .ok_or_else(|| GitError::OutputParse("Failed to resolve git directory".to_string()))?;
    let removed_rebase_merge = remove_stale_rebase_metadata_path(&git_dir.join("rebase-merge"))?;
    let removed_rebase_apply = remove_stale_rebase_metadata_path(&git_dir.join("rebase-apply"))?;

    Ok(removed_rebase_merge || removed_rebase_apply)
}

/// Removes one exact metadata path without following directory symlinks.
fn remove_stale_rebase_metadata_path(path: &Path) -> Result<bool, GitError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };

    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }

    Ok(true)
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
    let args = args
        .iter()
        .map(|arg| String::from(*arg))
        .collect::<Vec<_>>();
    let environment = environment
        .iter()
        .map(|(key, value)| (String::from(*key), String::from(*value)))
        .collect::<Vec<_>>();

    for attempt in 0..GIT_INDEX_LOCK_RETRY_ATTEMPTS {
        let output =
            command_runner.run_git_command_output_with_env(repo_path, &args, &environment)?;
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

/// Returns whether git output indicates transient index lock contention.
pub(super) fn is_git_index_lock_error(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("index.lock")
        && (normalized_detail.contains("file exists")
            || normalized_detail.contains("another git process"))
}

#[cfg(test)]
#[path = "rebase_test.rs"]
mod tests;
