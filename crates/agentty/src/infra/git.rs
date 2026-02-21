use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tokio::task::spawn_blocking;

const COMMIT_ALL_HOOK_RETRY_ATTEMPTS: usize = 5;
const GIT_INDEX_LOCK_RETRY_ATTEMPTS: usize = 5;
const GIT_INDEX_LOCK_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Result of attempting a rebase step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebaseStepResult {
    /// Rebase step completed successfully.
    Completed,
    /// Rebase step stopped because of merge conflicts.
    Conflict { detail: String },
}

/// Outcome of attempting a squash merge operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SquashMergeOutcome {
    /// Squash merge staged changes and created a commit.
    Committed,
    /// Squash merge staged nothing because changes already exist in target.
    AlreadyPresentInTarget,
}

/// Detects git repository information for the given directory.
/// Returns the current branch name if in a git repository, None otherwise.
pub async fn detect_git_info(dir: PathBuf) -> Option<String> {
    spawn_blocking(move || detect_git_info_sync(&dir))
        .await
        .ok()
        .flatten()
}

/// Walks up the directory tree to find a .git directory.
/// Returns the directory containing .git (the repository root) if found, None
/// otherwise.
pub async fn find_git_repo_root(dir: PathBuf) -> Option<PathBuf> {
    spawn_blocking(move || find_git_repo_root_sync(&dir))
        .await
        .ok()
        .flatten()
}

