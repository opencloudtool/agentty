use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;

use super::*;

/// Canonicalizes a test path for stable comparisons across symlinked
/// temporary directory roots (for example `/var` vs `/private/var`).
fn canonicalize_test_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

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
async fn test_real_git_client_runs_hook_checks_and_commits() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    fs::write(dir.path().join("README.md"), "updated repo").expect("failed to update tracked file");
    let client = RealGitClient;

    // Act
    client
        .check_pre_commit_hook_ready(dir.path().to_path_buf())
        .await
        .expect("repository without hook configuration should be ready");
    client
        .run_pre_commit_hook(dir.path().to_path_buf())
        .await
        .expect("missing pre-commit hook should be accepted");
    client
        .commit_all(
            dir.path().to_path_buf(),
            "Update repository documentation".to_string(),
        )
        .await
        .expect("real git client should commit changes");

    // Assert
    assert_eq!(
        run_git_command_stdout(dir.path(), &["log", "-1", "--pretty=%s"]),
        "Update repository documentation"
    );
}

#[tokio::test]
async fn test_real_git_client_detects_merge_conflicts() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    fs::write(dir.path().join("README.md"), "session content")
        .expect("failed to write session content");
    run_git_command(dir.path(), &["add", "README.md"]);
    run_git_command(dir.path(), &["commit", "-m", "Session change"]);
    run_git_command(dir.path(), &["checkout", "main"]);
    fs::write(dir.path().join("README.md"), "main content").expect("failed to write main content");
    run_git_command(dir.path(), &["add", "README.md"]);
    run_git_command(dir.path(), &["commit", "-m", "Main change"]);
    let client = RealGitClient;

    // Act
    let has_conflicts = client
        .has_merge_conflicts(
            dir.path().to_path_buf(),
            "session-branch".to_string(),
            "main".to_string(),
        )
        .await
        .expect("merge conflict query should succeed");

    // Assert
    assert!(has_conflicts);
}

#[tokio::test]
async fn test_real_git_client_reads_worktree_file() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    fs::write(dir.path().join("README.md"), "# Preview").expect("failed to write markdown file");
    let client = RealGitClient;

    // Act
    let result = client
        .read_worktree_file(dir.path().to_path_buf(), "README.md".to_string())
        .await
        .expect("failed to read worktree file");

    // Assert
    assert_eq!(result, WorktreeFileContent::Text("# Preview".to_string()));
}

#[tokio::test]
async fn test_real_git_client_lists_changed_files() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    fs::write(dir.path().join("new.txt"), "new content").expect("failed to write changed file");
    let client = RealGitClient;

    // Act
    let changed_files = client
        .diff_changed_files(dir.path().to_path_buf(), "main".to_string())
        .await
        .expect("failed to list changed files");

    // Assert
    assert_eq!(changed_files, vec!["new.txt".to_string()]);
}

#[tokio::test]
async fn test_real_git_client_pushes_new_remote_branch() {
    // Arrange
    let repo_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(repo_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(repo_dir.path(), &["remote", "add", "origin", &remote_path]);
    let client = RealGitClient;

    // Act
    let upstream_reference = client
        .push_current_branch_to_new_remote_branch(
            repo_dir.path().to_path_buf(),
            "review/new-branch".to_string(),
        )
        .await
        .expect("new remote branch push should succeed");
    let local_head = run_git_command_stdout(repo_dir.path(), &["rev-parse", "HEAD"]);
    let remote_head = run_git_command_stdout(
        remote_dir.path(),
        &["rev-parse", "refs/heads/review/new-branch"],
    );

    // Assert
    assert_eq!(upstream_reference, "origin/review/new-branch");
    assert_eq!(local_head, remote_head);
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
    assert_eq!(
        result.expect("squash merge should succeed"),
        SquashMergeOutcome::Committed,
    );
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
    assert_eq!(
        result.expect("squash merge should succeed"),
        SquashMergeOutcome::AlreadyPresentInTarget,
    );
}

#[tokio::test]
async fn test_commit_all_preserving_single_commit_creates_first_commit() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    let commit_message = "Session commit".to_string();
    fs::write(dir.path().join("work.txt"), "first change").expect("failed to write file");

    // Act
    let result = commit_all_preserving_single_commit(
        dir.path().to_path_buf(),
        "main".to_string(),
        commit_message.clone(),
        SingleCommitMessageStrategy::Replace,
    )
    .await;
    let commit_count = run_git_command_stdout(dir.path(), &["rev-list", "--count", "HEAD"]);
    let head_message = run_git_command_stdout(dir.path(), &["log", "-1", "--pretty=%B"]);

    // Assert
    assert!(
        result.is_ok(),
        "commit_all_preserving_single_commit should succeed: {result:?}"
    );
    assert_eq!(commit_count, "2");
    assert_eq!(head_message, commit_message);
}

