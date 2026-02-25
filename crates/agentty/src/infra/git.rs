use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Squash-merge workflows.
mod merge;
/// Rebase and conflict workflows.
mod rebase;
/// Repository-level helpers and metadata operations.
mod repo;
/// Commit, diff, and remote synchronization workflows.
mod sync;
/// Worktree and branch-detection workflows.
mod worktree;

pub use merge::{SquashMergeOutcome, squash_merge, squash_merge_diff};
pub use rebase::{
    RebaseStepResult, abort_rebase, has_unmerged_paths, is_rebase_in_progress,
    list_conflicted_files, list_staged_conflict_marker_files, rebase, rebase_continue,
    rebase_start,
};
pub use repo::{main_repo_root, repo_url};
pub use sync::{
    PullRebaseResult, commit_all, commit_all_preserving_single_commit, delete_branch, diff,
    fetch_remote, get_ahead_behind, head_short_hash, is_worktree_clean, pull_rebase,
    push_current_branch, stage_all,
};
pub use worktree::{create_worktree, detect_git_info, find_git_repo_root, remove_worktree};

/// Boxed async result used by [`GitClient`] trait methods.
pub type GitFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Low-level async git boundary used by app orchestration code.
///
/// Production uses [`RealGitClient`], while tests can inject
/// `MockGitClient` to avoid flaky multi-command process workflows.
#[cfg_attr(test, mockall::automock)]
pub trait GitClient: Send + Sync {
    /// Detects current branch information for `dir`.
    fn detect_git_info(&self, dir: PathBuf) -> GitFuture<Option<String>>;

    /// Resolves the repository root for `dir`.
    fn find_git_repo_root(&self, dir: PathBuf) -> GitFuture<Option<PathBuf>>;

    /// Creates a new worktree branch.
    fn create_worktree(
        &self,
        repo_path: PathBuf,
        worktree_path: PathBuf,
        branch_name: String,
        base_branch: String,
    ) -> GitFuture<Result<(), String>>;

    /// Removes an existing worktree.
    fn remove_worktree(&self, worktree_path: PathBuf) -> GitFuture<Result<(), String>>;

    /// Returns squash-merge diff text for source/target range.
    fn squash_merge_diff(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
    ) -> GitFuture<Result<String, String>>;

    /// Performs a squash merge and returns outcome.
    fn squash_merge(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
        commit_message: String,
    ) -> GitFuture<Result<SquashMergeOutcome, String>>;

    /// Runs full rebase against `target_branch`.
    fn rebase(&self, repo_path: PathBuf, target_branch: String) -> GitFuture<Result<(), String>>;

    /// Starts rebase and reports completion/conflict.
    fn rebase_start(
        &self,
        repo_path: PathBuf,
        target_branch: String,
    ) -> GitFuture<Result<RebaseStepResult, String>>;

    /// Continues in-progress rebase.
    fn rebase_continue(&self, repo_path: PathBuf) -> GitFuture<Result<RebaseStepResult, String>>;

    /// Aborts in-progress rebase.
    fn abort_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>>;

    /// Returns whether rebase state exists.
    fn is_rebase_in_progress(&self, repo_path: PathBuf) -> GitFuture<Result<bool, String>>;

    /// Returns whether unmerged index entries remain.
    fn has_unmerged_paths(&self, repo_path: PathBuf) -> GitFuture<Result<bool, String>>;

    /// Lists staged files that still contain conflict markers.
    fn list_staged_conflict_marker_files(
        &self,
        repo_path: PathBuf,
        paths: Vec<String>,
    ) -> GitFuture<Result<Vec<String>, String>>;

    /// Lists conflicted files from index.
    fn list_conflicted_files(&self, repo_path: PathBuf) -> GitFuture<Result<Vec<String>, String>>;

    /// Commits all changes in repository.
    fn commit_all(
        &self,
        repo_path: PathBuf,
        message: String,
        no_verify: bool,
    ) -> GitFuture<Result<(), String>>;

    /// Commits all changes while preserving single evolving session commit.
    fn commit_all_preserving_single_commit(
        &self,
        repo_path: PathBuf,
        commit_message: String,
        no_verify: bool,
    ) -> GitFuture<Result<(), String>>;

