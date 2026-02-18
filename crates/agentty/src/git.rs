use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::gh;

const COMMIT_ALL_HOOK_RETRY_ATTEMPTS: usize = 5;

/// Result of attempting a rebase step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebaseStepResult {
    /// Rebase step completed successfully.
    Completed,
    /// Rebase step stopped because of merge conflicts.
    Conflict { detail: String },
}

/// Detects git repository information for the given directory.
/// Returns the current branch name if in a git repository, None otherwise.
pub fn detect_git_info(dir: &Path) -> Option<String> {
    let repo_dir = find_git_repo(dir)?;
    get_git_branch(&repo_dir)
}

/// Walks up the directory tree to find a .git directory.
/// Returns the directory containing .git (the repository root) if found, None
/// otherwise.
pub fn find_git_repo_root(dir: &Path) -> Option<PathBuf> {
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

/// Legacy alias for `find_git_repo_root`, kept for internal use.
fn find_git_repo(dir: &Path) -> Option<PathBuf> {
    find_git_repo_root(dir)
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
pub fn create_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
    base_branch: &str,
) -> Result<(), String> {
    let output = Command::new("git")
        .args(["worktree", "add", "-b", branch_name])
        .arg(worktree_path)
        .arg(base_branch)
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Git worktree command failed: {}", stderr.trim()));
    }

    Ok(())
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
pub fn remove_worktree(worktree_path: &Path) -> Result<(), String> {
    // Read the .git file in the worktree to find the main repo
    let git_file = worktree_path.join(".git");
    let repo_root = if git_file.is_file() {
        let git_dir = resolve_git_dir(worktree_path)
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
        .arg(worktree_path)
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Git worktree command failed: {}", stderr.trim()));
    }

    Ok(())
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
pub fn squash_merge_diff(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
) -> Result<String, String> {
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
/// Ok(()) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if the repository is on the wrong branch, the merge
/// fails, or the commit fails.
pub fn squash_merge(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
    commit_message: &str,
) -> Result<(), String> {
    // Verify that repo_path is already on the target branch. Switching
    // branches here would disrupt the user's working directory.
    let current_branch = detect_git_info(repo_path)
        .ok_or_else(|| format!("Failed to detect current branch in {}", repo_path.display()))?;
    if current_branch != target_branch {
        return Err(format!(
            "Cannot merge: repository is on '{current_branch}' but expected '{target_branch}'. \
             Switch to '{target_branch}' first."
        ));
    }

    // Perform squash merge
    let output = Command::new("git")
        .args(["merge", "--squash", source_branch])
        .current_dir(repo_path)
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
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if cached_diff.status.success() {
        return Err(
            "Nothing to merge: the session changes are already present in the base branch"
                .to_string(),
        );
    }

    // Commit the squashed changes. Skip pre-commit hooks (`--no-verify`)
    // because the session code was already validated by those same hooks
    // during auto-commit in the session worktree. Re-running them here is
    // redundant and causes failures when hooks modify files in the main repo.
    let output = Command::new("git")
        .args(["commit", "--no-verify", "-m", commit_message])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to commit squash merge: {}", stderr.trim()));
    }

    Ok(())
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
pub fn rebase_start(repo_path: &Path, target_branch: &str) -> Result<RebaseStepResult, String> {
    let output = Command::new("git")
        .args(["rebase", target_branch])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))?;

    if output.status.success() {
        return Ok(RebaseStepResult::Completed);
    }

    let detail = command_output_detail(&output.stdout, &output.stderr);
    if is_rebase_conflict(&detail) {
        return Ok(RebaseStepResult::Conflict { detail });
    }

    Err(format!("Failed to rebase onto {target_branch}: {detail}."))
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
pub fn rebase_continue(repo_path: &Path) -> Result<RebaseStepResult, String> {
    let output = Command::new("git")
        .args(["rebase", "--continue"])
        .env("GIT_EDITOR", ":")
        .env("GIT_SEQUENCE_EDITOR", ":")
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))?;

    if output.status.success() {
        return Ok(RebaseStepResult::Completed);
    }

    let detail = command_output_detail(&output.stdout, &output.stderr);
    if is_rebase_conflict(&detail) {
        return Ok(RebaseStepResult::Conflict { detail });
    }

    Err(format!("Failed to continue rebase: {detail}."))
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
pub fn is_rebase_in_progress(repo_path: &Path) -> Result<bool, String> {
    let git_dir =
        resolve_git_dir(repo_path).ok_or_else(|| "Failed to resolve git directory".to_string())?;
    let rebase_merge = git_dir.join("rebase-merge");
    let rebase_apply = git_dir.join("rebase-apply");

    Ok(rebase_merge.exists() || rebase_apply.exists())
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
pub fn abort_rebase(repo_path: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["rebase", "--abort"])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(format!("Failed to abort rebase: {detail}."));
    }

    Ok(())
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
pub fn list_conflicted_files(repo_path: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(repo_path)
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
pub fn has_unmerged_paths(repo_path: &Path) -> Result<bool, String> {
    let conflicted_files = list_conflicted_files(repo_path)?;

    Ok(!conflicted_files.is_empty())
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
pub fn rebase(repo_path: &Path, target_branch: &str) -> Result<(), String> {
    match rebase_start(repo_path, target_branch)? {
        RebaseStepResult::Completed => Ok(()),
        RebaseStepResult::Conflict { detail } => {
            let abort_suffix = match abort_rebase(repo_path) {
                Ok(()) => String::new(),
                Err(error) => format!(" {error}"),
            };

            Err(format!(
                "Failed to rebase onto {target_branch}: {detail}.{abort_suffix}"
            ))
        }
    }
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
pub fn stage_all(repo_path: &Path) -> Result<(), String> {
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
pub fn commit_all(repo_path: &Path, commit_message: &str, no_verify: bool) -> Result<(), String> {
    // Stage all changes
    stage_all(repo_path)?;

    for _ in 0..COMMIT_ALL_HOOK_RETRY_ATTEMPTS {
        let output = run_commit_command(repo_path, commit_message, no_verify)?;

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
            stage_all(repo_path)?;

            continue;
        }

        return Err(format!("Failed to commit: {}", stderr.trim()));
    }

    Err(format!(
        "Failed to commit: commit hooks kept modifying files after \
         {COMMIT_ALL_HOOK_RETRY_ATTEMPTS} attempts"
    ))
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
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    combined.contains("files were modified by this hook")
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
pub fn head_short_hash(repo_path: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_path)
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
pub fn delete_branch(repo_path: &Path, branch_name: &str) -> Result<(), String> {
    let output = Command::new("git")
        .args(["branch", "-D", branch_name])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Git branch deletion failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Returns the output of `git diff` for the given repository path, showing
/// all changes (committed and uncommitted) relative to the base branch.
///
/// Uses `git add --intent-to-add` to mark untracked files in the index, then
/// `git diff <base_branch>` to compare the working tree against the base
/// branch. This shows all accumulated changes across commits plus any
/// uncommitted work. Finally resets the index to restore the original state.
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
pub fn diff(repo_path: &Path, base_branch: &str) -> Result<String, String> {
    let intent_to_add = Command::new("git")
        .args(["add", "-A", "--intent-to-add"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !intent_to_add.status.success() {
        let stderr = String::from_utf8_lossy(&intent_to_add.stderr);

        return Err(format!("Git add --intent-to-add failed: {}", stderr.trim()));
    }

    let diff_output = Command::new("git")
        .args(["diff", base_branch])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    let reset = Command::new("git")
        .arg("reset")
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !reset.status.success() {
        let stderr = String::from_utf8_lossy(&reset.stderr);

        return Err(format!("Git reset failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&diff_output.stdout).into_owned())
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
pub fn fetch_remote(repo_path: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .arg("fetch")
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Git fetch failed: {}", stderr.trim()));
    }

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
pub fn get_ahead_behind(repo_path: &Path) -> Result<(u32, u32), String> {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{u}"])
        .current_dir(repo_path)
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
}

/// Returns the number of commits in `HEAD` that are not in `base_branch`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `base_branch` - Branch to compare against (for example, `main`)
///
/// # Returns
/// The count of commits reachable from `HEAD` and not from `base_branch`.
///
/// # Errors
/// Returns an error if `git rev-list` fails or output cannot be parsed.
pub fn count_commits_since_base(repo_path: &Path, base_branch: &str) -> Result<i64, String> {
    let revision_range = format!("{base_branch}..HEAD");
    let output = Command::new("git")
        .args(["rev-list", "--count", revision_range.as_str()])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        return Err(format!("Git rev-list failed: {}", stderr.trim()));
    }

    let count = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<i64>()
        .map_err(|error| format!("Failed to parse commit count: {error}"))?;

    Ok(count)
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
pub fn repo_url(repo_path: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git remote get-url: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Git remote get-url failed: {}", stderr.trim()));
    }

    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(normalize_repo_url(&remote))
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

/// Returns whether git output detail indicates a rebase conflict state.
fn is_rebase_conflict(detail: &str) -> bool {
    detail.contains("CONFLICT")
        || detail.contains("Resolve all conflicts manually")
        || detail.contains("could not apply")
        || detail.contains("mark them as resolved")
}

/// Pushes the source branch to origin and creates a Pull Request via GitHub
/// CLI.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
/// * `source_branch` - Name of the branch to push and create PR from
/// * `target_branch` - Name of the base branch for the PR
/// * `title` - Title for the Pull Request
///
/// # Returns
/// Ok(url) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if branch push or PR creation fails.
pub fn create_pr(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
    title: &str,
) -> Result<String, String> {
    // 1. Push source branch to origin
    let output = Command::new("git")
        .args(["push", "-u", "origin", source_branch])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git push: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Git push failed: {}", stderr.trim()));
    }

    gh::create_pull_request(repo_path, source_branch, target_branch, title)
}

/// Returns whether the PR for `source_branch` has been merged.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
/// * `source_branch` - Branch used to create the PR (e.g., `agentty/abc123`)
///
/// # Returns
/// Ok(true) when merged, Ok(false) when still open, Err(msg) on failure
///
/// # Errors
/// Returns an error if `gh pr view` fails or returns an unexpected value.
pub fn is_pr_merged(repo_path: &Path, source_branch: &str) -> Result<bool, String> {
    gh::is_pull_request_merged(repo_path, source_branch)
}

/// Returns whether the PR for `source_branch` has been closed without merge.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
/// * `source_branch` - Branch used to create the PR (e.g., `agentty/abc123`)
///
/// # Returns
/// Ok(true) when closed, Ok(false) when still open or merged, Err(msg) on
/// failure.
///
/// # Errors
/// Returns an error if `gh pr view` fails or returns an unexpected value.
pub fn is_pr_closed(repo_path: &Path, source_branch: &str) -> Result<bool, String> {
    gh::is_pull_request_closed(repo_path, source_branch)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_find_git_repo_exists() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");

        // Act
        let result = find_git_repo(dir.path());

        // Assert
        assert_eq!(result, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn test_find_git_repo_not_exists() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let result = find_git_repo(dir.path());

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_git_repo_parent() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).expect("test setup failed");

        // Act
        let result = find_git_repo(&subdir);

        // Assert
        assert_eq!(result, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn test_get_git_branch_normal() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "ref: refs/heads/main\n").expect("test setup failed");

        // Act
        let result = get_git_branch(dir.path());

        // Assert
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn test_get_git_branch_detached() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0\n")
            .expect("test setup failed");

        // Act
        let result = get_git_branch(dir.path());

        // Assert
        assert_eq!(result, Some("HEAD@a1b2c3d".to_string()));
    }

    #[test]
    fn test_get_git_branch_invalid() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "invalid content\n").expect("test setup failed");

        // Act
        let result = get_git_branch(dir.path());

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_git_branch_worktree_git_file() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let worktree_path = dir.path().join("worktree");
        fs::create_dir(&worktree_path).expect("test setup failed");

        let git_dir = dir
            .path()
            .join("main")
            .join(".git")
            .join("worktrees")
            .join("worktree");
        fs::create_dir_all(&git_dir).expect("test setup failed");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/worktree\n")
            .expect("test setup failed");
        fs::write(
            worktree_path.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .expect("test setup failed");

        // Act
        let result = get_git_branch(&worktree_path);

        // Assert
        assert_eq!(result, Some("feature/worktree".to_string()));
    }

    #[test]
    fn test_detect_git_info_full_flow() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "ref: refs/heads/feature-branch\n").expect("test setup failed");

        // Act
        let result = detect_git_info(dir.path());

        // Assert
        assert_eq!(result, Some("feature-branch".to_string()));
    }

    #[test]
    fn test_detect_git_info_worktree_git_file() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let worktree_path = dir.path().join("worktree");
        fs::create_dir(&worktree_path).expect("test setup failed");

        let git_dir = dir
            .path()
            .join("main")
            .join(".git")
            .join("worktrees")
            .join("worktree");
        fs::create_dir_all(&git_dir).expect("test setup failed");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").expect("test setup failed");
        fs::write(
            worktree_path.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .expect("test setup failed");

        // Act
        let result = detect_git_info(&worktree_path);

        // Assert
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn test_detect_git_info_no_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let result = detect_git_info(dir.path());

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_git_repo_root() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");

        // Act
        let result = find_git_repo_root(dir.path());

        // Assert
        assert_eq!(result, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn test_find_git_repo_root_with_git_file() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        fs::write(
            dir.path().join(".git"),
            "gitdir: /tmp/main/.git/worktrees/test\n",
        )
        .expect("test setup failed");

        // Act
        let result = find_git_repo_root(dir.path());

        // Assert
        assert_eq!(result, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn test_create_worktree_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        let worktree_path = dir.path().join("worktree");
        let branch_name = "agentty/test123";
        let base_branch = "main";

        // Act
        let result = create_worktree(dir.path(), &worktree_path, branch_name, base_branch);

        // Assert
        assert!(result.is_ok());
        assert!(worktree_path.exists());
        assert!(worktree_path.join(".git").exists());
    }

    #[test]
    fn test_create_worktree_invalid_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let worktree_path = dir.path().join("worktree");
        let branch_name = "agentty/test123";
        let base_branch = "main";

        // Act
        let result = create_worktree(dir.path(), &worktree_path, branch_name, base_branch);

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("not a git repository")
        );
    }

    #[test]
    fn test_create_worktree_branch_exists() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        let worktree_path = dir.path().join("worktree");
        let branch_name = "main"; // Branch already exists
        let base_branch = "main";

        // Act
        let result = create_worktree(dir.path(), &worktree_path, branch_name, base_branch);

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("already exists")
        );
    }

    #[test]
    fn test_remove_worktree_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        let worktree_path = dir.path().join("worktree");
        let branch_name = "agentty/test123";
        let base_branch = "main";
        create_worktree(dir.path(), &worktree_path, branch_name, base_branch)
            .expect("test setup failed");

        // Act
        let result = remove_worktree(&worktree_path);

        // Assert
        assert!(result.is_ok());
        assert!(!worktree_path.exists());
    }

    #[test]
    fn test_delete_branch_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        let branch_name = "test-branch";
        Command::new("git")
            .args(["branch", branch_name])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let result = delete_branch(dir.path(), branch_name);

        // Assert
        assert!(result.is_ok());
        let output = Command::new("git")
            .args(["branch"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.contains(branch_name));
    }

    #[test]
    fn test_remove_worktree_not_exists() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let worktree_path = dir.path().join("nonexistent");

        // Act
        let result = remove_worktree(&worktree_path);

        // Assert
        // Should fail because worktree doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_squash_merge_diff_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        fs::write(dir.path().join("file-one.txt"), "one").expect("test setup failed");
        Command::new("git")
            .args(["add", "file-one.txt"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: add file one"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        fs::write(dir.path().join("file-two.txt"), "two").expect("test setup failed");
        Command::new("git")
            .args(["add", "file-two.txt"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "fix: add file two"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let result = squash_merge_diff(dir.path(), "feature-branch", "main");

        // Assert
        assert!(result.is_ok());
        let diff = result.expect("should succeed");
        assert!(diff.contains("diff --git a/file-one.txt b/file-one.txt"));
        assert!(diff.contains("diff --git a/file-two.txt b/file-two.txt"));
    }

    #[test]
    fn test_squash_merge_diff_invalid_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let result = squash_merge_diff(dir.path(), "missing-branch", "main");

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Failed to read squash merge diff")
        );
    }

    #[test]
    fn test_squash_merge_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Create a feature branch and add a commit, then return to main.
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("feature.txt"), "feature content").expect("test setup failed");
        Command::new("git")
            .args(["add", "feature.txt"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "Add feature"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act — squash_merge requires the repo to already be on the target branch.
        let result = squash_merge(dir.path(), "feature-branch", "main", "Squash merge feature");

        // Assert
        assert!(result.is_ok());

        // Verify the file was squash-merged onto main.
        assert!(dir.path().join("feature.txt").exists());
    }

    #[test]
    fn test_squash_merge_no_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Create a branch without any new commits
        Command::new("git")
            .args(["branch", "empty-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let result = squash_merge(dir.path(), "empty-branch", "main", "Empty merge");

        // Assert - git merge --squash stages nothing (branch is already at same
        // commit as main), so we detect it via `git diff --cached --quiet` and
        // return "Nothing to merge" before even attempting git commit.
        assert!(result.is_err());
        let error = result.expect_err("should be error");
        assert!(
            error.contains("Nothing to merge"),
            "Expected 'Nothing to merge', got: {error}"
        );
    }

    #[test]
    fn test_squash_merge_changes_already_in_base() {
        // Arrange — session branch has a commit, but main already has the same
        // changes (e.g., user manually applied the same patch to main).
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Create the session branch from main and add a commit.
        Command::new("git")
            .args(["checkout", "-b", "session-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        std::fs::write(dir.path().join("session.txt"), "session change").expect("write failed");
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "session change"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Switch back to main and apply the same change independently.
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        std::fs::write(dir.path().join("session.txt"), "session change").expect("write failed");
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "same change applied to main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act — trying to merge session-branch into main should detect that
        // there is nothing new to stage after the squash merge.
        let result = squash_merge(dir.path(), "session-branch", "main", "Merge session");

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("should be error");
        assert!(
            error.contains("Nothing to merge"),
            "Expected 'Nothing to merge', got: {error}"
        );
    }

    #[test]
    fn test_squash_merge_invalid_source_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let result = squash_merge(dir.path(), "nonexistent-branch", "main", "Test merge");

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Failed to squash merge")
        );
    }

    #[test]
    fn test_squash_merge_wrong_branch() {
        // Arrange — repo is on main but target_branch says something else.
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let result = squash_merge(dir.path(), "main", "nonexistent-branch", "Test merge");

        // Assert — should fail immediately because the current branch does not
        // match target_branch; no git merge is attempted.
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Cannot merge"),
            "Expected 'Cannot merge' error for wrong branch"
        );
    }

    #[test]
    fn test_rebase_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("feature.txt"), "feature content").expect("test setup failed");
        Command::new("git")
            .args(["add", "feature.txt"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: feature branch update"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "main branch update").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: main branch update"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let result = rebase(dir.path(), "main");

        // Assert
        assert!(result.is_ok(), "rebase should succeed: {:?}", result.err());
        let current_branch_output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        let current_branch = String::from_utf8_lossy(&current_branch_output.stdout)
            .trim()
            .to_string();
        assert_eq!(current_branch, "feature-branch");

        let status_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        let status = String::from_utf8_lossy(&status_output.stdout);
        assert!(status.trim().is_empty(), "working tree should be clean");
    }

    #[test]
    fn test_rebase_conflict_aborts_in_progress_state() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "feature branch update")
            .expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: feature branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "main branch update").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: main branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let result = rebase(dir.path(), "main");

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("should be error");
        assert!(
            error.contains("Failed to rebase onto main"),
            "unexpected error: {error}"
        );

        let rebase_head_output = Command::new("git")
            .args(["rev-parse", "--verify", "REBASE_HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        assert!(
            !rebase_head_output.status.success(),
            "rebase should be aborted and REBASE_HEAD should not exist"
        );
    }

    #[test]
    fn test_rebase_start_conflict_keeps_rebase_in_progress() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "feature branch update")
            .expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: feature branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "main branch update").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: main branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let result = rebase_start(dir.path(), "main");

        // Assert
        assert!(matches!(result, Ok(RebaseStepResult::Conflict { .. })));
        let rebase_head_output = Command::new("git")
            .args(["rev-parse", "--verify", "REBASE_HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        assert!(
            rebase_head_output.status.success(),
            "rebase should remain in progress"
        );

        let abort_result = abort_rebase(dir.path());
        assert!(abort_result.is_ok(), "failed to abort test rebase");
    }

    #[test]
    fn test_list_conflicted_files_and_unmerged_paths_on_rebase_conflict() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "feature branch update")
            .expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: feature branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "main branch update").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: main branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        let rebase_result = rebase_start(dir.path(), "main");
        assert!(matches!(
            rebase_result,
            Ok(RebaseStepResult::Conflict { .. })
        ));

        // Act
        let conflicted_files = list_conflicted_files(dir.path()).expect("should list conflicts");
        let has_unmerged = has_unmerged_paths(dir.path()).expect("should query unmerged paths");

        // Assert
        assert_eq!(conflicted_files, vec!["README.md".to_string()]);
        assert!(has_unmerged);

        let abort_result = abort_rebase(dir.path());
        assert!(abort_result.is_ok(), "failed to abort test rebase");
    }

    #[test]
    fn test_rebase_continue_conflict_when_unmerged_paths_remain() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "feature branch update")
            .expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: feature branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "main branch update").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: main branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        let rebase_result = rebase_start(dir.path(), "main");
        assert!(matches!(
            rebase_result,
            Ok(RebaseStepResult::Conflict { .. })
        ));

        // Act
        let continue_result = rebase_continue(dir.path());

        // Assert
        assert!(matches!(
            continue_result,
            Ok(RebaseStepResult::Conflict { .. })
        ));

        let abort_result = abort_rebase(dir.path());
        assert!(abort_result.is_ok(), "failed to abort test rebase");
    }

    #[test]
    fn test_rebase_continue_succeeds_with_non_interactive_editor() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "feature branch update")
            .expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: feature branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "main branch update").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: main branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        let rebase_result = rebase_start(dir.path(), "main");
        assert!(matches!(
            rebase_result,
            Ok(RebaseStepResult::Conflict { .. })
        ));
        fs::write(dir.path().join("README.md"), "resolved readme").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["config", "core.editor", "editor"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let continue_result = rebase_continue(dir.path());

        // Assert
        assert!(matches!(continue_result, Ok(RebaseStepResult::Completed)));
        let in_progress = is_rebase_in_progress(dir.path()).expect("should query rebase state");
        assert!(!in_progress, "rebase should be completed");
    }

    #[test]
    fn test_is_rebase_in_progress_false_when_no_rebase_exists() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let is_rebase_in_progress_result = is_rebase_in_progress(dir.path());

        // Assert
        assert_eq!(is_rebase_in_progress_result, Ok(false));
    }

    #[test]
    fn test_is_rebase_in_progress_true_during_conflict() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "feature branch update")
            .expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: feature branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("README.md"), "main branch update").expect("test setup failed");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: main branch readme"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["checkout", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        let rebase_result = rebase_start(dir.path(), "main");
        assert!(matches!(
            rebase_result,
            Ok(RebaseStepResult::Conflict { .. })
        ));

        // Act
        let is_rebase_in_progress_result = is_rebase_in_progress(dir.path());

        // Assert
        assert_eq!(is_rebase_in_progress_result, Ok(true));
        let abort_result = abort_rebase(dir.path());
        assert!(abort_result.is_ok(), "failed to abort test rebase");
    }

    #[test]
    fn test_stage_all_stages_changed_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        fs::write(dir.path().join("new_file.txt"), "new content").expect("test setup failed");

        // Act
        let stage_result = stage_all(dir.path());

        // Assert
        assert!(stage_result.is_ok());
        let output = Command::new("git")
            .args(["status", "--short"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        let status = String::from_utf8_lossy(&output.stdout);
        assert!(status.contains("A  new_file.txt"));
    }

    #[test]
    fn test_commit_all_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Create a new file to commit
        fs::write(dir.path().join("new_file.txt"), "new content").expect("test setup failed");

        // Act
        let result = commit_all(dir.path(), "Test commit message", false);

        // Assert
        assert!(result.is_ok());

        // Verify the commit was made
        let output = Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(log.contains("Test commit message"));
    }

    #[test]
    fn test_commit_all_no_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act - no changes to commit
        let result = commit_all(dir.path(), "Empty commit", false);

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("should be error");
        assert!(
            error.contains("Nothing to commit"),
            "Expected 'Nothing to commit', got: {error}"
        );
    }

    #[test]
    fn test_commit_all_retries_when_hook_modifies_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        fs::write(dir.path().join("tracked.txt"), "needs-fix\n").expect("test setup failed");
        write_pre_commit_hook(
            dir.path(),
            r#"#!/bin/sh
if grep -q "needs-fix" tracked.txt; then
  printf "fixed\n" > tracked.txt
  echo "hook id: clippy-fix" >&2
  echo "files were modified by this hook" >&2
  exit 1
fi
exit 0
"#,
        );

        // Act
        let result = commit_all(dir.path(), "Retry commit message", false);

        // Assert
        assert!(
            result.is_ok(),
            "expected commit to succeed, got: {result:?}"
        );

        let output = Command::new("git")
            .args(["show", "HEAD:tracked.txt"])
            .current_dir(dir.path())
            .output()
            .expect("failed to read committed file");
        let content = String::from_utf8_lossy(&output.stdout);
        assert_eq!(content, "fixed\n");
    }

    #[test]
    fn test_commit_all_returns_error_when_hook_keeps_modifying_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        fs::write(dir.path().join("tracked.txt"), "start\n").expect("test setup failed");
        write_pre_commit_hook(
            dir.path(),
            r#"#!/bin/sh
printf "loop\n" >> tracked.txt
echo "hook id: clippy-fix" >&2
echo "files were modified by this hook" >&2
exit 1
"#,
        );

        // Act
        let result = commit_all(dir.path(), "Looping hook commit", false);

        // Assert
        assert!(result.is_err(), "expected commit to fail");
        let error = result.expect_err("expected error");
        assert!(
            error.contains("kept modifying files"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_head_short_hash_returns_current_commit_hash() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let result = head_short_hash(dir.path());

        // Assert
        let hash = result.expect("should succeed");
        assert!(!hash.is_empty());
    }

    #[test]
    fn test_count_commits_since_base_returns_zero_when_no_commits_ahead() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let result = count_commits_since_base(dir.path(), "main");

        // Assert
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_count_commits_since_base_returns_commits_ahead_of_base() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        Command::new("git")
            .args(["checkout", "-b", "feature-branch"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        fs::write(dir.path().join("feature.txt"), "feature").expect("test setup failed");
        Command::new("git")
            .args(["add", "feature.txt"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");
        Command::new("git")
            .args(["commit", "-m", "feat: add feature"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act
        let result = count_commits_since_base(dir.path(), "main");

        // Assert
        assert_eq!(result, Ok(1));
    }

    #[test]
    fn test_diff_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        fs::write(dir.path().join("README.md"), "modified").expect("test setup failed");

        // Act
        let result = diff(dir.path(), "HEAD");

        // Assert
        assert!(result.is_ok());
        let output = result.expect("should succeed");
        assert!(
            output.contains("diff --git"),
            "Expected diff output, got: {output}"
        );
    }

    #[test]
    fn test_diff_no_changes() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let result = diff(dir.path(), "HEAD");

        // Assert
        assert!(result.is_ok());
        assert!(result.expect("should succeed").is_empty());
    }

    #[test]
    fn test_diff_includes_untracked_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        fs::write(dir.path().join("new_file.txt"), "hello world").expect("test setup failed");

        // Act
        let result = diff(dir.path(), "HEAD");

        // Assert
        let output = result.expect("should succeed");
        assert!(
            output.contains("new_file.txt"),
            "Expected untracked file in diff, got: {output}"
        );
        assert!(
            output.contains("hello world"),
            "Expected file content in diff, got: {output}"
        );

        // Verify file remains untracked after diff
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(dir.path())
            .output()
            .expect("git status failed");
        let status_output = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_output.contains("?? new_file.txt"),
            "Expected file to remain untracked, got: {status_output}"
        );
    }

    #[test]
    fn test_diff_includes_deleted_files() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");
        fs::remove_file(dir.path().join("README.md")).expect("test setup failed");

        // Act
        let result = diff(dir.path(), "HEAD");

        // Assert
        let output = result.expect("should succeed");
        assert!(
            output.contains("deleted file"),
            "Expected deleted file in diff, got: {output}"
        );
        assert!(
            output.contains("README.md"),
            "Expected deleted filename in diff, got: {output}"
        );
    }

    #[test]
    fn test_create_pr_push_fails_no_remote() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Create a feature branch
        Command::new("git")
            .args(["checkout", "-b", "agentty/test123"])
            .current_dir(dir.path())
            .output()
            .expect("test setup failed");

        // Act — push should fail because there is no remote
        let result = create_pr(dir.path(), "agentty/test123", "main", "Test PR");

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("should be error");
        assert!(
            error.contains("Git push failed"),
            "Expected 'Git push failed', got: {error}"
        );
    }

    #[test]
    fn test_create_pr_invalid_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act — no git repo at all
        let result = create_pr(dir.path(), "some-branch", "main", "Test PR");

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_repo_url_invalid_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let result = repo_url(dir.path());

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_is_pr_merged_invalid_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let result = is_pr_merged(dir.path(), "agentty/test123");

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_is_pr_closed_invalid_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");

        // Act
        let result = is_pr_closed(dir.path(), "agentty/test123");

        // Assert
        assert!(result.is_err());
    }

    /// Helper function to set up a test git repository with an initial commit
    fn setup_test_git_repo(path: &Path) -> std::io::Result<()> {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()?;
        fs::write(path.join("README.md"), "test")?;
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()?;
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(path)
            .output()?;
        Ok(())
    }

    fn write_pre_commit_hook(repo_path: &Path, script: &str) {
        let hook_path = repo_path.join(".git/hooks/pre-commit");
        fs::write(&hook_path, script).expect("failed to write pre-commit hook");
        let status = Command::new("chmod")
            .args(["+x", hook_path.to_string_lossy().as_ref()])
            .status()
            .expect("failed to run chmod for pre-commit hook");
        assert!(
            status.success(),
            "failed to make pre-commit hook executable"
        );
    }
}
