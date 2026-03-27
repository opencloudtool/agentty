use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::task::spawn_blocking;

use super::error::GitError;
use super::rebase::{is_rebase_conflict, run_git_command_with_index_lock_retry};
use super::repo::{
    command_output_detail, run_git_command, run_git_command_output_sync, run_git_command_sync,
};

/// Map of local branch names to their ahead/behind counts relative to their
/// tracked upstream branch. `None` indicates no upstream or a gone upstream.
pub type BranchTrackingMap = HashMap<String, Option<(u32, u32)>>;

const COMMIT_ALL_HOOK_RETRY_ATTEMPTS: usize = 5;

/// Controls how single-commit session branches treat the commit message when
/// amending `HEAD`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SingleCommitMessageStrategy {
    /// Replaces the existing `HEAD` message with the newly generated message.
    Replace,
    /// Keeps the current `HEAD` message while amending file content only.
    Reuse,
}

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
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if staging or committing changes fails.
pub async fn commit_all(
    repo_path: PathBuf,
    commit_message: String,
    no_verify: bool,
) -> Result<(), GitError> {
    commit_all_with_retry(
        repo_path,
        commit_message,
        SingleCommitMessageStrategy::Replace,
        no_verify,
        false,
    )
    .await
}

/// Stages all changes and keeps a single commit for the provided message.
///
/// Creates a new commit when `HEAD` has no commits beyond `base_branch`.
/// Otherwise, amends `HEAD` so the branch keeps one evolving session commit.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `base_branch` - Branch used to detect whether a session commit already
///   exists on `HEAD`
/// * `commit_message` - Message that identifies the session commit
/// * `message_strategy` - Whether amends replace or reuse the existing `HEAD`
///   message
/// * `no_verify` - When `true`, skips pre-commit and commit-msg hooks
///   (`--no-verify`)
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if staging, commit lookup, or committing changes
/// fails.
pub async fn commit_all_preserving_single_commit(
    repo_path: PathBuf,
    base_branch: String,
    commit_message: String,
    message_strategy: SingleCommitMessageStrategy,
    no_verify: bool,
) -> Result<(), GitError> {
    let amend_existing_commit = has_commits_since(repo_path.clone(), base_branch).await?;

    commit_all_with_retry(
        repo_path,
        commit_message,
        message_strategy,
        no_verify,
        amend_existing_commit,
    )
    .await
}

/// Stages all changes in the repository or worktree.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if `git add -A` fails.
pub async fn stage_all(repo_path: PathBuf) -> Result<(), GitError> {
    spawn_blocking(move || stage_all_sync(&repo_path)).await?
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
/// Returns a [`GitError`] if resolving `HEAD` fails.
pub async fn head_short_hash(repo_path: PathBuf) -> Result<String, GitError> {
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
        return Err(GitError::OutputParse(
            "Failed to resolve HEAD hash: empty output".to_string(),
        ));
    }

    Ok(hash)
}

