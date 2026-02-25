use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::task::spawn_blocking;

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
            .map_err(|error| format!("Failed to execute git remote get-url: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Git remote get-url failed: {}", stderr.trim()));
        }

        let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Ok(normalize_repo_url(&remote))
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Resolves the main repository root for a repository or linked worktree.
///
/// Uses `git rev-parse --git-dir --git-common-dir`, normalizes both paths to
/// absolute form, detects whether `repo_path` is a linked worktree (`git-dir`
/// differs from `git-common-dir`), and then returns the shared repository
/// root.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(path) containing the main repository root, Err(msg) on failure.
///
/// # Errors
/// Returns an error if git metadata cannot be queried from `repo_path`.
pub async fn main_repo_root(repo_path: PathBuf) -> Result<PathBuf, String> {
    spawn_blocking(move || main_repo_root_sync(&repo_path))
        .await
        .map_err(|error| format!("Join error: {error}"))?
}

/// Resolves the main repository root for `repo_path` in synchronous code.
pub(super) fn main_repo_root_sync(repo_path: &Path) -> Result<PathBuf, String> {
    let (git_dir, git_common_dir) = git_directory_paths(repo_path)?;

    if git_dir == git_common_dir {
        return repo_root_from_git_dir(repo_path, &git_dir);
    }

    repo_root_from_git_dir(repo_path, &git_common_dir)
}

/// Resolves the git directory path for a repository root or worktree root.
pub(super) fn resolve_git_dir(repo_dir: &Path) -> Option<PathBuf> {
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

/// Extracts the best human-readable error detail from command output.
pub(super) fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
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

/// Converts SSH-style GitHub remotes into HTTPS while preserving other URLs.
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

/// Reads absolute git and common git directory paths for `repo_path`.
fn git_directory_paths(repo_path: &Path) -> Result<(PathBuf, PathBuf), String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir", "--git-common-dir"])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git rev-parse: {error}"))?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(format!("Git rev-parse failed: {detail}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let git_dir = lines
        .next()
        .ok_or_else(|| "Git rev-parse output missing git-dir".to_string())?;
    let git_common_dir = lines
        .next()
        .ok_or_else(|| "Git rev-parse output missing git-common-dir".to_string())?;

    Ok((
        normalize_git_dir_path(repo_path, git_dir),
        normalize_git_dir_path(repo_path, git_common_dir),
    ))
}

/// Converts a git directory path (typically `.git`) into repository root.
fn repo_root_from_git_dir(repo_path: &Path, git_dir: &Path) -> Result<PathBuf, String> {
    if git_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".git")
    {
        return git_dir
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("Git directory has no parent: {}", git_dir.display()));
    }

    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute git rev-parse: {error}"))?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(format!("Git rev-parse --show-toplevel failed: {detail}"));
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Err("Git rev-parse --show-toplevel returned empty output".to_string());
    }

    Ok(PathBuf::from(root))
}

/// Normalizes a git metadata path into absolute form for path comparisons.
fn normalize_git_dir_path(repo_path: &Path, git_path: &str) -> PathBuf {
    let git_path = PathBuf::from(git_path);
    let git_path = if git_path.is_absolute() {
        git_path
    } else {
        repo_path.join(git_path)
    };

    std::fs::canonicalize(&git_path).unwrap_or(git_path)
}