#[tokio::test]
async fn test_commit_all_preserving_single_commit_amends_existing_session_commit() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    let commit_message = "Session commit".to_string();
    fs::write(dir.path().join("work.txt"), "first change").expect("failed to write file");
    commit_all_preserving_single_commit(
        dir.path().to_path_buf(),
        "main".to_string(),
        commit_message.clone(),
        SingleCommitMessageStrategy::Replace,
    )
    .await
    .expect("failed to create first session commit");
    let first_hash = run_git_command_stdout(dir.path(), &["rev-parse", "HEAD"]);
    let first_count = run_git_command_stdout(dir.path(), &["rev-list", "--count", "HEAD"]);

    // Act
    fs::write(dir.path().join("work.txt"), "second change").expect("failed to write file");
    let result = commit_all_preserving_single_commit(
        dir.path().to_path_buf(),
        "main".to_string(),
        commit_message.clone(),
        SingleCommitMessageStrategy::Replace,
    )
    .await;
    let second_hash = run_git_command_stdout(dir.path(), &["rev-parse", "HEAD"]);
    let second_count = run_git_command_stdout(dir.path(), &["rev-list", "--count", "HEAD"]);

    // Assert
    assert!(result.is_ok(), "amend commit should succeed: {result:?}");
    assert_ne!(first_hash, second_hash);
    assert_eq!(first_count, second_count);
}

#[tokio::test]
async fn test_commit_all_preserving_single_commit_replaces_amended_message() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    fs::write(dir.path().join("work.txt"), "first change").expect("failed to write file");
    commit_all_preserving_single_commit(
        dir.path().to_path_buf(),
        "main".to_string(),
        "First session message".to_string(),
        SingleCommitMessageStrategy::Replace,
    )
    .await
    .expect("failed to create first session commit");

    // Act
    fs::write(dir.path().join("work.txt"), "second change").expect("failed to write file");
    let result = commit_all_preserving_single_commit(
        dir.path().to_path_buf(),
        "main".to_string(),
        "Refined session message".to_string(),
        SingleCommitMessageStrategy::Replace,
    )
    .await;
    let head_message = run_git_command_stdout(dir.path(), &["log", "-1", "--pretty=%B"]);

    // Assert
    assert!(
        result.is_ok(),
        "replace amended message should succeed: {result:?}"
    );
    assert_eq!(head_message, "Refined session message");
}

#[tokio::test]
async fn test_commit_all_preserving_single_commit_retries_index_lock_and_succeeds() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    let commit_message = "Session commit".to_string();
    fs::write(dir.path().join("work.txt"), "locked change").expect("failed to write file");
    let index_lock_path = dir.path().join(".git").join("index.lock");
    fs::write(&index_lock_path, "active writer").expect("failed to write lock file");
    let lock_cleanup = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        fs::remove_file(index_lock_path).expect("writer should release its lock");
    });

    // Act
    let result = commit_all_preserving_single_commit(
        dir.path().to_path_buf(),
        "main".to_string(),
        commit_message.clone(),
        SingleCommitMessageStrategy::Replace,
    )
    .await;
    lock_cleanup
        .await
        .expect("failed to join lock cleanup task");
    let head_message = run_git_command_stdout(dir.path(), &["log", "-1", "--pretty=%B"]);

    // Assert
    assert!(
        result.is_ok(),
        "retry with index lock should succeed: {result:?}"
    );
    assert_eq!(head_message, commit_message);
}

