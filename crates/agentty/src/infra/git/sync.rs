use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::task::spawn_blocking;

use super::rebase::{is_rebase_conflict, run_git_command_with_index_lock_retry};
use super::repo::{
    command_output_detail, run_git_command, run_git_command_output_sync, run_git_command_sync,
};

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
    let hash = run_git_command(
        repo_path,
        vec![
            "rev-parse".to_string(),
            "--short".to_string(),
            "HEAD".to_string(),
        ],
        "Failed to resolve HEAD hash".to_string(),
    )
    .await?;
    let hash = hash.trim().to_string();
    if hash.is_empty() {
        return Err("Failed to resolve HEAD hash: empty output".to_string());
    }

    Ok(hash)
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
    run_git_command(
        repo_path,
        vec!["branch".to_string(), "-D".to_string(), branch_name],
        "Git branch deletion failed".to_string(),
    )
    .await?;

    Ok(())
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
        run_git_command_sync(
            &repo_path,
            &["add", "-A", "--intent-to-add"],
            "Git add --intent-to-add failed",
        )?;

        let merge_base_output =
            run_git_command_output_sync(&repo_path, &["merge-base", "HEAD", &base_branch])?;

        let diff_target = if merge_base_output.status.success() {
            String::from_utf8_lossy(&merge_base_output.stdout)
                .trim()
                .to_string()
        } else {
            base_branch
        };

        let diff_output = run_git_command_sync(
            &repo_path,
            &["diff", diff_target.as_str()],
            "Git diff failed",
        )?;
        run_git_command_sync(&repo_path, &["reset"], "Git reset failed")?;

        Ok(diff_output)
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
    let status_output = run_git_command(
        repo_path,
        vec!["status".to_string(), "--porcelain".to_string()],
        "Git status --porcelain failed".to_string(),
    )
    .await?;

    Ok(status_output.trim().is_empty())
}

/// Runs `git pull --rebase` and returns conflict outcome when applicable.
///
/// When an upstream branch can be resolved, this uses an explicit
/// `git pull --rebase <remote> <branch>` target to avoid ambiguous rebase
/// failures caused by multiple configured merge branches.
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
        let pull_arguments = pull_rebase_arguments(&repo_path)
            .unwrap_or_else(|_| vec!["pull".to_string(), "--rebase".to_string()]);
        let pull_argument_refs: Vec<&str> = pull_arguments.iter().map(String::as_str).collect();
        let output = run_git_command_with_index_lock_retry(
            &repo_path,
            &pull_argument_refs,
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

/// Builds pull arguments that target a single upstream branch when available.
///
/// Resolves an explicit `<remote> <branch>` pull target for both remote and
/// local upstreams so git does not need to infer one from branch config.
fn pull_rebase_arguments(repo_path: &Path) -> Result<Vec<String>, String> {
    let upstream_reference = primary_upstream_reference(repo_path)?;

    if let Some((remote_name, branch_name)) = upstream_reference.split_once('/') {
        return Ok(vec![
            "pull".to_string(),
            "--rebase".to_string(),
            remote_name.to_string(),
            branch_name.to_string(),
        ]);
    }

    let remote_name = current_branch_remote_name(repo_path)?;

    Ok(vec![
        "pull".to_string(),
        "--rebase".to_string(),
        remote_name,
        upstream_reference,
    ])
}

/// Returns the first upstream reference reported for `HEAD`.
///
/// Git can return multiple lines when multiple merge targets are configured.
/// Pulling with rebase needs one concrete target, so this selects the first
/// non-empty line.
fn primary_upstream_reference(repo_path: &Path) -> Result<String, String> {
    let upstream_reference = upstream_reference_name(repo_path)?;
    let Some(primary_reference) = upstream_reference
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
    else {
        return Err("Failed to resolve upstream branch: empty output".to_string());
    };

    Ok(primary_reference.to_string())
}

/// Returns the full upstream reference for `HEAD` (for example, `origin/main`).
fn upstream_reference_name(repo_path: &Path) -> Result<String, String> {
    let upstream_reference = run_git_command_sync(
        repo_path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        "Failed to resolve upstream branch",
    )?;
    let upstream_reference = upstream_reference.trim().to_string();
    if upstream_reference.is_empty() {
        return Err("Failed to resolve upstream branch: empty output".to_string());
    }

    Ok(upstream_reference)
}

/// Returns the configured remote name for the current local branch.
///
/// This is used when the upstream short name omits a remote prefix (for
/// example, `main` with `branch.<name>.remote=.`).
fn current_branch_remote_name(repo_path: &Path) -> Result<String, String> {
    let current_branch_name = current_branch_name(repo_path)?;
    let remote_config_key = format!("branch.{current_branch_name}.remote");
    let remote_name = run_git_command_sync(
        repo_path,
        &["config", "--get", &remote_config_key],
        &format!("Failed to resolve current branch remote `{remote_config_key}`"),
    )?;
    let remote_name = remote_name.trim().to_string();
    if remote_name.is_empty() {
        return Err(format!(
            "Failed to resolve current branch remote `{remote_config_key}`: empty output"
        ));
    }

    Ok(remote_name)
}

/// Returns the current local branch name for `HEAD`.
fn current_branch_name(repo_path: &Path) -> Result<String, String> {
    let branch_name = run_git_command_sync(
        repo_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "Failed to resolve current branch name",
    )?;
    let branch_name = branch_name.trim().to_string();
    if branch_name.is_empty() {
        return Err("Failed to resolve current branch name: empty output".to_string());
    }

    if branch_name == "HEAD" {
        return Err("Failed to resolve current branch name: detached HEAD".to_string());
    }

    Ok(branch_name)
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
        let push_output = run_git_command_output_sync(&repo_path, &["push"])?;

        if push_output.status.success() {
            return Ok(());
        }

        let push_detail = command_output_detail(&push_output.stdout, &push_output.stderr);
        if !is_no_upstream_error(&push_detail) {
            return Err(format!("Git push failed: {push_detail}"));
        }

        run_git_command_sync(
            &repo_path,
            &["push", "--set-upstream", "origin", "HEAD"],
            "Git push failed",
        )?;

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
    run_git_command(
        repo_path,
        vec!["fetch".to_string()],
        "Git fetch failed".to_string(),
    )
    .await?;

    Ok(())
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
    let rev_list_output = run_git_command(
        repo_path,
        vec![
            "rev-list".to_string(),
            "--left-right".to_string(),
            "--count".to_string(),
            "HEAD...@{u}".to_string(),
        ],
        "Git rev-list failed".to_string(),
    )
    .await?;
    let parts: Vec<&str> = rev_list_output.split_whitespace().collect();
    if parts.len() >= 2 {
        let ahead = parts[0].parse().unwrap_or(0);
        let behind = parts[1].parse().unwrap_or(0);

        return Ok((ahead, behind));
    }

    Err("Unexpected output format from git rev-list".to_string())
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

            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(format!("Failed to commit: {detail}"));
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

    let output = run_git_command_sync(
        repo_path,
        &["log", "-1", "--pretty=%B"],
        "Failed to read HEAD commit message",
    )?;

    Ok(Some(output.trim().to_string()))
}

/// Returns whether `HEAD` resolves to an existing commit.
fn has_head_commit_sync(repo_path: &Path) -> Result<bool, String> {
    let output = run_git_command_output_sync(repo_path, &["rev-parse", "--verify", "HEAD"])?;

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