/// Creates a git worktree at the specified path with a new branch.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
/// * `worktree_path` - Path where the worktree should be created
/// * `branch_name` - Name of the new branch to create
/// * `base_branch` - Name of the branch to base the new branch on
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if invoking `git` fails or the worktree command exits with
/// a non-zero status.
pub async fn create_worktree(
    repo_path: PathBuf,
    worktree_path: PathBuf,
    branch_name: String,
    base_branch: String,
) -> Result<(), String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["worktree", "add", "-b", &branch_name])
            .arg(&worktree_path)
            .arg(&base_branch)
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Git worktree command failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Removes a git worktree at the specified path.
///
/// Uses --force to remove even with uncommitted changes.
/// Finds the main repository by reading the worktree's .git file.
///
/// # Arguments
/// * `worktree_path` - Path to the worktree to remove
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if invoking `git` fails or the worktree remove command
/// exits with a non-zero status.
pub async fn remove_worktree(worktree_path: PathBuf) -> Result<(), String> {
    spawn_blocking(move || {
        // Read the .git file in the worktree to find the main repo
        let git_file = worktree_path.join(".git");
        let repo_root = if git_file.is_file() {
            let git_dir = resolve_git_dir(&worktree_path)
                .ok_or_else(|| "Invalid .git file format in worktree".to_string())?;

            // Extract main repo path: /path/to/main/.git/worktrees/name -> /path/to/main
            git_dir
                .parent() // Remove worktree name
                .and_then(|path| path.parent()) // Remove "worktrees"
                .and_then(|path| path.parent()) // Remove ".git"
                .ok_or_else(|| "Invalid gitdir path in .git file".to_string())?
                .to_path_buf()
        } else {
            // Not a worktree or doesn't exist - try parent directory
            worktree_path
                .parent()
                .ok_or_else(|| "Worktree path has no parent".to_string())?
                .to_path_buf()
        };

        let output = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&worktree_path)
            .current_dir(repo_root)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Git worktree command failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
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
        let output = Command::new("git")
            .arg("diff")
            .arg(revision_range)
            .current_dir(repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!(
                "Failed to read squash merge diff: {}",
                stderr.trim()
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
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
        // Verify that repo_path is already on the target branch. Switching
        // branches here would disrupt the user's working directory.
        let current_branch = detect_git_info_sync(&repo_path)
            .ok_or_else(|| format!("Failed to detect current branch in {}", repo_path.display()))?;
        if current_branch != target_branch {
            return Err(format!(
                "Cannot merge: repository is on '{current_branch}' but expected
                 '{target_branch}'. Switch to '{target_branch}' first."
            ));
        }

        // Perform squash merge
        let output = Command::new("git")
            .args(["merge", "--squash", &source_branch])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Failed to squash merge {source_branch}: {}",
                stderr.trim()
            ));
        }

        // Check whether the squash merge staged any changes before committing.
        // `git diff --cached --quiet` exits 0 when the index matches HEAD (nothing
        // staged) and 1 when there are staged changes.
        let cached_diff = Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if cached_diff.status.success() {
            return Ok(SquashMergeOutcome::AlreadyPresentInTarget);
        }

        if cached_diff.status.code() != Some(1) {
            let detail = command_output_detail(&cached_diff.stdout, &cached_diff.stderr);

            return Err(format!(
                "Failed to inspect staged squash merge diff: {detail}"
            ));
        }

        // Commit the squashed changes. Skip pre-commit hooks (`--no-verify`)
        // because the session code was already validated by those same hooks
        // during auto-commit in the session worktree. Re-running them here is
        // redundant and causes failures when hooks modify files in the main repo.
        let output = Command::new("git")
            .args(["commit", "--no-verify", "-m", &commit_message])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Failed to commit squash merge: {}", stderr.trim()));
        }

        Ok(SquashMergeOutcome::Committed)
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
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
/// Ok(()) on success, Err(msg) with detailed error message on failure.
///
/// # Errors
/// Returns an error if rebase fails, or aborting a conflicted rebase also
/// fails.
pub async fn rebase(repo_path: PathBuf, target_branch: String) -> Result<(), String> {
    match rebase_start(repo_path.clone(), target_branch.clone()).await? {
        RebaseStepResult::Completed => Ok(()),
        RebaseStepResult::Conflict { detail } => {
            let abort_suffix = match abort_rebase(repo_path).await {
                Ok(()) => String::new(),
                Err(error) => format!(" {error}"),
            };

            Err(format!(
                "Failed to rebase onto {target_branch}: {detail}.{abort_suffix}"
            ))
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
/// Returns an error for non-conflict git failures.
pub async fn rebase_start(
    repo_path: PathBuf,
    target_branch: String,
) -> Result<RebaseStepResult, String> {
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

        Err(format!("Failed to rebase onto {target_branch}: {detail}."))
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
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
/// Returns an error for non-conflict git failures.
pub async fn rebase_continue(repo_path: PathBuf) -> Result<RebaseStepResult, String> {
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

        Err(format!("Failed to continue rebase: {detail}."))
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Aborts an in-progress rebase.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Ok(()) on success, Err(msg) on failure.
///
/// # Errors
/// Returns an error when `git rebase --abort` cannot be executed.
pub async fn abort_rebase(repo_path: PathBuf) -> Result<(), String> {
    spawn_blocking(move || {
        let output =
            run_git_command_with_index_lock_retry(&repo_path, &["rebase", "--abort"], &[])?;

        if !output.status.success() {
            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(format!("Failed to abort rebase: {detail}."));
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
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
/// Returns an error when the git directory cannot be resolved.
pub async fn is_rebase_in_progress(repo_path: PathBuf) -> Result<bool, String> {
    spawn_blocking(move || {
        let git_dir = resolve_git_dir(&repo_path)
            .ok_or_else(|| "Failed to resolve git directory".to_string())?;
        let rebase_merge = git_dir.join("rebase-merge");
        let rebase_apply = git_dir.join("rebase-apply");

        Ok(rebase_merge.exists() || rebase_apply.exists())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
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
/// Returns an error when conflicted files cannot be queried.
pub async fn has_unmerged_paths(repo_path: PathBuf) -> Result<bool, String> {
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
/// Returns an error if `git grep` cannot be executed or exits with an
/// unexpected error code. An exit code of `1` (no matches) is treated as
/// success with an empty result.
pub async fn list_staged_conflict_marker_files(
    repo_path: PathBuf,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(vec![]);
    }

    spawn_blocking(move || {
        let mut command = Command::new("git");
        command
            .args(["grep", "--cached", "-l", "^<<<<<<<", "--"])
            .args(&paths)
            .current_dir(&repo_path);
        let output = command
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        // git grep exits with 1 when no matches are found — that is not an
        // error. Any exit code above 1 signals an actual git error.
        let exit_code = output.status.code().unwrap_or(2);
        if !output.status.success() && exit_code != 1 {
            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(format!(
                "Failed to check for staged conflict markers: {detail}"
            ));
        }

        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect();

        Ok(files)
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
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
/// Returns an error if invoking `git diff --name-only --diff-filter=U` fails.
pub async fn list_conflicted_files(repo_path: PathBuf) -> Result<Vec<String>, String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(&repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(format!("Failed to read conflicted files: {detail}."));
        }

        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect();

        Ok(files)
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Stages all changes and commits them with the given message.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `commit_message` - Message for the commit
/// * `no_verify` - When `true`, skips pre-commit and commit-msg hooks
///   (`--no-verify`)
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if staging or committing changes fails.
pub async fn commit_all(
    repo_path: PathBuf,
    commit_message: String,
    no_verify: bool,
) -> Result<(), String> {
    spawn_blocking(move || {
        // Stage all changes
        stage_all_sync(&repo_path)?;

        for _ in 0..COMMIT_ALL_HOOK_RETRY_ATTEMPTS {
            let output = run_commit_command(&repo_path, &commit_message, no_verify)?;

            if output.status.success() {
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Check if there's nothing to commit
            if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
                return Err("Nothing to commit: no changes detected".to_string());
            }

            if is_hook_modified_error(&stdout, &stderr) {
                stage_all_sync(&repo_path)?;

                continue;
            }

            return Err(format!("Failed to commit: {}", stderr.trim()));
        }

        Err(format!(
            "Failed to commit: commit hooks kept modifying files after
             {COMMIT_ALL_HOOK_RETRY_ATTEMPTS} attempts"
        ))
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Stages all changes in the repository or worktree.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Ok(()) on success, Err(msg) on failure.
///
/// # Errors
/// Returns an error if `git add -A` fails.
pub async fn stage_all(repo_path: PathBuf) -> Result<(), String> {
    spawn_blocking(move || stage_all_sync(&repo_path))
        .await
        .map_err(|e| format!("Join error: {e}"))?
}

/// Returns the short hash of the current `HEAD` commit.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// The short commit hash as a string.
///
/// # Errors
/// Returns an error if resolving `HEAD` fails.
pub async fn head_short_hash(repo_path: PathBuf) -> Result<String, String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Failed to resolve HEAD hash: {}", stderr.trim()));
        }

        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hash.is_empty() {
            return Err("Failed to resolve HEAD hash: empty output".to_string());
        }

        Ok(hash)
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Deletes a git branch.
///
/// Uses -D to force deletion even if not merged.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
/// * `branch_name` - Name of the branch to delete
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if the branch delete command fails.
pub async fn delete_branch(repo_path: PathBuf, branch_name: String) -> Result<(), String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["branch", "-D", &branch_name])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Git branch deletion failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Returns the output of `git diff` for the given repository path, showing
/// all changes (committed and uncommitted) relative to the base branch.
///
/// Uses `git add --intent-to-add` to mark untracked files in the index, then
/// finds the merge-base between `HEAD` and `base_branch` to diff against the
/// fork point. This ensures only the session's changes are shown, excluding
/// any new commits pushed to the base branch after the session was created.
/// Finally resets the index to restore the original state.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `base_branch` - Branch to diff against (e.g., `main`)
///
/// # Returns
/// The diff output as a string, or an error message on failure
///
/// # Errors
/// Returns an error if preparing the index, generating the diff, or restoring
/// index state fails.
pub async fn diff(repo_path: PathBuf, base_branch: String) -> Result<String, String> {
    spawn_blocking(move || {
        let intent_to_add = Command::new("git")
            .args(["add", "-A", "--intent-to-add"])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !intent_to_add.status.success() {
            let stderr = String::from_utf8_lossy(&intent_to_add.stderr);

            return Err(format!("Git add --intent-to-add failed: {}", stderr.trim()));
        }

        let merge_base_output = Command::new("git")
            .args(["merge-base", "HEAD", &base_branch])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        let diff_target = if merge_base_output.status.success() {
            String::from_utf8_lossy(&merge_base_output.stdout)
                .trim()
                .to_string()
        } else {
            base_branch
        };

        let diff_output = Command::new("git")
            .args(["diff", &diff_target])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        let reset = Command::new("git")
            .arg("reset")
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !reset.status.success() {
            let stderr = String::from_utf8_lossy(&reset.stderr);

            return Err(format!("Git reset failed: {}", stderr.trim()));
        }

        Ok(String::from_utf8_lossy(&diff_output.stdout).into_owned())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Fetches from the configured remote.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if `git fetch` cannot be executed successfully.
pub async fn fetch_remote(repo_path: PathBuf) -> Result<(), String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .arg("fetch")
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Git fetch failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Returns the number of commits ahead and behind the upstream branch.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Returns
/// Ok((ahead, behind)) on success, Err(msg) on failure (e.g., no upstream)
///
/// # Errors
/// Returns an error if `git rev-list` fails or returns unexpected output.
pub async fn get_ahead_behind(repo_path: PathBuf) -> Result<(u32, u32), String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["rev-list", "--left-right", "--count", "HEAD...@{u}"])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Git rev-list failed: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = stdout.split_whitespace().collect();
        if parts.len() >= 2 {
            let ahead = parts[0].parse().unwrap_or(0);
            let behind = parts[1].parse().unwrap_or(0);
            Ok((ahead, behind))
        } else {
            Err("Unexpected output format from git rev-list".to_string())
        }
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Returns the origin repository URL normalized to HTTPS form when possible.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(url) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if the remote URL cannot be read via `git remote get-url`.
pub async fn repo_url(repo_path: PathBuf) -> Result<String, String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(&repo_path)
            .output()
            .map_err(|e| format!("Failed to execute git remote get-url: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Git remote get-url failed: {}", stderr.trim()));
        }

        let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(normalize_repo_url(&remote))
    })
    .await
    .map_err(|e| format!("Join error: {e}"))?
}

/// Synchronous version of `detect_git_info` for internal use.
fn detect_git_info_sync(dir: &Path) -> Option<String> {
    let repo_dir = find_git_repo(dir)?;
    get_git_branch(&repo_dir)
}

/// Legacy alias for `find_git_repo_root`, kept for internal use.
fn find_git_repo(dir: &Path) -> Option<PathBuf> {
    find_git_repo_root_sync(dir)
}

/// Synchronous version of `find_git_repo_root` for internal use.
fn find_git_repo_root_sync(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        let git_dir = current.join(".git");
        if git_dir.exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Reads .git/HEAD and extracts the current branch name.
/// Returns the branch name for normal HEAD, or "HEAD@{hash}" for detached HEAD.
fn get_git_branch(repo_dir: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(repo_dir)?;
    let head_path = git_dir.join("HEAD");
    let content = fs::read_to_string(head_path).ok()?;
    let content = content.trim();

    // Normal branch: "ref: refs/heads/main"
    if let Some(branch_ref) = content.strip_prefix("ref: refs/heads/") {
        return Some(branch_ref.to_string());
    }

    // Detached HEAD: full commit hash
    if content.len() >= 7 && content.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("HEAD@{}", &content[..7]));
    }

    None
}

fn resolve_git_dir(repo_dir: &Path) -> Option<PathBuf> {
    let dot_git = repo_dir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if dot_git.is_file() {
        let content = fs::read_to_string(&dot_git).ok()?;
        let git_dir_line = content.lines().find(|line| line.starts_with("gitdir:"))?;
        let git_dir_path = git_dir_line.trim_start_matches("gitdir:").trim();
        let git_dir = PathBuf::from(git_dir_path);

        if git_dir.is_absolute() {
            return Some(git_dir);
        }

        return Some(repo_dir.join(git_dir));
    }

    None
}

fn run_git_command_with_index_lock_retry(
    repo_path: &Path,
    args: &[&str],
    environment: &[(&str, &str)],
) -> Result<std::process::Output, String> {
    for attempt in 0..GIT_INDEX_LOCK_RETRY_ATTEMPTS {
        let mut command = Command::new("git");
        command.args(args).current_dir(repo_path);

        for (key, value) in environment {
            command.env(key, value);
        }

        let output = command
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;
        if output.status.success() {
            return Ok(output);
        }

        let detail = command_output_detail(&output.stdout, &output.stderr);
        let is_last_attempt = attempt + 1 == GIT_INDEX_LOCK_RETRY_ATTEMPTS;
        if !is_git_index_lock_error(&detail) || is_last_attempt {
            return Ok(output);
        }

        std::thread::sleep(GIT_INDEX_LOCK_RETRY_DELAY);
    }

    unreachable!("index lock retry loop should always return an output")
}

fn stage_all_sync(repo_path: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(format!("Failed to stage changes: {detail}"));
    }

    Ok(())
}

fn run_commit_command(
    repo_path: &Path,
    commit_message: &str,
    no_verify: bool,
) -> Result<std::process::Output, String> {
    let mut args = vec!["commit", "-m", commit_message];
    if no_verify {
        args.push("--no-verify");
    }

    Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))
}

fn is_hook_modified_error(stdout: &str, stderr: &str) -> bool {
    let combined = format!(
        "{stdout}
{stderr}"
    )
    .to_ascii_lowercase();

    combined.contains("files were modified by this hook")
}

fn normalize_repo_url(remote: &str) -> String {
    let trimmed = remote.trim_end_matches(".git");
    if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        return format!("https://github.com/{path}");
    }
    if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        return format!("https://github.com/{path}");
    }

    trimmed.to_string()
}

/// Extracts the best human-readable error detail from command output.
fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return stderr_text;
    }

    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    if !stdout_text.is_empty() {
        return stdout_text;
    }

    "Unknown git error".to_string()
}

fn is_git_index_lock_error(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("index.lock")
        && (normalized_detail.contains("file exists")
            || normalized_detail.contains("unable to create")
            || normalized_detail.contains("another git process"))
}

/// Returns whether git output detail indicates a rebase conflict state.
///
/// Matches all known git messages that signal a conflict requiring manual
/// resolution, including messages emitted when staging partially-resolved
/// files and attempting `git rebase --continue` prematurely.
fn is_rebase_conflict(detail: &str) -> bool {
    detail.contains("CONFLICT")
        || detail.contains("Resolve all conflicts manually")
        || detail.contains("could not apply")
        || detail.contains("mark them as resolved")
        || detail.contains("unresolved conflict")
        || detail.contains("Committing is not possible")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn run_git_command(repo_path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .expect("failed to run git command");

        assert!(
            output.status.success(),
            "git command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn setup_test_git_repo(repo_path: &Path) {
        run_git_command(repo_path, &["init", "-b", "main"]);
        run_git_command(repo_path, &["config", "user.name", "Test User"]);
        run_git_command(repo_path, &["config", "user.email", "test@example.com"]);

        fs::write(repo_path.join("README.md"), "test repo").expect("failed to write file");
        run_git_command(repo_path, &["add", "README.md"]);
        run_git_command(repo_path, &["commit", "-m", "Initial commit"]);
    }

    #[tokio::test]
    async fn test_squash_merge_returns_committed_when_changes_exist() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        run_git_command(dir.path(), &["checkout", "-b", "feature-branch"]);
        fs::write(dir.path().join("feature.txt"), "feature content").expect("failed to write file");
        run_git_command(dir.path(), &["add", "feature.txt"]);
        run_git_command(dir.path(), &["commit", "-m", "Add feature"]);
        run_git_command(dir.path(), &["checkout", "main"]);

        // Act
        let result = squash_merge(
            dir.path().to_path_buf(),
            "feature-branch".to_string(),
            "main".to_string(),
            "Squash merge feature".to_string(),
        )
        .await;

        // Assert
        assert_eq!(result, Ok(SquashMergeOutcome::Committed));
    }

    #[tokio::test]
    async fn test_squash_merge_returns_already_present_when_changes_exist_in_target() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
        fs::write(dir.path().join("session.txt"), "session change").expect("failed to write file");
        run_git_command(dir.path(), &["add", "session.txt"]);
        run_git_command(dir.path(), &["commit", "-m", "Session change"]);
        run_git_command(dir.path(), &["checkout", "main"]);
        fs::write(dir.path().join("session.txt"), "session change").expect("failed to write file");
        run_git_command(dir.path(), &["add", "session.txt"]);
        run_git_command(dir.path(), &["commit", "-m", "Apply same change on main"]);

        // Act
        let result = squash_merge(
            dir.path().to_path_buf(),
            "session-branch".to_string(),
            "main".to_string(),
            "Merge session".to_string(),
        )
        .await;

        // Assert
        assert_eq!(result, Ok(SquashMergeOutcome::AlreadyPresentInTarget));
    }

    #[test]
    fn test_is_rebase_conflict_detects_conflict_keyword() {
        // Arrange
        let detail = "CONFLICT (content): Merge conflict in src/main.rs";

        // Act / Assert
        assert!(is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_could_not_apply() {
        // Arrange
        let detail = "error: could not apply abc1234... Update handler";

        // Act / Assert
        assert!(is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_mark_as_resolved() {
        // Arrange
        let detail = "hint: mark them as resolved using git add";

        // Act / Assert
        assert!(is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_unresolved_conflict() {
        // Arrange
        let detail = "fatal: Exiting because of an unresolved conflict.";

        // Act / Assert
        assert!(is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_committing_not_possible() {
        // Arrange
        let detail = "error: Committing is not possible because you have unmerged files.";

        // Act / Assert
        assert!(is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_returns_false_for_unrelated_error() {
        // Arrange
        let detail = "fatal: not a git repository (or any parent up to mount point /)";

        // Act / Assert
        assert!(!is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_returns_false_for_index_lock_error() {
        // Arrange
        let detail = "fatal: Unable to create '.git/index.lock': File exists.";

        // Act / Assert
        assert!(!is_rebase_conflict(detail));
    }
}