#[tokio::test]
async fn test_diff_hides_leading_squash_merged_commit_for_non_rebased_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    fs::write(dir.path().join("merged.txt"), "already merged change")
        .expect("failed to write merged file");
    run_git_command(dir.path(), &["add", "merged.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Session change"]);
    run_git_command(dir.path(), &["checkout", "main"]);
    run_git_command(dir.path(), &["merge", "--squash", "session-branch"]);
    run_git_command(dir.path(), &["commit", "-m", "Squash merge session change"]);
    run_git_command(dir.path(), &["checkout", "session-branch"]);

    // Act
    let diff_output = diff(dir.path().to_path_buf(), "main".to_string())
        .await
        .expect("failed to load diff");

    // Assert
    assert!(
        diff_output.trim().is_empty(),
        "expected no diff, got: {diff_output}"
    );
}

#[tokio::test]
async fn test_diff_keeps_new_commits_after_leading_squash_merged_commit() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    fs::write(dir.path().join("merged.txt"), "already merged change")
        .expect("failed to write merged file");
    run_git_command(dir.path(), &["add", "merged.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Session change"]);
    run_git_command(dir.path(), &["checkout", "main"]);
    run_git_command(dir.path(), &["merge", "--squash", "session-branch"]);
    run_git_command(dir.path(), &["commit", "-m", "Squash merge session change"]);
    run_git_command(dir.path(), &["checkout", "session-branch"]);
    fs::write(dir.path().join("new.txt"), "new session-only change")
        .expect("failed to write new file");
    run_git_command(dir.path(), &["add", "new.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "New session change"]);

    // Act
    let diff_output = diff(dir.path().to_path_buf(), "main".to_string())
        .await
        .expect("failed to load diff");

    // Assert
    assert!(diff_output.contains("new.txt"));
    assert!(!diff_output.contains("merged.txt"));
}

#[tokio::test]
async fn test_diff_does_not_include_base_only_commits() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "session-branch"]);
    fs::write(dir.path().join("session.txt"), "session change").expect("failed to write file");
    run_git_command(dir.path(), &["add", "session.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Session change"]);
    run_git_command(dir.path(), &["checkout", "main"]);
    fs::write(dir.path().join("main-only.txt"), "base branch only")
        .expect("failed to write base-only file");
    run_git_command(dir.path(), &["add", "main-only.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Main branch change"]);
    run_git_command(dir.path(), &["checkout", "session-branch"]);

    // Act
    let diff_output = diff(dir.path().to_path_buf(), "main".to_string())
        .await
        .expect("failed to load diff");

    // Assert
    assert!(diff_output.contains("session.txt"));
    assert!(!diff_output.contains("main-only.txt"));
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
async fn test_worktree_status_reports_dirty_repo_paths() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    fs::write(dir.path().join("README.md"), "dirty change").expect("failed to write change");
    fs::write(dir.path().join("new-file.txt"), "new").expect("failed to write new file");

    // Act
    let status = worktree_status(dir.path().to_path_buf())
        .await
        .expect("failed to read worktree status");

    // Assert
    assert!(status.contains("README.md"));
    assert!(status.contains("new-file.txt"));
}