    /// Stages all modified files.
    fn stage_all(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>>;

    /// Returns short `HEAD` hash.
    fn head_short_hash(&self, repo_path: PathBuf) -> GitFuture<Result<String, String>>;

    /// Deletes branch in repository.
    fn delete_branch(
        &self,
        repo_path: PathBuf,
        branch_name: String,
    ) -> GitFuture<Result<(), String>>;

    /// Returns diff against base branch.
    fn diff(&self, repo_path: PathBuf, base_branch: String) -> GitFuture<Result<String, String>>;

    /// Returns whether worktree is clean.
    fn is_worktree_clean(&self, repo_path: PathBuf) -> GitFuture<Result<bool, String>>;

    /// Performs pull --rebase operation.
    fn pull_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<PullRebaseResult, String>>;

    /// Pushes current branch.
    fn push_current_branch(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>>;

    /// Fetches from remote.
    fn fetch_remote(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>>;

    /// Reads ahead/behind status.
    fn get_ahead_behind(&self, repo_path: PathBuf) -> GitFuture<Result<(u32, u32), String>>;

    /// Reads repository origin URL.
    fn repo_url(&self, repo_path: PathBuf) -> GitFuture<Result<String, String>>;

    /// Resolves the main repository root for repository/worktree path.
    fn main_repo_root(&self, repo_path: PathBuf) -> GitFuture<Result<PathBuf, String>>;
}

/// Production [`GitClient`] implementation backed by real git commands.
pub struct RealGitClient;

impl GitClient for RealGitClient {
    fn detect_git_info(&self, dir: PathBuf) -> GitFuture<Option<String>> {
        Box::pin(async move { detect_git_info(dir).await })
    }

    fn find_git_repo_root(&self, dir: PathBuf) -> GitFuture<Option<PathBuf>> {
        Box::pin(async move { find_git_repo_root(dir).await })
    }

    fn create_worktree(
        &self,
        repo_path: PathBuf,
        worktree_path: PathBuf,
        branch_name: String,
        base_branch: String,
    ) -> GitFuture<Result<(), String>> {
        Box::pin(async move {
            create_worktree(repo_path, worktree_path, branch_name, base_branch).await
        })
    }

    fn remove_worktree(&self, worktree_path: PathBuf) -> GitFuture<Result<(), String>> {
        Box::pin(async move { remove_worktree(worktree_path).await })
    }

    fn squash_merge_diff(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
    ) -> GitFuture<Result<String, String>> {
        Box::pin(async move { squash_merge_diff(repo_path, source_branch, target_branch).await })
    }

    fn squash_merge(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
        commit_message: String,
    ) -> GitFuture<Result<SquashMergeOutcome, String>> {
        Box::pin(async move {
            squash_merge(repo_path, source_branch, target_branch, commit_message).await
        })
    }

    fn rebase(&self, repo_path: PathBuf, target_branch: String) -> GitFuture<Result<(), String>> {
        Box::pin(async move { rebase(repo_path, target_branch).await })
    }

    fn rebase_start(
        &self,
        repo_path: PathBuf,
        target_branch: String,
    ) -> GitFuture<Result<RebaseStepResult, String>> {
        Box::pin(async move { rebase_start(repo_path, target_branch).await })
    }

    fn rebase_continue(&self, repo_path: PathBuf) -> GitFuture<Result<RebaseStepResult, String>> {
        Box::pin(async move { rebase_continue(repo_path).await })
    }

    fn abort_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>> {
        Box::pin(async move { abort_rebase(repo_path).await })
    }

    fn is_rebase_in_progress(&self, repo_path: PathBuf) -> GitFuture<Result<bool, String>> {
        Box::pin(async move { is_rebase_in_progress(repo_path).await })
    }

    fn has_unmerged_paths(&self, repo_path: PathBuf) -> GitFuture<Result<bool, String>> {
        Box::pin(async move { has_unmerged_paths(repo_path).await })
    }

    fn list_staged_conflict_marker_files(
        &self,
        repo_path: PathBuf,
        paths: Vec<String>,
    ) -> GitFuture<Result<Vec<String>, String>> {
        Box::pin(async move { list_staged_conflict_marker_files(repo_path, paths).await })
    }

    fn list_conflicted_files(&self, repo_path: PathBuf) -> GitFuture<Result<Vec<String>, String>> {
        Box::pin(async move { list_conflicted_files(repo_path).await })
    }

    fn commit_all(
        &self,
        repo_path: PathBuf,
        message: String,
        no_verify: bool,
    ) -> GitFuture<Result<(), String>> {
        Box::pin(async move { commit_all(repo_path, message, no_verify).await })
    }

    fn commit_all_preserving_single_commit(
        &self,
        repo_path: PathBuf,
        commit_message: String,
        no_verify: bool,
    ) -> GitFuture<Result<(), String>> {
        Box::pin(async move {
            commit_all_preserving_single_commit(repo_path, commit_message, no_verify).await
        })
    }

    fn stage_all(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>> {
        Box::pin(async move { stage_all(repo_path).await })
    }

    fn head_short_hash(&self, repo_path: PathBuf) -> GitFuture<Result<String, String>> {
        Box::pin(async move { head_short_hash(repo_path).await })
    }

    fn delete_branch(
        &self,
        repo_path: PathBuf,
        branch_name: String,
    ) -> GitFuture<Result<(), String>> {
        Box::pin(async move { delete_branch(repo_path, branch_name).await })
    }

    fn diff(&self, repo_path: PathBuf, base_branch: String) -> GitFuture<Result<String, String>> {
        Box::pin(async move { diff(repo_path, base_branch).await })
    }

    fn is_worktree_clean(&self, repo_path: PathBuf) -> GitFuture<Result<bool, String>> {
        Box::pin(async move { is_worktree_clean(repo_path).await })
    }

    fn pull_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<PullRebaseResult, String>> {
        Box::pin(async move { pull_rebase(repo_path).await })
    }

    fn push_current_branch(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>> {
        Box::pin(async move { push_current_branch(repo_path).await })
    }

    fn fetch_remote(&self, repo_path: PathBuf) -> GitFuture<Result<(), String>> {
        Box::pin(async move { fetch_remote(repo_path).await })
    }

    fn get_ahead_behind(&self, repo_path: PathBuf) -> GitFuture<Result<(u32, u32), String>> {
        Box::pin(async move { get_ahead_behind(repo_path).await })
    }

    fn repo_url(&self, repo_path: PathBuf) -> GitFuture<Result<String, String>> {
        Box::pin(async move { repo_url(repo_path).await })
    }

    fn main_repo_root(&self, repo_path: PathBuf) -> GitFuture<Result<PathBuf, String>> {
        Box::pin(async move { main_repo_root(repo_path).await })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;
    use std::{fs, thread};

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

    fn run_git_command_stdout(repo_path: &Path, args: &[&str]) -> String {
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

    #[tokio::test]
    async fn test_commit_all_preserving_single_commit_creates_first_commit() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let commit_message = "Session commit".to_string();
        fs::write(dir.path().join("work.txt"), "first change").expect("failed to write file");

        // Act
        let result = commit_all_preserving_single_commit(
            dir.path().to_path_buf(),
            commit_message.clone(),
            false,
        )
        .await;
        let commit_count = run_git_command_stdout(dir.path(), &["rev-list", "--count", "HEAD"]);
        let head_message = run_git_command_stdout(dir.path(), &["log", "-1", "--pretty=%B"]);

        // Assert
        assert_eq!(result, Ok(()));
        assert_eq!(commit_count, "2");
        assert_eq!(head_message, commit_message);
    }

    #[tokio::test]
    async fn test_commit_all_preserving_single_commit_amends_existing_session_commit() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let commit_message = "Session commit".to_string();
        fs::write(dir.path().join("work.txt"), "first change").expect("failed to write file");
        commit_all_preserving_single_commit(
            dir.path().to_path_buf(),
            commit_message.clone(),
            false,
        )
        .await
        .expect("failed to create first session commit");
        let first_hash = run_git_command_stdout(dir.path(), &["rev-parse", "HEAD"]);
        let first_count = run_git_command_stdout(dir.path(), &["rev-list", "--count", "HEAD"]);

        // Act
        fs::write(dir.path().join("work.txt"), "second change").expect("failed to write file");
        let result = commit_all_preserving_single_commit(
            dir.path().to_path_buf(),
            commit_message.clone(),
            false,
        )
        .await;
        let second_hash = run_git_command_stdout(dir.path(), &["rev-parse", "HEAD"]);
        let second_count = run_git_command_stdout(dir.path(), &["rev-list", "--count", "HEAD"]);

        // Assert
        assert_eq!(result, Ok(()));
        assert_ne!(first_hash, second_hash);
        assert_eq!(first_count, second_count);
    }

    #[tokio::test]
    async fn test_commit_all_preserving_single_commit_retries_index_lock_and_succeeds() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let commit_message = "Session commit".to_string();
        fs::write(dir.path().join("work.txt"), "locked change").expect("failed to write file");
        let index_lock_path = dir.path().join(".git").join("index.lock");
        fs::write(&index_lock_path, "stale lock").expect("failed to write lock file");
        let lock_cleanup = thread::spawn(move || {
            thread::sleep(Duration::from_millis(250));
            let _ = fs::remove_file(index_lock_path);
        });

        // Act
        let result = commit_all_preserving_single_commit(
            dir.path().to_path_buf(),
            commit_message.clone(),
            false,
        )
        .await;
        lock_cleanup
            .join()
            .expect("failed to join lock cleanup thread");
        let head_message = run_git_command_stdout(dir.path(), &["log", "-1", "--pretty=%B"]);

        // Assert
        assert_eq!(result, Ok(()));
        assert_eq!(head_message, commit_message);
    }

    #[tokio::test]
    async fn test_is_worktree_clean_returns_true_for_clean_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());

        // Act
        let is_clean = is_worktree_clean(dir.path().to_path_buf())
            .await
            .expect("failed to check worktree cleanliness");

        // Assert
        assert!(is_clean);
    }

    #[tokio::test]
    async fn test_is_worktree_clean_returns_false_for_dirty_repo() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        fs::write(dir.path().join("README.md"), "dirty change").expect("failed to write change");

        // Act
        let is_clean = is_worktree_clean(dir.path().to_path_buf())
            .await
            .expect("failed to check worktree cleanliness");

        // Assert
        assert!(!is_clean);
    }

    #[tokio::test]
    async fn test_main_repo_root_returns_repo_root_for_main_worktree() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());

