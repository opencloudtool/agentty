use std::path::{Path, PathBuf};

use tempfile::tempdir;
use tokio::task::spawn_blocking;

use super::error::GitError;
use super::repo::{command_output_detail, run_git_command_output_sync, run_git_command_sync};
use super::worktree::detect_git_info_sync;

/// Outcome of attempting a squash merge operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SquashMergeOutcome {
    /// Squash merge staged changes and created a commit.
    Committed,
    /// Squash merge staged nothing because changes already exist in target.
    AlreadyPresentInTarget,
}

/// Outcome classification for one attempted `merge-tree --write-tree` probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MergeTreeAttempt {
    Clean,
    Conflict,
    Unsupported,
    Failed,
}

/// Captured output from the compatibility merge command.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CompatibilityMergeOutput {
    stderr: Vec<u8>,
    stdout: Vec<u8>,
    success: bool,
}

/// Executes the Git commands used by the compatibility merge probe.
#[cfg_attr(test, mockall::automock)]
trait CompatibilityMergeRunner: Send + Sync {
    /// Runs a Git command that must succeed and returns its standard output.
    fn run_git_command(
        &self,
        repo_path: &Path,
        args: &[String],
        error_context: &str,
    ) -> Result<String, GitError>;

    /// Runs the merge command and returns its status and captured output.
    fn run_git_command_output(
        &self,
        repo_path: &Path,
        args: &[String],
    ) -> Result<CompatibilityMergeOutput, GitError>;
}

/// Compatibility merge runner backed by local Git subprocesses.
struct ProcessCompatibilityMergeRunner;

impl CompatibilityMergeRunner for ProcessCompatibilityMergeRunner {
    fn run_git_command(
        &self,
        repo_path: &Path,
        args: &[String],
        error_context: &str,
    ) -> Result<String, GitError> {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();

        run_git_command_sync(repo_path, &args, error_context)
    }

    fn run_git_command_output(
        &self,
        repo_path: &Path,
        args: &[String],
    ) -> Result<CompatibilityMergeOutput, GitError> {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = run_git_command_output_sync(repo_path, &args)?;
        let success = output.status.success();

        Ok(CompatibilityMergeOutput {
            stderr: output.stderr,
            stdout: output.stdout,
            success,
        })
    }
}

/// Returns whether merging `source_branch` into `target_branch` would produce
/// conflicts without reading or changing the repository index or worktree.
///
/// # Errors
/// Returns an error when either branch cannot be resolved or neither the
/// native `git merge-tree` probe nor its compatibility fallback can compute
/// the merge.
pub(crate) async fn has_merge_conflicts(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
) -> Result<bool, GitError> {
    spawn_blocking(move || {
        let output = run_git_command_output_sync(
            &repo_path,
            &[
                "merge-tree",
                "--write-tree",
                target_branch.as_str(),
                source_branch.as_str(),
            ],
        )?;

        let attempt = classify_merge_tree_attempt(
            output.status.code(),
            output.stdout.as_slice(),
            output.stderr.as_slice(),
        );

        resolve_merge_tree_attempt(
            &repo_path,
            source_branch.as_str(),
            target_branch.as_str(),
            attempt,
            output.stdout.as_slice(),
            output.stderr.as_slice(),
        )
    })
    .await?
}

/// Classifies native merge-tree output, including the pre-2.38 unsupported
/// synopsis that does not advertise `--write-tree`.
fn classify_merge_tree_attempt(
    exit_code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> MergeTreeAttempt {
    match exit_code {
        Some(0) => MergeTreeAttempt::Clean,
        Some(1) if stderr.is_empty() => MergeTreeAttempt::Conflict,
        Some(129)
            if !String::from_utf8_lossy(stdout).contains("--write-tree")
                && !String::from_utf8_lossy(stderr).contains("--write-tree") =>
        {
            MergeTreeAttempt::Unsupported
        }
        _ => MergeTreeAttempt::Failed,
    }
}

/// Resolves a classified native probe, delegating unsupported Git versions to
/// an isolated compatibility merge.
fn resolve_merge_tree_attempt(
    repo_path: &std::path::Path,
    source_branch: &str,
    target_branch: &str,
    attempt: MergeTreeAttempt,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<bool, GitError> {
    match attempt {
        MergeTreeAttempt::Clean => Ok(false),
        MergeTreeAttempt::Conflict => Ok(true),
        MergeTreeAttempt::Unsupported => {
            has_merge_conflicts_via_temporary_clone(repo_path, source_branch, target_branch)
        }
        MergeTreeAttempt::Failed => {
            let detail = command_output_detail(stdout, stderr);

            Err(GitError::CommandFailed {
                command: format!("git merge-tree --write-tree {target_branch} {source_branch}"),
                stderr: format!("Failed to inspect merge conflicts: {detail}"),
            })
        }
    }
}

/// Computes the merge in a disposable local clone for Git versions whose
/// `merge-tree` lacks `--write-tree`.
fn has_merge_conflicts_via_temporary_clone(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
) -> Result<bool, GitError> {
    let temporary_directory = tempdir()?;
    let command_runner = ProcessCompatibilityMergeRunner;

    has_merge_conflicts_via_temporary_clone_with_runner(
        repo_path,
        source_branch,
        target_branch,
        &temporary_directory,
        &command_runner,
    )
}

/// Computes a compatibility merge through an injectable command boundary.
fn has_merge_conflicts_via_temporary_clone_with_runner(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
    temporary_directory: &tempfile::TempDir,
    command_runner: &dyn CompatibilityMergeRunner,
) -> Result<bool, GitError> {
    let source_revision = format!("{source_branch}^{{commit}}");
    let source_commit = command_runner.run_git_command(
        repo_path,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            source_revision,
        ],
        "Failed to resolve merge source",
    )?;
    let target_revision = format!("{target_branch}^{{commit}}");
    let target_commit = command_runner.run_git_command(
        repo_path,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            target_revision,
        ],
        "Failed to resolve merge target",
    )?;
    let source_commit = source_commit.trim();
    let target_commit = target_commit.trim();

    let clone_path = temporary_directory.path().join("repository");
    let clone_path_text = clone_path.to_string_lossy();
    command_runner.run_git_command(
        repo_path,
        &[
            "clone".to_string(),
            "--shared".to_string(),
            "--no-checkout".to_string(),
            "--quiet".to_string(),
            ".".to_string(),
            clone_path_text.into_owned(),
        ],
        "Failed to create compatibility merge clone",
    )?;
    command_runner.run_git_command(
        &clone_path,
        &[
            "checkout".to_string(),
            "--detach".to_string(),
            "--quiet".to_string(),
            target_commit.to_string(),
        ],
        "Failed to check out compatibility merge target",
    )?;

    let disabled_hooks_path = temporary_directory.path().join("disabled-hooks");
    let disabled_hooks_path = disabled_hooks_path.to_string_lossy();
    let hooks_config = format!("core.hooksPath={disabled_hooks_path}");
    let merge_output = command_runner.run_git_command_output(
        &clone_path,
        &[
            "-c".to_string(),
            hooks_config,
            "-c".to_string(),
            "user.name=Agentty".to_string(),
            "-c".to_string(),
            "user.email=agentty@localhost".to_string(),
            "-c".to_string(),
            "user.useConfigOnly=true".to_string(),
            "merge".to_string(),
            "--no-commit".to_string(),
            "--no-ff".to_string(),
            source_commit.to_string(),
        ],
    )?;
    if merge_output.success {
        return Ok(false);
    }

    let unmerged_files = command_runner.run_git_command(
        &clone_path,
        &["ls-files".to_string(), "--unmerged".to_string()],
        "Failed to inspect compatibility merge conflicts",
    )?;
    if !unmerged_files.trim().is_empty() {
        return Ok(true);
    }

    let detail = command_output_detail(&merge_output.stdout, &merge_output.stderr);

    Err(GitError::CommandFailed {
        command: format!("git merge --no-commit --no-ff {source_commit}"),
        stderr: format!("Failed to inspect merge conflicts in compatibility clone: {detail}"),
    })
}

