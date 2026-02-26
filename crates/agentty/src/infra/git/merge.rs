use std::path::PathBuf;

use tokio::task::spawn_blocking;

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
pub async fn squash_merge_diff(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
) -> Result<String, String> {
    spawn_blocking(move || {
        let revision_range = format!("{target_branch}..{source_branch}");
        run_git_command_sync(
            &repo_path,
            &["diff", revision_range.as_str()],
            "Failed to read squash merge diff",
        )
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Performs a squash merge from a source branch to a target branch.
///
/// This function:
/// 1. Verifies the repository is already on the target branch
/// 2. Performs `git merge --squash` from the source branch
/// 3. Commits the squashed changes (skipping pre-commit hooks)
///
/// The caller is responsible for ensuring `repo_path` is already checked out
/// on `target_branch`. Switching branches here would disrupt the user's
/// working directory.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root, already on `target_branch`
/// * `source_branch` - Name of the branch to merge from (e.g.,
///   `agentty/abc123`)
/// * `target_branch` - Name of the branch to merge into (e.g., `main`)
/// * `commit_message` - Message for the squash commit
///
/// # Returns
/// A [`SquashMergeOutcome`] describing whether a squash commit was created.
///
/// # Errors
/// Returns an error if the repository is on the wrong branch, the merge
/// fails, or the commit fails.
pub async fn squash_merge(
    repo_path: PathBuf,
    source_branch: String,
    target_branch: String,
    commit_message: String,
) -> Result<SquashMergeOutcome, String> {
    spawn_blocking(move || {
        // Verify that `repo_path` is already on the target branch.
        let current_branch = detect_git_info_sync(&repo_path)
            .ok_or_else(|| format!("Failed to detect current branch in {}", repo_path.display()))?;
        if current_branch != target_branch {
            return Err(format!(
                "Cannot merge: repository is on '{current_branch}' but expected
                 '{target_branch}'. Switch to '{target_branch}' first."
            ));
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

            return Err(format!(
                "Failed to inspect staged squash merge diff: {detail}"
            ));
        }

        // Skip hooks here because the session worktree already ran them.
        run_git_command_sync(
            &repo_path,
            &["commit", "--no-verify", "-m", commit_message.as_str()],
            "Failed to commit squash merge",
        )?;

        Ok(SquashMergeOutcome::Committed)
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}
