use std::fs;
use std::path::{Path, PathBuf};

use tokio::task::spawn_blocking;

use super::error::GitError;
use super::repo::{main_repo_root, resolve_git_dir, run_git_command};

/// Detects git repository information for the given directory.
/// Returns the current branch name if in a git repository, None otherwise.
pub(crate) async fn detect_git_info(dir: PathBuf) -> Option<String> {
    spawn_blocking(move || detect_git_info_sync(&dir))
        .await
        .ok()
        .flatten()
}

/// Walks up the directory tree to find a .git directory.
/// Returns the directory containing .git (the repository root) if found, None
/// otherwise.
pub(crate) async fn find_git_repo_root(dir: PathBuf) -> Option<PathBuf> {
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
/// * `start_ref` - Git ref used as the new worktree branch starting point
///
/// # Returns
/// Ok(()) on success, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns [`GitError::CommandFailed`] if spawning fails or the worktree
/// command exits with a non-zero status.
pub(crate) async fn create_worktree(
    repo_path: PathBuf,
    worktree_path: PathBuf,
    branch_name: String,
    start_ref: String,
) -> Result<(), GitError> {
    run_git_command(
        repo_path,
        vec![
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch_name,
            worktree_path.to_string_lossy().into_owned(),
            start_ref,
        ],
        "Git worktree command failed".into(),
    )
    .await?;

    Ok(())
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
/// Ok(()) on success, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns [`GitError::CommandTimedOut`] when repository inspection or
/// worktree removal exceeds its runtime bound, or [`GitError::CommandFailed`]
/// if spawning fails or the command exits with a non-zero status.
pub(crate) async fn remove_worktree(worktree_path: PathBuf) -> Result<(), GitError> {
    let repo_root = main_repo_root(worktree_path.clone()).await?;
    let worktree_path = worktree_path.to_string_lossy().to_string();
    run_git_command(
        repo_root,
        vec![
            "worktree".to_string(),
            "remove".to_string(),
            "--force".to_string(),
            worktree_path,
        ],
        "Git worktree command failed".to_string(),
    )
    .await?;

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_git_repo_root_sync_finds_repo_at_current_dir() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        fs::create_dir(temp_dir.path().join(".git")).expect("should create .git");

        // Act
        let result = find_git_repo_root_sync(temp_dir.path());

        // Assert
        assert_eq!(result, Some(temp_dir.path().to_path_buf()));
    }

    #[test]
    fn find_git_repo_root_sync_walks_up_to_parent() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        fs::create_dir(temp_dir.path().join(".git")).expect("should create .git");
        let nested = temp_dir.path().join("src").join("lib");
        fs::create_dir_all(&nested).expect("should create nested dirs");

        // Act
        let result = find_git_repo_root_sync(&nested);

        // Assert
        assert_eq!(result, Some(temp_dir.path().to_path_buf()));
    }

    #[test]
    fn find_git_repo_root_sync_returns_none_when_no_git_dir() {
        // Arrange
        // TMPDIR may itself be inside a checkout. The filesystem root has no
        // parent checkout and exercises the search exhaustion boundary.
        let root = Path::new(std::path::MAIN_SEPARATOR_STR);

        // Act
        let result = find_git_repo_root_sync(root);

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn get_git_branch_returns_branch_from_ref() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        let git_dir = temp_dir.path().join(".git");
        fs::create_dir(&git_dir).expect("should create .git");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").expect("should write HEAD");

        // Act
        let result = get_git_branch(temp_dir.path());

        // Assert
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn get_git_branch_returns_detached_head_for_commit_hash() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        let git_dir = temp_dir.path().join(".git");
        fs::create_dir(&git_dir).expect("should create .git");
        fs::write(git_dir.join("HEAD"), "abc1234def5678\n").expect("should write HEAD");

        // Act
        let result = get_git_branch(temp_dir.path());

        // Assert
        assert_eq!(result, Some("HEAD@abc1234".to_string()));
    }

    #[test]
    fn get_git_branch_returns_none_for_unrecognized_content() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        let git_dir = temp_dir.path().join(".git");
        fs::create_dir(&git_dir).expect("should create .git");
        fs::write(git_dir.join("HEAD"), "unknown\n").expect("should write HEAD");

        // Act
        let result = get_git_branch(temp_dir.path());

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn get_git_branch_returns_none_when_no_git_dir() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");

        // Act
        let result = get_git_branch(temp_dir.path());

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn detect_git_info_sync_returns_branch_for_repo() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        let git_dir = temp_dir.path().join(".git");
        fs::create_dir(&git_dir).expect("should create .git");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature\n").expect("should write HEAD");

        // Act
        let result = detect_git_info_sync(temp_dir.path());

        // Assert
        assert_eq!(result, Some("feature".to_string()));
    }

    #[test]
    fn detect_git_info_sync_returns_none_for_non_repo() {
        // Arrange
        let root = Path::new(std::path::MAIN_SEPARATOR_STR);

        // Act
        let result = detect_git_info_sync(root);

        // Assert
        assert!(result.is_none());
    }
}
