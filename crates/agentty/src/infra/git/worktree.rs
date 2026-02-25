use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::task::spawn_blocking;

use super::repo::{main_repo_root_sync, resolve_git_dir};

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
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Git worktree command failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Removes a git worktree at the specified path.
///
/// Uses --force to remove even with uncommitted changes.
/// Finds the main repository by comparing git-dir and git-common-dir.
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
        let repo_root = main_repo_root_sync(&worktree_path)?;

        let output = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&worktree_path)
            .current_dir(repo_root)
            .output()
            .map_err(|error| format!("Failed to execute git: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Git worktree command failed: {}", stderr.trim()));
        }

        Ok(())
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Returns branch information for a repository directory in synchronous code.
pub(super) fn detect_git_info_sync(dir: &Path) -> Option<String> {
    let repo_dir = find_git_repo(dir)?;

    get_git_branch(&repo_dir)
}

/// Legacy alias for `find_git_repo_root`, kept for internal use.
fn find_git_repo(dir: &Path) -> Option<PathBuf> {
    find_git_repo_root_sync(dir)
}

/// Returns the repository root by searching upward for `.git`.
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

/// Reads `.git/HEAD` and extracts the current branch identifier.
fn get_git_branch(repo_dir: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(repo_dir)?;
    let head_path = git_dir.join("HEAD");
    let content = fs::read_to_string(head_path).ok()?;
    let content = content.trim();

    if let Some(branch_ref) = content.strip_prefix("ref: refs/heads/") {
        return Some(branch_ref.to_string());
    }

    if content.len() >= 7
        && content
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Some(format!("HEAD@{}", &content[..7]));
    }

    None
}