        // Act
        let repo_root = main_repo_root(dir.path().to_path_buf())
            .await
            .expect("failed to resolve main repo root");

        // Assert
        assert_eq!(repo_root, dir.path().to_path_buf());
    }

    #[tokio::test]
    async fn test_main_repo_root_returns_shared_repo_root_for_linked_worktree() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let linked_worktree = dir.path().join("linked-worktree");
        create_worktree(
            dir.path().to_path_buf(),
            linked_worktree.clone(),
            "agentty/main-repo-root-test".to_string(),
            "main".to_string(),
        )
        .await
        .expect("failed to create linked worktree");

        // Act
        let repo_root = main_repo_root(linked_worktree)
            .await
            .expect("failed to resolve shared repo root");

        // Assert
        assert_eq!(repo_root, dir.path().to_path_buf());
    }

    #[tokio::test]
    async fn test_abort_rebase_cleans_stale_rebase_merge_metadata() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());
        let stale_rebase_dir = dir.path().join(".git/rebase-merge");
        fs::create_dir_all(&stale_rebase_dir).expect("failed to create stale rebase metadata");
        fs::write(stale_rebase_dir.join("head-name"), "refs/heads/main")
            .expect("failed to write stale rebase metadata");

        // Act
        let result = abort_rebase(dir.path().to_path_buf()).await;

        // Assert
        assert_eq!(result, Ok(()));
        assert!(!stale_rebase_dir.exists());
    }

    #[tokio::test]
    async fn test_abort_rebase_returns_error_without_rebase_state_or_stale_metadata() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());

        // Act
        let result = abort_rebase(dir.path().to_path_buf()).await;

        // Assert
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pull_rebase_returns_error_without_upstream() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());

        // Act
        let result = pull_rebase(dir.path().to_path_buf()).await;

        // Assert
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pull_rebase_targets_single_upstream_when_merge_targets_are_ambiguous() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let remote_dir = tempdir().expect("failed to create remote temp dir");
        setup_test_git_repo(dir.path());
        run_git_command(remote_dir.path(), &["init", "--bare"]);

        let remote_path = remote_dir.path().to_string_lossy().to_string();
        run_git_command(dir.path(), &["remote", "add", "origin", &remote_path]);
        run_git_command(dir.path(), &["push", "-u", "origin", "main"]);

        run_git_command(dir.path(), &["checkout", "-b", "feature"]);
        fs::write(dir.path().join("feature.txt"), "feature change").expect("failed to write file");
        run_git_command(dir.path(), &["add", "feature.txt"]);
        run_git_command(dir.path(), &["commit", "-m", "Add feature branch"]);
        run_git_command(dir.path(), &["push", "-u", "origin", "feature"]);
        run_git_command(dir.path(), &["checkout", "main"]);

        run_git_command(
            dir.path(),
            &["config", "--add", "branch.main.merge", "refs/heads/feature"],
        );

        let pull_without_explicit_target = Command::new("git")
            .args(["pull", "--rebase"])
            .current_dir(dir.path())
            .output()
            .expect("failed to run pull --rebase");

        assert!(
            !pull_without_explicit_target.status.success(),
            "expected plain pull --rebase to fail in ambiguous merge-target setup"
        );
        assert!(
            String::from_utf8_lossy(&pull_without_explicit_target.stderr)
                .contains("Cannot rebase onto multiple branches"),
            "expected ambiguous merge-target failure"
        );

        // Act
        let result = pull_rebase(dir.path().to_path_buf()).await;

        // Assert
        assert_eq!(result, Ok(PullRebaseResult::Completed));
    }

    #[tokio::test]
    async fn test_pull_rebase_targets_local_upstream_when_upstream_name_has_no_remote_prefix() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());

        run_git_command(dir.path(), &["checkout", "-b", "feature"]);
        fs::write(dir.path().join("feature.txt"), "feature change").expect("failed to write file");
        run_git_command(dir.path(), &["add", "feature.txt"]);
        run_git_command(dir.path(), &["commit", "-m", "Add feature branch"]);
        run_git_command(dir.path(), &["checkout", "main"]);

        run_git_command(dir.path(), &["config", "branch.main.remote", "."]);
        run_git_command(
            dir.path(),
            &[
                "config",
                "--replace-all",
                "branch.main.merge",
                "refs/heads/main",
            ],
        );
        run_git_command(
            dir.path(),
            &["config", "--add", "branch.main.merge", "refs/heads/feature"],
        );

        let pull_without_explicit_target = Command::new("git")
            .args(["pull", "--rebase"])
            .current_dir(dir.path())
            .output()
            .expect("failed to run pull --rebase");

        assert!(
            !pull_without_explicit_target.status.success(),
            "expected plain pull --rebase to fail in ambiguous merge-target setup"
        );
        assert!(
            String::from_utf8_lossy(&pull_without_explicit_target.stderr)
                .contains("Cannot rebase onto multiple branches"),
            "expected ambiguous merge-target failure"
        );

        // Act
        let result = pull_rebase(dir.path().to_path_buf()).await;

        // Assert
        assert_eq!(result, Ok(PullRebaseResult::Completed));
    }

    #[tokio::test]
    async fn test_push_current_branch_returns_error_without_remote() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(dir.path());

        // Act
        let result = push_current_branch(dir.path().to_path_buf()).await;

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_is_no_upstream_error_detects_upstream_hint() {
        // Arrange
        let detail = "fatal: The current branch main has no upstream branch.";

        // Act
        let is_no_upstream = sync::is_no_upstream_error(detail);

        // Assert
        assert!(is_no_upstream);
    }

    #[test]
    fn test_is_rebase_conflict_detects_conflict_keyword() {
        // Arrange
        let detail = "CONFLICT (content): Merge conflict in src/main.rs";

        // Act / Assert
        assert!(rebase::is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_could_not_apply() {
        // Arrange
        let detail = "error: could not apply abc1234... Update handler";

        // Act / Assert
        assert!(rebase::is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_mark_as_resolved() {
        // Arrange
        let detail = "hint: mark them as resolved using git add";

        // Act / Assert
        assert!(rebase::is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_unresolved_conflict() {
        // Arrange
        let detail = "fatal: Exiting because of an unresolved conflict.";

        // Act / Assert
        assert!(rebase::is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_detects_committing_not_possible() {
        // Arrange
        let detail = "error: Committing is not possible because you have unmerged files.";

        // Act / Assert
        assert!(rebase::is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_returns_false_for_unrelated_error() {
        // Arrange
        let detail = "fatal: not a git repository (or any parent up to mount point /)";

        // Act / Assert
        assert!(!rebase::is_rebase_conflict(detail));
    }

    #[test]
    fn test_is_rebase_conflict_returns_false_for_index_lock_error() {
        // Arrange
        let detail = "fatal: Unable to create '.git/index.lock': File exists.";

        // Act / Assert
        assert!(!rebase::is_rebase_conflict(detail));
    }
}