/// Returns the full patch diff that will be squashed when merging a source
/// branch into a target branch.
///
/// Uses `git diff <target>..<source>`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
/// * `source_branch` - Name of the branch being merged
/// * `target_branch` - Name of the branch receiving the squash merge
///
/// # Returns
/// The full patch diff for the squash merge range.
///
/// # Errors
/// Returns an error if invoking `git` fails or `git diff` exits with a
/// non-zero status.
pub(crate) async fn squash_merge_diff(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
) -> Result<String, GitError> {
    spawn_blocking(move || {
        let revision_range = format!("{target_branch}..{source_branch}");

        run_git_command_sync(
            &repo_path,
            &["diff", revision_range.as_str()],
            "Failed to read squash merge diff",
        )
    })
    .await?
}

/// Performs a squash merge from a source branch to a target branch.
///
/// This function:
/// 1. Verifies the repository is already on the target branch
/// 2. Performs `git merge --squash` from the source branch
/// 3. Commits the squashed changes, running configured commit hooks
///
/// The caller is responsible for ensuring `repo_path` is already checked out
/// on `target_branch`. Switching branches here would disrupt the user's
/// working directory.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root, already on `target_branch`
/// * `source_branch` - Name of the branch to merge from (e.g., `wt/abc123`)
/// * `target_branch` - Name of the branch to merge into (e.g., `main`)
/// * `commit_message` - Message for the squash commit
///
/// # Returns
/// A [`SquashMergeOutcome`] describing whether a squash commit was created.
///
/// # Errors
/// Returns an error if the repository is on the wrong branch, the merge
/// fails, or the commit or a configured commit hook fails.
pub(crate) async fn squash_merge(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
    commit_message: String,
) -> Result<SquashMergeOutcome, GitError> {
    spawn_blocking(move || {
        // Verify that `repo_path` is already on the target branch.
        let current_branch = detect_git_info_sync(&repo_path).ok_or_else(|| {
            GitError::OutputParse(format!(
                "Failed to detect current branch in {}",
                repo_path.display()
            ))
        })?;

        if current_branch != target_branch {
            return Err(GitError::CommandFailed {
                command: "git merge --squash".to_string(),
                stderr: format!(
                    "Cannot merge: repository is on '{current_branch}' but expected \
                     '{target_branch}'. Switch to '{target_branch}' first."
                ),
            });
        }

        run_git_command_sync(
            &repo_path,
            &["merge", "--squash", source_branch.as_str()],
            &format!("Failed to squash merge {source_branch}"),
        )?;

        // `git diff --cached --quiet` exits 0 when index matches `HEAD`.
        let cached_diff =
            run_git_command_output_sync(&repo_path, &["diff", "--cached", "--quiet"])?;

        if cached_diff.status.success() {
            return Ok(SquashMergeOutcome::AlreadyPresentInTarget);
        }

        if cached_diff.status.code() != Some(1) {
            let detail = command_output_detail(&cached_diff.stdout, &cached_diff.stderr);

            return Err(GitError::CommandFailed {
                command: "git diff --cached".to_string(),
                stderr: detail,
            });
        }

        run_git_command_sync(
            &repo_path,
            &["commit", "-m", commit_message.as_str()],
            "Failed to commit squash merge",
        )?;

        Ok(SquashMergeOutcome::Committed)
    })
    .await?
}

#[cfg(test)]
#[path = "merge_test.rs"]
mod tests;
