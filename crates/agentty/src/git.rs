use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let head_path = repo_dir.join(".git").join("HEAD");
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
pub fn remove_worktree(worktree_path: &Path) -> Result<(), String> {
    // Read the .git file in the worktree to find the main repo
    let git_file = worktree_path.join(".git");
    let repo_root = if git_file.is_file() {
        // Worktree: .git file contains "gitdir: /path/to/main/.git/worktrees/name"
        let content =
            fs::read_to_string(&git_file).map_err(|e| format!("Failed to read .git file: {e}"))?;

        if let Some(gitdir_line) = content.lines().find(|l| l.starts_with("gitdir:")) {
            let gitdir = gitdir_line.trim_start_matches("gitdir:").trim();
            // Extract main repo path: /path/to/main/.git/worktrees/name -> /path/to/main
            PathBuf::from(gitdir)
                .parent() // Remove "worktrees/name"
                .and_then(|p| p.parent()) // Remove ".git"
                .ok_or_else(|| "Invalid gitdir path in .git file".to_string())?
                .to_path_buf()
        } else {
            return Err("Invalid .git file format".to_string());
        }
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

/// Performs a squash merge from a source branch to a target branch.
///
/// This function:
/// 1. Checks out the target branch
/// 2. Performs `git merge --squash` from the source branch
/// 3. Commits the squashed changes
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
/// * `source_branch` - Name of the branch to merge from (e.g.,
///   `agentty/abc123`)
/// * `target_branch` - Name of the branch to merge into (e.g., `main`)
/// * `commit_message` - Message for the squash commit
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
pub fn squash_merge(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
    commit_message: &str,
) -> Result<(), String> {
    // Checkout target branch
    let output = Command::new("git")
        .args(["checkout", target_branch])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Failed to checkout {target_branch}: {}",
            stderr.trim()
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

    // Commit the squashed changes
    let output = Command::new("git")
        .args(["commit", "-m", commit_message])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Check if there's nothing to commit (no changes) - message appears on stdout
        if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
            return Err("Nothing to merge: no changes detected".to_string());
        }
        return Err(format!("Failed to commit squash merge: {}", stderr.trim()));
    }

    Ok(())
}

/// Stages all changes and commits them with the given message.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `commit_message` - Message for the commit
///
/// # Returns
/// Ok(()) on success, Err(msg) with detailed error message on failure
pub fn commit_all(repo_path: &Path, commit_message: &str) -> Result<(), String> {
    // Stage all changes
    let output = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to stage changes: {}", stderr.trim()));
    }

    // Commit (skip pre-commit hooks)
    let output = Command::new("git")
        .args(["commit", "--no-verify", "-m", commit_message])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Check if there's nothing to commit
        if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
            return Err("Nothing to commit: no changes detected".to_string());
        }
        return Err(format!("Failed to commit: {}", stderr.trim()));
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use std::fs;

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
    fn test_squash_merge_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Create a feature branch and add a commit
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

        // Act
        let result = squash_merge(dir.path(), "feature-branch", "main", "Squash merge feature");

        // Assert
        assert!(result.is_ok());

        // Verify we're on main branch
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(dir.path())
            .output()
            .expect("test execution failed");
        let current_branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(current_branch, "main");

        // Verify the file exists on main
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

        // Assert - git merge --squash succeeds with "nothing to squash", then commit
        // fails with "nothing to commit", which we report as "Nothing to merge:
        // no changes detected"
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
    fn test_squash_merge_invalid_target_branch() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Act
        let result = squash_merge(dir.path(), "main", "nonexistent-branch", "Test merge");

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .expect_err("should be error")
                .contains("Failed to checkout")
        );
    }

    #[test]
    fn test_commit_all_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path()).expect("test setup failed");

        // Create a new file to commit
        fs::write(dir.path().join("new_file.txt"), "new content").expect("test setup failed");

        // Act
        let result = commit_all(dir.path(), "Test commit message");

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
        let result = commit_all(dir.path(), "Empty commit");

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("should be error");
        assert!(
            error.contains("Nothing to commit"),
            "Expected 'Nothing to commit', got: {error}"
        );
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
}