/// Returns the full `HEAD` commit message, or `None` when no commits exist.
///
/// # Errors
/// Returns a [`GitError`] if `HEAD` cannot be inspected.
pub async fn head_commit_message(repo_path: PathBuf) -> Result<Option<String>, GitError> {
    spawn_blocking(move || head_commit_message_sync(&repo_path)).await?
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
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if the branch delete command fails.
pub async fn delete_branch(repo_path: PathBuf, branch_name: String) -> Result<(), GitError> {
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
/// fork point. To avoid re-showing squash-merged/cherry-picked session commits
/// on non-rebased branches, this also checks `git cherry` and, when applicable,
/// diffs from the last leading commit already applied to `base_branch`.
/// Finally resets the index to restore the original state.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `base_branch` - Branch to diff against (e.g., `main`)
///
/// # Returns
/// The diff output as a string.
///
/// # Errors
/// Returns a [`GitError`] if preparing the index, generating the diff, or
/// restoring index state fails.
pub async fn diff(repo_path: PathBuf, base_branch: String) -> Result<String, GitError> {
    spawn_blocking(move || -> Result<String, GitError> {
        run_git_command_sync(
            &repo_path,
            &["add", "-A", "--intent-to-add"],
            "Git add --intent-to-add failed",
        )?;

        let merge_base_output =
            run_git_command_output_sync(&repo_path, &["merge-base", "HEAD", &base_branch])?;

        let diff_target = if merge_base_output.status.success() {
            resolve_diff_target(
                &repo_path,
                &base_branch,
                String::from_utf8_lossy(&merge_base_output.stdout).trim(),
            )?
        } else {
            base_branch
        };

        let diff_output = run_git_command_sync(
            &repo_path,
            &["diff", diff_target.as_str()],
            "Git diff failed",
        );
        let reset_result = run_git_command_sync(&repo_path, &["reset"], "Git reset failed");

        if let Err(diff_error) = diff_output {
            return match reset_result {
                Ok(_) => Err(diff_error),
                Err(reset_error) => Err(GitError::CommandFailed {
                    command: "git diff".to_string(),
                    stderr: format!(
                        "{diff_error} Additionally failed to restore index state: {reset_error}"
                    ),
                }),
            };
        }

        reset_result?;

        diff_output
    })
    .await?
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
/// Returns a [`GitError`] if `git status --porcelain` cannot be executed.
pub async fn is_worktree_clean(repo_path: PathBuf) -> Result<bool, GitError> {
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
/// Returns a [`GitError`] for non-conflict pull/rebase failures.
pub async fn pull_rebase(repo_path: PathBuf) -> Result<PullRebaseResult, GitError> {
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

        Err(GitError::CommandFailed {
            command: "git pull --rebase".to_string(),
            stderr: detail,
        })
    })
    .await?
}

/// Builds pull arguments that target a single upstream branch when available.
///
/// Resolves an explicit `<remote> <branch>` pull target for both remote and
/// local upstreams so git does not need to infer one from branch config.
fn pull_rebase_arguments(repo_path: &Path) -> Result<Vec<String>, GitError> {
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
fn primary_upstream_reference(repo_path: &Path) -> Result<String, GitError> {
    let upstream_reference = upstream_reference_name(repo_path)?;
    let Some(primary_reference) = upstream_reference
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
    else {
        return Err(GitError::OutputParse(
            "Failed to resolve upstream branch: empty output".to_string(),
        ));
    };

    Ok(primary_reference.to_string())
}

/// Returns the full upstream reference for `HEAD` (for example, `origin/main`).
fn upstream_reference_name(repo_path: &Path) -> Result<String, GitError> {
    let upstream_reference = run_git_command_sync(
        repo_path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        "Failed to resolve upstream branch",
    )?;
    let upstream_reference = upstream_reference.trim().to_string();
    if upstream_reference.is_empty() {
        return Err(GitError::OutputParse(
            "Failed to resolve upstream branch: empty output".to_string(),
        ));
    }

    Ok(upstream_reference)
}

/// Returns the configured remote name for the current local branch.
///
/// This is used when the upstream short name omits a remote prefix (for
/// example, `main` with `branch.<name>.remote=.`).
fn current_branch_remote_name(repo_path: &Path) -> Result<String, GitError> {
    let current_branch_name = current_branch_name(repo_path)?;
    let remote_config_key = format!("branch.{current_branch_name}.remote");
    let remote_name = run_git_command_sync(
        repo_path,
        &["config", "--get", &remote_config_key],
        &format!("Failed to resolve current branch remote `{remote_config_key}`"),
    )?;
    let remote_name = remote_name.trim().to_string();
    if remote_name.is_empty() {
        return Err(GitError::OutputParse(format!(
            "Failed to resolve current branch remote `{remote_config_key}`: empty output"
        )));
    }

    Ok(remote_name)
}

/// Returns the current local branch name for `HEAD`.
fn current_branch_name(repo_path: &Path) -> Result<String, GitError> {
    let branch_name = run_git_command_sync(
        repo_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "Failed to resolve current branch name",
    )?;
    let branch_name = branch_name.trim().to_string();
    if branch_name.is_empty() {
        return Err(GitError::OutputParse(
            "Failed to resolve current branch name: empty output".to_string(),
        ));
    }

    if branch_name == "HEAD" {
        return Err(GitError::OutputParse(
            "Failed to resolve current branch name: detached HEAD".to_string(),
        ));
    }

    Ok(branch_name)
}

/// Pushes the current branch to its upstream remote with
/// `--force-with-lease`.
///
/// Falls back to `git push --force-with-lease --set-upstream origin HEAD`
/// when no upstream branch is configured, then returns the resolved upstream
/// reference.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// The upstream reference on success.
///
/// # Errors
/// Returns a [`GitError`] if `git push` fails or upstream tracking cannot be
/// resolved afterwards.
pub async fn push_current_branch(repo_path: PathBuf) -> Result<String, GitError> {
    spawn_blocking(move || -> Result<String, GitError> {
        let push_output = run_git_command_output_sync(&repo_path, &["push", "--force-with-lease"])?;

        if push_output.status.success() {
            return primary_upstream_reference(&repo_path);
        }

        let push_detail = command_output_detail(&push_output.stdout, &push_output.stderr);
        if !is_no_upstream_error(&push_detail) {
            return Err(GitError::CommandFailed {
                command: "git push".to_string(),
                stderr: push_detail,
            });
        }

        run_git_command_sync(
            &repo_path,
            &[
                "push",
                "--force-with-lease",
                "--set-upstream",
                "origin",
                "HEAD",
            ],
            "Git push failed",
        )?;

        primary_upstream_reference(&repo_path)
    })
    .await?
}

/// Pushes the current branch to one explicit remote branch name with
/// `--force-with-lease` and returns the resulting upstream reference.
///
/// When the current branch already tracks a remote, that remote name is
/// reused. Otherwise this falls back to `origin`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `remote_branch_name` - Target branch name to create or update on the
///   remote
///
/// # Returns
/// The upstream reference on success, for example `origin/feature/review`.
///
/// # Errors
/// Returns a [`GitError`] if `git push` fails.
pub async fn push_current_branch_to_remote_branch(
    repo_path: PathBuf,
    remote_branch_name: String,
) -> Result<String, GitError> {
    spawn_blocking(move || -> Result<String, GitError> {
        let remote_name =
            current_branch_remote_name(&repo_path).unwrap_or_else(|_| "origin".to_string());
        let push_refspec = format!("HEAD:{remote_branch_name}");

        run_git_command_sync(
            &repo_path,
            &[
                "push",
                "--force-with-lease",
                "--set-upstream",
                &remote_name,
                &push_refspec,
            ],
            "Git push failed",
        )?;

        Ok(format!("{remote_name}/{remote_branch_name}"))
    })
    .await?
}

/// Returns the current upstream reference for `HEAD`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// The configured upstream reference, for example `origin/main`.
///
/// # Errors
/// Returns a [`GitError`] when upstream tracking information cannot be
/// resolved.
pub async fn current_upstream_reference(repo_path: PathBuf) -> Result<String, GitError> {
    spawn_blocking(move || primary_upstream_reference(&repo_path)).await?
}

/// Fetches from the configured remote.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if `git fetch` cannot be executed successfully.
pub async fn fetch_remote(repo_path: PathBuf) -> Result<(), GitError> {
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
/// Ok((ahead, behind)) on success.
///
/// # Errors
/// Returns a [`GitError`] if `git rev-list` fails or returns unexpected
/// output.
pub async fn get_ahead_behind(repo_path: PathBuf) -> Result<(u32, u32), GitError> {
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

    Err(GitError::OutputParse(
        "Unexpected output format from git rev-list".to_string(),
    ))
}

/// Returns ahead/behind snapshots for every local branch in `repo_path`.
///
/// The returned map is keyed by local branch name. Branches without an
/// upstream, with a gone upstream, or without ahead/behind markers map to
/// `None`.
///
/// # Errors
/// Returns a [`GitError`] if `git for-each-ref` fails.
pub async fn branch_tracking_statuses(repo_path: PathBuf) -> Result<BranchTrackingMap, GitError> {
    let git_output = run_git_command(
        repo_path,
        vec![
            "for-each-ref".to_string(),
            "--format=%(refname:short)\t%(upstream:short)\t%(upstream:track,nobracket)".to_string(),
            "refs/heads".to_string(),
        ],
        "Git for-each-ref failed".to_string(),
    )
    .await?;

    Ok(parse_branch_tracking_statuses(&git_output))
}

/// Returns upstream commit subjects that are not yet in local `HEAD`.
///
/// The returned order is oldest to newest to match pull application order.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Errors
/// Returns a [`GitError`] when `git log` fails or upstream tracking refs are
/// unavailable.
pub async fn list_upstream_commit_titles(repo_path: PathBuf) -> Result<Vec<String>, GitError> {
    let git_output = run_git_command(
        repo_path,
        vec![
            "log".to_string(),
            "--reverse".to_string(),
            "--pretty=%s".to_string(),
            "HEAD..@{u}".to_string(),
        ],
        "Git log failed".to_string(),
    )
    .await?;

    Ok(parse_commit_titles(&git_output))
}

/// Returns local commit subjects that are not yet present in upstream.
///
/// The returned order is oldest to newest to match push application order.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Errors
/// Returns a [`GitError`] when `git log` fails or upstream tracking refs are
/// unavailable.
pub async fn list_local_commit_titles(repo_path: PathBuf) -> Result<Vec<String>, GitError> {
    let git_output = run_git_command(
        repo_path,
        vec![
            "log".to_string(),
            "--reverse".to_string(),
            "--pretty=%s".to_string(),
            "@{u}..HEAD".to_string(),
        ],
        "Git log failed".to_string(),
    )
    .await?;

    Ok(parse_commit_titles(&git_output))
}

/// Returns whether `HEAD` contains commits that are not reachable from
/// `base_branch`.
///
/// # Errors
/// Returns a [`GitError`] if commit ancestry cannot be queried.
pub async fn has_commits_since(repo_path: PathBuf, base_branch: String) -> Result<bool, GitError> {
    spawn_blocking(move || -> Result<bool, GitError> {
        let rev_list_output = run_git_command_sync(
            &repo_path,
            &["rev-list", "--count", &format!("{base_branch}..HEAD")],
            "Failed to count commits since base branch",
        )?;
        let commit_count = rev_list_output.trim().parse::<u32>().map_err(|error| {
            GitError::OutputParse(format!(
                "Failed to parse commit count since base branch `{base_branch}`: {error}"
            ))
        })?;

        Ok(commit_count > 0)
    })
    .await?
}

/// Parses newline-delimited commit subjects from `git log` output.
fn parse_commit_titles(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Parses repo-wide branch tracking information from `git for-each-ref`.
fn parse_branch_tracking_statuses(output: &str) -> BranchTrackingMap {
    let mut branch_tracking_statuses = HashMap::new();

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut parts = line.splitn(3, '\t');
        let Some(branch_name) = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let upstream_ref = parts.next().map(str::trim).unwrap_or_default();
        let track = parts.next().map(str::trim).unwrap_or_default();

        let status = if upstream_ref.is_empty() {
            None
        } else {
            parse_branch_tracking_counts(track)
        };
        branch_tracking_statuses.insert(branch_name.to_string(), status);
    }

    branch_tracking_statuses
}

/// Parses one `%(upstream:track,nobracket)` marker into ahead/behind counts.
fn parse_branch_tracking_counts(track: &str) -> Option<(u32, u32)> {
    let normalized_track = track.trim();
    if normalized_track.is_empty() || normalized_track == "gone" {
        return None;
    }

    let mut ahead = 0;
    let mut behind = 0;

    for part in normalized_track.split(',').map(str::trim) {
        if let Some(count) = part.strip_prefix("ahead ") {
            ahead = count.parse().ok()?;
        } else if let Some(count) = part.strip_prefix("behind ") {
            behind = count.parse().ok()?;
        }
    }

    Some((ahead, behind))
}

/// Resolves the commit/tree to use as the `git diff` "before" side.
///
/// Starts from the merge-base fallback and, when `git cherry` reports leading
/// commits already applied to `base_branch`, advances the baseline to the last
/// such commit so squash-merged session changes are not shown again.
fn resolve_diff_target(
    repo_path: &Path,
    base_branch: &str,
    merge_base: &str,
) -> Result<String, GitError> {
    let cherry_output = run_git_command_output_sync(repo_path, &["cherry", base_branch, "HEAD"])?;
    if !cherry_output.status.success() {
        return Ok(merge_base.to_string());
    }

    let cherry_stdout = String::from_utf8_lossy(&cherry_output.stdout);
    let Some(last_leading_applied_commit) = last_leading_applied_commit(&cherry_stdout) else {
        return Ok(merge_base.to_string());
    };

    Ok(last_leading_applied_commit.to_string())
}

/// Returns the last leading commit from `git cherry` marked as already applied.
///
/// `git cherry` prefixes commits with `-` when an equivalent patch exists in
/// the upstream branch and `+` when it does not. This helper only consumes the
/// initial contiguous `-` block and stops at the first `+` to avoid dropping
/// non-merged changes.
fn last_leading_applied_commit(cherry_output: &str) -> Option<&str> {
    let mut last_applied_commit = None;

    for line in cherry_output.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        let mut parts = trimmed_line.split_whitespace();
        let marker = parts.next()?;
        let commit_hash = parts.next()?;

        if marker == "-" {
            last_applied_commit = Some(commit_hash);

            continue;
        }

        if marker == "+" {
            break;
        }

        break;
    }

    last_applied_commit
}

/// Stages all changes and commits or amends with retry behavior for hook
/// rewrites.
async fn commit_all_with_retry(
    repo_path: PathBuf,
    commit_message: String,
    message_strategy: SingleCommitMessageStrategy,
    no_verify: bool,
    amend_existing_commit: bool,
) -> Result<(), GitError> {
    spawn_blocking(move || {
        stage_all_sync(&repo_path)?;

        for _ in 0..COMMIT_ALL_HOOK_RETRY_ATTEMPTS {
            let output = run_commit_command(
                &repo_path,
                &commit_message,
                message_strategy,
                no_verify,
                amend_existing_commit,
            )?;

            if output.status.success() {
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
                return Err(GitError::CommandFailed {
                    command: "git commit".to_string(),
                    stderr: "Nothing to commit: no changes detected".to_string(),
                });
            }

            if is_hook_modified_error(&stdout, &stderr) {
                stage_all_sync(&repo_path)?;

                continue;
            }

            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(GitError::CommandFailed {
                command: "git commit".to_string(),
                stderr: detail,
            });
        }

        Err(GitError::CommandFailed {
            command: "git commit".to_string(),
            stderr: format!(
                "Failed to commit: commit hooks kept modifying files after \
                 {COMMIT_ALL_HOOK_RETRY_ATTEMPTS} attempts"
            ),
        })
    })
    .await?
}

/// Stages all changed files in the repository.
///
/// Uses shared git retry behavior for transient `index.lock` contention.
fn stage_all_sync(repo_path: &Path) -> Result<(), GitError> {
    let output = run_git_command_with_index_lock_retry(repo_path, &["add", "-A"], &[])?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(GitError::CommandFailed {
            command: "git add -A".to_string(),
            stderr: format!("Failed to stage changes: {detail}"),
        });
    }

    Ok(())
}

/// Returns the full `HEAD` commit message, or `None` when no commits exist.
fn head_commit_message_sync(repo_path: &Path) -> Result<Option<String>, GitError> {
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
fn has_head_commit_sync(repo_path: &Path) -> Result<bool, GitError> {
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

    Err(GitError::CommandFailed {
        command: "git rev-parse --verify HEAD".to_string(),
        stderr: detail,
    })
}

/// Runs `git commit` with optional amend and hook settings.
///
/// Uses shared git retry behavior for transient `index.lock` contention.
fn run_commit_command(
    repo_path: &Path,
    commit_message: &str,
    message_strategy: SingleCommitMessageStrategy,
    no_verify: bool,
    amend_existing_commit: bool,
) -> Result<Output, GitError> {
    let mut args = vec!["commit"];
    if amend_existing_commit {
        args.push("--amend");
        match message_strategy {
            SingleCommitMessageStrategy::Replace => {
                args.push("-m");
                args.push(commit_message);
            }
            SingleCommitMessageStrategy::Reuse => {
                args.push("--no-edit");
            }
        }
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Output};

    use tempfile::tempdir;

    use super::*;

    /// Runs `git` in `repo_path` and asserts the command succeeds.
    fn run_git_command(repo_path: &Path, args: &[&str]) {
        let output = git_command_output(repo_path, args);

        assert!(
            output.status.success(),
            "git command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Runs `git` in `repo_path` and returns the captured command output.
    fn git_command_output(repo_path: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .expect("failed to run git command")
    }

    /// Runs `git` in `repo_path`, asserts success, and returns trimmed stdout.
    fn git_command_stdout(repo_path: &Path, args: &[&str]) -> String {
        let output = git_command_output(repo_path, args);

        assert!(
            output.status.success(),
            "git command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8(output.stdout)
            .expect("git stdout should be valid utf-8")
            .trim()
            .to_string()
    }

    /// Creates a committed repository rooted at `repo_path`.
    fn setup_test_git_repo(repo_path: &Path) {
        run_git_command(repo_path, &["init", "-b", "main"]);
        run_git_command(repo_path, &["config", "user.name", "Test User"]);
        run_git_command(repo_path, &["config", "user.email", "test@example.com"]);
        fs::write(repo_path.join("README.md"), "base\n").expect("failed to write base file");
        run_git_command(repo_path, &["add", "README.md"]);
        run_git_command(repo_path, &["commit", "-m", "Initial commit"]);
    }

    #[test]
    fn current_branch_name_returns_error_for_detached_head() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "--detach"]);

        // Act
        let result = current_branch_name(temp_dir.path());

        // Assert
        let error = result.expect_err("detached HEAD should fail");
        assert!(error.to_string().contains("detached HEAD"));
    }

    #[test]
    fn primary_upstream_reference_uses_first_non_empty_line() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
        run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
        run_git_command(
            temp_dir.path(),
            &[
                "config",
                "--replace-all",
                "branch.main.merge",
                "refs/heads/main",
            ],
        );
        run_git_command(
            temp_dir.path(),
            &["config", "--add", "branch.main.merge", "refs/heads/feature"],
        );

        // Act
        let upstream_reference =
            primary_upstream_reference(temp_dir.path()).expect("failed to resolve upstream");

        // Assert
        assert_eq!(upstream_reference, "origin/main");
    }

    #[test]
    fn parse_branch_tracking_statuses_reads_repo_wide_branch_snapshot() {
        // Arrange
        let output = "\
main\torigin/main\tbehind 2\nagentty/1234abcd\torigin/agentty/1234abcd\tahead 3, behind \
                      1\nfeature/local\t\t\nfeature/gone\torigin/feature/gone\tgone\n";

        // Act
        let branch_tracking_statuses = parse_branch_tracking_statuses(output);

        // Assert
        assert_eq!(branch_tracking_statuses.get("main"), Some(&Some((0, 2))));
        assert_eq!(
            branch_tracking_statuses.get("agentty/1234abcd"),
            Some(&Some((3, 1)))
        );
        assert_eq!(branch_tracking_statuses.get("feature/local"), Some(&None));
        assert_eq!(branch_tracking_statuses.get("feature/gone"), Some(&None));
    }

    #[tokio::test]
    async fn pull_rebase_returns_conflict_detail_for_conflicting_remote_change() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        let contributor_dir = tempdir().expect("failed to create contributor temp dir");
        let contributor_clone_path = contributor_dir.path().join("clone");
        setup_test_git_repo(temp_dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        let contributor_clone_path_text = contributor_clone_path.to_string_lossy().to_string();
        run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
        run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
        fs::write(temp_dir.path().join("README.md"), "local change\n")
            .expect("failed to write local change");
        run_git_command(temp_dir.path(), &["add", "README.md"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Local change"]);
        run_git_command(
            contributor_dir.path(),
            &["clone", &remote_path, &contributor_clone_path_text],
        );
        run_git_command(
            &contributor_clone_path,
            &["config", "user.name", "Contributor User"],
        );
        run_git_command(
            &contributor_clone_path,
            &["config", "user.email", "contributor@example.com"],
        );
        run_git_command(
            &contributor_clone_path,
            &["checkout", "-B", "main", "origin/main"],
        );
        fs::write(contributor_clone_path.join("README.md"), "remote change\n")
            .expect("failed to write remote change");
        run_git_command(&contributor_clone_path, &["add", "README.md"]);
        run_git_command(&contributor_clone_path, &["commit", "-m", "Remote change"]);
        run_git_command(&contributor_clone_path, &["push", "origin", "main"]);

        // Act
        let result = pull_rebase(temp_dir.path().to_path_buf()).await;

        // Assert
        assert!(matches!(
            result,
            Ok(PullRebaseResult::Conflict { ref detail })
                if {
                    let normalized_detail = detail.to_ascii_lowercase();

                    (normalized_detail.contains("conflict")
                        || normalized_detail.contains("could not apply"))
                        && !detail.is_empty()
                }
        ));
    }

    #[tokio::test]
    async fn push_current_branch_returns_rejected_error_for_non_fast_forward_push() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        let contributor_dir = tempdir().expect("failed to create contributor temp dir");
        let contributor_clone_path = contributor_dir.path().join("clone");
        setup_test_git_repo(temp_dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        let contributor_clone_path_text = contributor_clone_path.to_string_lossy().to_string();
        run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
        run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
        run_git_command(
            contributor_dir.path(),
            &["clone", &remote_path, &contributor_clone_path_text],
        );
        run_git_command(
            &contributor_clone_path,
            &["config", "user.name", "Contributor User"],
        );
        run_git_command(
            &contributor_clone_path,
            &["config", "user.email", "contributor@example.com"],
        );
        run_git_command(
            &contributor_clone_path,
            &["checkout", "-B", "main", "origin/main"],
        );
        fs::write(contributor_clone_path.join("remote.txt"), "remote change")
            .expect("failed to write remote file");
        run_git_command(&contributor_clone_path, &["add", "remote.txt"]);
        run_git_command(&contributor_clone_path, &["commit", "-m", "Remote change"]);
        run_git_command(&contributor_clone_path, &["push", "origin", "main"]);
        fs::write(temp_dir.path().join("local.txt"), "local change")
            .expect("failed to write local file");
        run_git_command(temp_dir.path(), &["add", "local.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Local change"]);

        // Act
        let result = push_current_branch(temp_dir.path().to_path_buf()).await;

        // Assert
        let error = result
            .expect_err("non-fast-forward push should fail")
            .to_string();
        assert!(error.contains("git push"));
        assert!(
            error.contains("stale info")
                || error.contains("rejected")
                || error.contains("fetch first")
        );
    }

    #[tokio::test]
    async fn push_current_branch_force_with_lease_updates_rewritten_history() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
        run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
        fs::write(
            temp_dir.path().join("README.md"),
            "first published version\n",
        )
        .expect("failed to write first version");
        run_git_command(temp_dir.path(), &["add", "README.md"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Publish branch change"]);
        push_current_branch(temp_dir.path().to_path_buf())
            .await
            .expect("initial push should succeed");
        fs::write(
            temp_dir.path().join("README.md"),
            "rewritten published version\n",
        )
        .expect("failed to rewrite published version");
        run_git_command(temp_dir.path(), &["add", "README.md"]);
        run_git_command(
            temp_dir.path(),
            &["commit", "--amend", "-m", "Rewrite published branch change"],
        );

        // Act
        let upstream_reference = push_current_branch(temp_dir.path().to_path_buf())
            .await
            .expect("force-with-lease push should update rewritten history");
        let local_head = git_command_stdout(temp_dir.path(), &["rev-parse", "HEAD"]);
        let remote_head = git_command_stdout(remote_dir.path(), &["rev-parse", "refs/heads/main"]);

        // Assert
        assert_eq!(upstream_reference, "origin/main");
        assert_eq!(local_head, remote_head);
    }

    #[tokio::test]
    async fn push_current_branch_to_remote_branch_returns_custom_upstream_reference() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);

        // Act
        let upstream_reference = push_current_branch_to_remote_branch(
            temp_dir.path().to_path_buf(),
            "review/custom-branch".to_string(),
        )
        .await
        .expect("failed to push current branch to custom remote branch");

        // Assert
        assert_eq!(upstream_reference, "origin/review/custom-branch");
    }

    #[tokio::test]
    async fn current_upstream_reference_returns_origin_main() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
        run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);

        // Act
        let upstream_reference = current_upstream_reference(temp_dir.path().to_path_buf())
            .await
            .expect("failed to resolve upstream reference");

        // Assert
        assert_eq!(upstream_reference, "origin/main");
    }

    #[tokio::test]
    async fn branch_tracking_statuses_returns_repo_wide_branch_counts() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        let contributor_dir = tempdir().expect("failed to create contributor temp dir");
        let contributor_clone_path = contributor_dir.path().join("clone");
        setup_test_git_repo(temp_dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        let contributor_clone_path_text = contributor_clone_path.to_string_lossy().to_string();
        run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
        run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
        run_git_command(
            contributor_dir.path(),
            &["clone", &remote_path, &contributor_clone_path_text],
        );
        run_git_command(
            &contributor_clone_path,
            &["config", "user.name", "Contributor User"],
        );
        run_git_command(
            &contributor_clone_path,
            &["config", "user.email", "contributor@example.com"],
        );
        run_git_command(
            &contributor_clone_path,
            &["checkout", "-B", "main", "origin/main"],
        );
        fs::write(contributor_clone_path.join("remote.txt"), "remote change")
            .expect("failed to write remote file");
        run_git_command(&contributor_clone_path, &["add", "remote.txt"]);
        run_git_command(&contributor_clone_path, &["commit", "-m", "Remote change"]);
        run_git_command(&contributor_clone_path, &["push", "origin", "main"]);
        run_git_command(temp_dir.path(), &["checkout", "-b", "agentty/1234abcd"]);
        fs::write(temp_dir.path().join("session.txt"), "session change\n")
            .expect("failed to write session file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Session change"]);
        run_git_command(
            temp_dir.path(),
            &["push", "-u", "origin", "agentty/1234abcd"],
        );
        fs::write(
            temp_dir.path().join("session.txt"),
            "session change\nmore local\n",
        )
        .expect("failed to extend session file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "More session work"]);
        run_git_command(temp_dir.path(), &["fetch"]);

        // Act
        let branch_tracking_statuses = branch_tracking_statuses(temp_dir.path().to_path_buf())
            .await
            .expect("failed to read branch tracking statuses");

        // Assert
        assert_eq!(branch_tracking_statuses.get("main"), Some(&Some((0, 1))));
        assert_eq!(
            branch_tracking_statuses.get("agentty/1234abcd"),
            Some(&Some((1, 0)))
        );
    }
}
