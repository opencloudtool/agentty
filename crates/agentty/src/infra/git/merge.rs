use std::path::PathBuf;

use tokio::task::spawn_blocking;

use super::error::GitError;
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
) -> Result<String, GitError> {
    spawn_blocking(move || {
        let revision_range = format!("{target_branch}..{source_branch}");

        run_git_command_sync(
            &repo_path,
            &["diff", revision_range.as_str()],
            "Failed to read squash merge diff",
        )
    })
    .await?
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
) -> Result<SquashMergeOutcome, GitError> {
    spawn_blocking(move || {
        // Verify that `repo_path` is already on the target branch.
        let current_branch = detect_git_info_sync(&repo_path).ok_or_else(|| {
            GitError::OutputParse(format!(
                "Failed to detect current branch in {}",
                repo_path.display()
            ))
        })?;

        if current_branch != target_branch {
            return Err(GitError::CommandFailed {
                command: "git merge --squash".to_string(),
                stderr: format!(
                    "Cannot merge: repository is on '{current_branch}' but expected \
                     '{target_branch}'. Switch to '{target_branch}' first."
                ),
            });
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

            return Err(GitError::CommandFailed {
                command: "git diff --cached".to_string(),
                stderr: detail,
            });
        }

        // Skip hooks here because the session worktree already ran them.
        run_git_command_sync(
            &repo_path,
            &["commit", "--no-verify", "-m", commit_message.as_str()],
            "Failed to commit squash merge",
        )?;

        Ok(SquashMergeOutcome::Committed)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    /// Runs `git` in `repo_path` and asserts the command succeeds.
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

    /// Runs `git` in `repo_path` and returns trimmed stdout.
    fn run_git_stdout(repo_path: &Path, args: &[&str]) -> String {
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

        String::from_utf8_lossy(&output.stdout).trim().to_string()
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

    #[tokio::test]
    async fn squash_merge_returns_branch_mismatch_error_when_target_is_not_checked_out() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "feature-branch"]);

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "feature-branch".to_string(),
            "main".to_string(),
            "Merge feature".to_string(),
        )
        .await;

        // Assert
        let error = result.expect_err("branch mismatch should fail").to_string();
        assert!(error.contains("repository is on 'feature-branch'"));
        assert!(error.contains("Switch to 'main' first."));
    }

    #[tokio::test]
    async fn squash_merge_commits_the_provided_multiline_message() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "feature-branch"]);
        fs::write(temp_dir.path().join("feature.txt"), "feature content")
            .expect("failed to write feature file");
        run_git_command(temp_dir.path(), &["add", "feature.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Add feature"]);
        run_git_command(temp_dir.path(), &["checkout", "main"]);
        let commit_message = "Refine merge flow\n\n- Reuse the session commit body".to_string();

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "feature-branch".to_string(),
            "main".to_string(),
            commit_message.clone(),
        )
        .await;
        let head_message = run_git_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]);

        // Assert
        assert_eq!(
            result.expect("squash merge should succeed"),
            SquashMergeOutcome::Committed,
        );
        assert_eq!(head_message, commit_message);
    }

    #[tokio::test]
    async fn squash_merge_skips_commit_creation_when_changes_are_already_present() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        run_git_command(temp_dir.path(), &["checkout", "-b", "session-branch"]);
        fs::write(temp_dir.path().join("session.txt"), "session change")
            .expect("failed to write session file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(temp_dir.path(), &["commit", "-m", "Session change"]);
        run_git_command(temp_dir.path(), &["checkout", "main"]);
        fs::write(temp_dir.path().join("session.txt"), "session change")
            .expect("failed to write main file");
        run_git_command(temp_dir.path(), &["add", "session.txt"]);
        run_git_command(
            temp_dir.path(),
            &["commit", "-m", "Apply same change on main"],
        );
        let commit_count_before = run_git_stdout(temp_dir.path(), &["rev-list", "--count", "HEAD"]);
        let head_message_before = run_git_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]);

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "session-branch".to_string(),
            "main".to_string(),
            "Merge session".to_string(),
        )
        .await;
        let commit_count_after = run_git_stdout(temp_dir.path(), &["rev-list", "--count", "HEAD"]);
        let head_message_after = run_git_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]);

        // Assert
        assert_eq!(
            result.expect("squash merge should succeed"),
            SquashMergeOutcome::AlreadyPresentInTarget,
        );
        assert_eq!(commit_count_after, commit_count_before);
        assert_eq!(head_message_after, head_message_before);
    }

    #[tokio::test]
    async fn squash_merge_returns_command_detail_for_missing_source_branch() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());

        // Act
        let result = squash_merge(
            temp_dir.path().to_path_buf(),
            "missing-branch".to_string(),
            "main".to_string(),
            "Merge feature".to_string(),
        )
        .await;

        // Assert
        let error = result.expect_err("missing branch should fail").to_string();
        assert!(error.contains("Failed to squash merge missing-branch"));
        assert!(error.contains("missing-branch"));
    }
}