#[tokio::test]
async fn test_status_reads_preserve_index_with_stale_file_metadata() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    let index_path = dir.path().join(".git/index");
    let original_index = fs::read(&index_path).expect("failed to read index");
    let readme = fs::File::options()
        .write(true)
        .open(dir.path().join("README.md"))
        .expect("failed to open tracked file");
    readme
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .expect("failed to invalidate cached file metadata");

    // Act
    let status = worktree_status(dir.path().to_path_buf())
        .await
        .expect("failed to read worktree status");
    let tracked_status = tracked_worktree_status(dir.path().to_path_buf())
        .await
        .expect("failed to read tracked status");
    let sync_status = crate::repo::run_git_command_sync(
        dir.path(),
        &["status", "--porcelain"],
        "Failed to read synchronous status",
    )
    .expect("failed to read synchronous status");

    // Assert
    assert_eq!(status, "");
    assert_eq!(tracked_status, "");
    assert_eq!(sync_status, "");
    assert_eq!(
        fs::read(&index_path).expect("failed to read index"),
        original_index
    );
    // Prove the fixture actually needs an index refresh when optional
    // writes are enabled, rather than merely reading an unchanged index.
    let refresh = Command::new("git")
        .args(["status", "--porcelain"])
        .env("GIT_OPTIONAL_LOCKS", "1")
        .current_dir(dir.path())
        .output()
        .expect("failed to refresh index");
    assert!(refresh.status.success());
    assert_ne!(
        fs::read(index_path).expect("failed to read refreshed index"),
        original_index
    );
}

#[tokio::test]
async fn test_tracked_worktree_status_ignores_untracked_repo_paths() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    fs::write(dir.path().join("README.md"), "dirty change").expect("failed to write change");
    fs::write(dir.path().join("new-file.txt"), "new").expect("failed to write new file");

    // Act
    let status = tracked_worktree_status(dir.path().to_path_buf())
        .await
        .expect("failed to read tracked worktree status");

    // Assert
    assert!(status.contains("README.md"));
    assert!(!status.contains("new-file.txt"));
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
    assert_eq!(
        canonicalize_test_path(&repo_root),
        canonicalize_test_path(dir.path())
    );
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
        "wt/main-repo-root-test".to_string(),
        "main".to_string(),
    )
    .await
    .expect("failed to create linked worktree");

    // Act
    let repo_root = main_repo_root(linked_worktree)
        .await
        .expect("failed to resolve shared repo root");

    // Assert
    assert_eq!(
        canonicalize_test_path(&repo_root),
        canonicalize_test_path(dir.path())
    );
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
async fn test_ref_hash_resolves_branch_head() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    let expected_hash = run_git_command_stdout(dir.path(), &["rev-parse", "main"]);

    // Act
    let resolved_hash = ref_hash(dir.path().to_path_buf(), "main".to_string())
        .await
        .expect("failed to resolve main hash");

    // Assert
    assert_eq!(resolved_hash, expected_hash);
}

#[tokio::test]
async fn test_rebase_onto_start_replays_commits_after_old_base() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(dir.path(), &["checkout", "-b", "parent"]);
    fs::write(dir.path().join("parent.txt"), "parent").expect("failed to write parent file");
    run_git_command(dir.path(), &["add", "parent.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Parent change"]);
    let parent_tip = run_git_command_stdout(dir.path(), &["rev-parse", "HEAD"]);
    run_git_command(dir.path(), &["checkout", "-b", "child"]);
    fs::write(dir.path().join("child.txt"), "child").expect("failed to write child file");
    run_git_command(dir.path(), &["add", "child.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Child change"]);
    run_git_command(dir.path(), &["checkout", "main"]);
    fs::write(dir.path().join("main.txt"), "main").expect("failed to write main file");
    run_git_command(dir.path(), &["add", "main.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Main change"]);
    run_git_command(dir.path(), &["checkout", "child"]);

    // Act
    let result = rebase_onto_start(dir.path().to_path_buf(), "main".to_string(), parent_tip)
        .await
        .expect("failed to start rebase --onto");
    let child_only_subjects = run_git_command_stdout(
        dir.path(),
        &["log", "--format=%s", "--reverse", "main..HEAD"],
    );

    // Assert
    assert_eq!(result, RebaseStepResult::Completed);
    assert_eq!(child_only_subjects, "Child change");
    assert!(!dir.path().join("parent.txt").exists());
    assert!(dir.path().join("child.txt").exists());
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
    assert!(
        matches!(result, Ok(PullRebaseResult::Completed)),
        "pull_rebase should complete: {result:?}"
    );
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
    assert!(
        matches!(result, Ok(PullRebaseResult::Completed)),
        "pull_rebase with local upstream should complete: {result:?}"
    );
}

