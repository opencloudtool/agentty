use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tokio::task::spawn_blocking;

use super::rebase::{is_rebase_conflict, run_git_command_with_index_lock_retry};
use super::repo::command_output_detail;

const COMMIT_ALL_HOOK_RETRY_ATTEMPTS: usize = 5;

/// Result of attempting `git pull --rebase`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PullRebaseResult {
    /// Pull and rebase completed successfully.
    Completed,
    /// Pull stopped because of merge conflicts.
    Conflict { detail: String },
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
    commit_all_with_retry(repo_path, commit_message, no_verify, false).await
}

/// Stages all changes and keeps a single commit for the provided message.
///
/// Creates a new commit when `HEAD` does not already use `commit_message`.
/// Otherwise, amends `HEAD` with `--amend --no-edit` so the branch keeps one
/// evolving session commit.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `commit_message` - Message that identifies the session commit
/// * `no_verify` - When `true`, skips pre-commit and commit-msg hooks
///   (`--no-verify`)
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if staging, commit lookup, or committing changes fails.
pub async fn commit_all_preserving_single_commit(
    repo_path: PathBuf,
    commit_message: String,
    no_verify: bool,
) -> Result<(), String> {
    let amend_existing_commit =
        should_amend_existing_commit(repo_path.clone(), commit_message.clone()).await?;

    commit_all_with_retry(repo_path, commit_message, no_verify, amend_existing_commit).await
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
        .map_err(|error| format!("Join error: {error}"))?
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
            .map_err(|error| format!("Failed to execute git: {error}"))?;

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
    .map_err(|error| format!("Join error: {error}"))?
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
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Git branch deletion failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
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
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !intent_to_add.status.success() {
            let stderr = String::from_utf8_lossy(&intent_to_add.stderr);

            return Err(format!("Git add --intent-to-add failed: {}", stderr.trim()));
        }

        let merge_base_output = Command::new("git")
            .args(["merge-base", "HEAD", &base_branch])
            .current_dir(&repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

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
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        let reset = Command::new("git")
            .arg("reset")
            .current_dir(&repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !reset.status.success() {
            let stderr = String::from_utf8_lossy(&reset.stderr);

            return Err(format!("Git reset failed: {}", stderr.trim()));
        }

        Ok(String::from_utf8_lossy(&diff_output.stdout).into_owned())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Returns whether a repository or worktree has no uncommitted changes.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// `true` when `git status --porcelain` is empty, `false` otherwise.
///
/// # Errors
/// Returns an error if `git status --porcelain` cannot be executed.
pub async fn is_worktree_clean(repo_path: PathBuf) -> Result<bool, String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(format!("Git status --porcelain failed: {detail}"));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().is_empty())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Runs `git pull --rebase` and returns conflict outcome when applicable.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// A [`PullRebaseResult`] describing whether pull/rebase completed or stopped
/// on conflicts.
///
/// # Errors
/// Returns an error for non-conflict pull/rebase failures.
pub async fn pull_rebase(repo_path: PathBuf) -> Result<PullRebaseResult, String> {
    spawn_blocking(move || {
        let output = run_git_command_with_index_lock_retry(
            &repo_path,
            &["pull", "--rebase"],
            &[("GIT_EDITOR", ":"), ("GIT_SEQUENCE_EDITOR", ":")],
        )?;

        if output.status.success() {
            return Ok(PullRebaseResult::Completed);
        }

        let detail = command_output_detail(&output.stdout, &output.stderr);
        if is_rebase_conflict(&detail) {
            return Ok(PullRebaseResult::Conflict { detail });
        }

        Err(format!("Failed to pull with rebase: {detail}."))
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Pushes the current branch to its upstream remote.
///
/// Falls back to `git push --set-upstream origin HEAD` when no upstream branch
/// is configured.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure.
///
/// # Errors
/// Returns an error if `git push` fails.
pub async fn push_current_branch(repo_path: PathBuf) -> Result<(), String> {
    spawn_blocking(move || {
        let push_output = Command::new("git")
            .arg("push")
            .current_dir(&repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if push_output.status.success() {
            return Ok(());
        }

        let push_detail = command_output_detail(&push_output.stdout, &push_output.stderr);
        if !is_no_upstream_error(&push_detail) {
            return Err(format!("Git push failed: {push_detail}"));
        }

        let upstream_output = Command::new("git")
            .args(["push", "--set-upstream", "origin", "HEAD"])
            .current_dir(&repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !upstream_output.status.success() {
            let upstream_detail =
                command_output_detail(&upstream_output.stdout, &upstream_output.stderr);

            return Err(format!("Git push failed: {upstream_detail}"));
        }

        Ok(())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
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
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Git fetch failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
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
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Git rev-list failed: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = stdout.split_whitespace().collect();
        if parts.len() >= 2 {
            let ahead = parts[0].parse().unwrap_or(0);
            let behind = parts[1].parse().unwrap_or(0);

            return Ok((ahead, behind));
        }

        Err("Unexpected output format from git rev-list".to_string())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Stages all changes and commits or amends with retry behavior for hook
/// rewrites.
async fn commit_all_with_retry(
    repo_path: PathBuf,
    commit_message: String,
    no_verify: bool,
    amend_existing_commit: bool,
) -> Result<(), String> {
    spawn_blocking(move || {
        stage_all_sync(&repo_path)?;

        for _ in 0..COMMIT_ALL_HOOK_RETRY_ATTEMPTS {
            let output = run_commit_command(
                &repo_path,
                &commit_message,
                no_verify,
                amend_existing_commit,
            )?;

            if output.status.success() {
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
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
    .map_err(|error| format!("Join error: {error}"))?
}

/// Returns whether `HEAD` should be amended for the incoming commit message.
///
/// When `HEAD` already has `commit_message`, new staged changes should be
/// folded into the same commit.
async fn should_amend_existing_commit(
    repo_path: PathBuf,
    commit_message: String,
) -> Result<bool, String> {
    spawn_blocking(move || {
        let Some(head_commit_message) = head_commit_message_sync(&repo_path)? else {
            return Ok(false);
        };

        Ok(head_commit_message.trim() == commit_message)
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Stages all changed files in the repository.
///
/// Uses shared git retry behavior for transient `index.lock` contention.
fn stage_all_sync(repo_path: &Path) -> Result<(), String> {
    let output = run_git_command_with_index_lock_retry(repo_path, &["add", "-A"], &[])?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(format!("Failed to stage changes: {detail}"));
    }

    Ok(())
}

/// Returns the full `HEAD` commit message, or `None` when no commits exist.
fn head_commit_message_sync(repo_path: &Path) -> Result<Option<String>, String> {
    if !has_head_commit_sync(repo_path)? {
        return Ok(None);
    }

    let output = Command::new("git")
        .args(["log", "-1", "--pretty=%B"])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(format!("Failed to read HEAD commit message: {detail}"));
    }

    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// Returns whether `HEAD` resolves to an existing commit.
fn has_head_commit_sync(repo_path: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))?;

    if output.status.success() {
        return Ok(true);
    }

    let detail = command_output_detail(&output.stdout, &output.stderr);
    let normalized_detail = detail.to_ascii_lowercase();
    if normalized_detail.contains("needed a single revision")
        || normalized_detail.contains("unknown revision")
        || normalized_detail.contains("does not have any commits yet")
    {
        return Ok(false);
    }

    Err(format!("Failed to resolve HEAD: {detail}"))
}

/// Runs `git commit` with optional amend and hook settings.
///
/// Uses shared git retry behavior for transient `index.lock` contention.
fn run_commit_command(
    repo_path: &Path,
    commit_message: &str,
    no_verify: bool,
    amend_existing_commit: bool,
) -> Result<Output, String> {
    let mut args = vec!["commit"];
    if amend_existing_commit {
        args.push("--amend");
        args.push("--no-edit");
    } else {
        args.push("-m");
        args.push(commit_message);
    }

    if no_verify {
        args.push("--no-verify");
    }

    run_git_command_with_index_lock_retry(repo_path, &args, &[])
}

/// Returns whether commit output indicates hooks rewrote files.
fn is_hook_modified_error(stdout: &str, stderr: &str) -> bool {
    let combined = format!(
        "{stdout}
{stderr}"
    )
    .to_ascii_lowercase();

    combined.contains("files were modified by this hook")
}

/// Returns whether git push output indicates a missing upstream branch.
pub(super) fn is_no_upstream_error(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("has no upstream branch")
        || normalized_detail.contains("no upstream branch")
        || normalized_detail.contains("set-upstream")
}