#[tokio::test]
async fn test_list_upstream_commit_titles_returns_error_without_upstream() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());

    // Act
    let result = list_upstream_commit_titles(dir.path().to_path_buf()).await;

    // Assert
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_upstream_commit_titles_returns_new_upstream_commit_titles() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    let contributor_dir = tempdir().expect("failed to create contributor temp dir");
    let contributor_clone_path = contributor_dir.path().join("clone");
    setup_test_git_repo(dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);

    let remote_path = remote_dir.path().to_string_lossy().to_string();
    let contributor_clone_path_text = contributor_clone_path.to_string_lossy().to_string();
    run_git_command(dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(dir.path(), &["push", "-u", "origin", "main"]);

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
        .expect("failed to write remote change");
    run_git_command(&contributor_clone_path, &["add", "remote.txt"]);
    run_git_command(
        &contributor_clone_path,
        &["commit", "-m", "Remote commit title"],
    );
    run_git_command(&contributor_clone_path, &["push", "origin", "main"]);
    run_git_command(dir.path(), &["fetch", "origin"]);

    // Act
    let titles = list_upstream_commit_titles(dir.path().to_path_buf())
        .await
        .expect("failed to list upstream commit titles");

    // Assert
    assert_eq!(titles, vec!["Remote commit title".to_string()]);
}

#[tokio::test]
async fn test_list_local_commit_titles_returns_error_without_upstream() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(dir.path());

    // Act
    let result = list_local_commit_titles(dir.path().to_path_buf()).await;

    // Assert
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_local_commit_titles_returns_new_local_commit_titles() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);

    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(dir.path(), &["push", "-u", "origin", "main"]);

    fs::write(dir.path().join("local_1.txt"), "local change 1")
        .expect("failed to write local change 1");
    run_git_command(dir.path(), &["add", "local_1.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Local commit title one"]);

    fs::write(dir.path().join("local_2.txt"), "local change 2")
        .expect("failed to write local change 2");
    run_git_command(dir.path(), &["add", "local_2.txt"]);
    run_git_command(dir.path(), &["commit", "-m", "Local commit title two"]);

    // Act
    let titles = list_local_commit_titles(dir.path().to_path_buf())
        .await
        .expect("failed to list local commit titles");

    // Assert
    assert_eq!(
        titles,
        vec![
            "Local commit title one".to_string(),
            "Local commit title two".to_string(),
        ]
    );
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

#[tokio::test]
async fn test_push_current_branch_returns_upstream_reference() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(dir.path(), &["remote", "add", "origin", &remote_path]);

    // Act
    let upstream_reference = push_current_branch(dir.path().to_path_buf())
        .await
        .expect("push should set upstream");

    // Assert
    assert_eq!(upstream_reference, "origin/main");
}

#[tokio::test]
async fn test_push_current_branch_to_remote_branch_returns_upstream_reference() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(dir.path(), &["remote", "add", "origin", &remote_path]);

    // Act
    let upstream_reference = push_current_branch_to_remote_branch(
        dir.path().to_path_buf(),
        "review/custom-branch".to_string(),
    )
    .await
    .expect("push should set a custom upstream");

    // Assert
    assert_eq!(upstream_reference, "origin/review/custom-branch");
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
