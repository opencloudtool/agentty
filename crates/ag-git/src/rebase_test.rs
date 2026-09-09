use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::process::{Command, Output};

use mockall::predicate::eq;
use tempfile::tempdir;

use super::*;
use crate::sleeper::MockSleeper;

#[tokio::test]
async fn rebase_probe_reports_missing_repository() {
    // Arrange
    let directory = tempdir().expect("temporary directory should exist");
    let missing = directory.path().join("removed-worktree");

    // Act
    let error = is_rebase_in_progress(missing.clone())
        .await
        .expect_err("missing worktree should be classified");

    // Assert
    assert!(
        matches!(error, GitError::RepositoryUnavailable { ref detail }
            if detail.contains(&missing.display().to_string()))
    );
}

#[tokio::test]
async fn rebase_probe_preserves_invalid_repository_and_io_errors() {
    // Arrange
    let directory = tempdir().expect("temporary directory should exist");
    let parent_file = directory.path().join("file");
    fs::write(&parent_file, "not a directory").expect("file should be written");

    // Act
    let invalid_repository = is_rebase_in_progress(directory.path().to_path_buf()).await;
    let io_error = is_rebase_in_progress(parent_file.join("worktree")).await;

    // Assert
    assert!(
        matches!(invalid_repository, Err(GitError::OutputParse(message))
            if message.contains(&directory.path().display().to_string()))
    );
    assert!(matches!(io_error, Err(GitError::Io(_))));
}

#[tokio::test]
async fn rebase_probe_detects_metadata_in_checkout_and_linked_worktree() {
    // Arrange
    let directory = tempdir().expect("temporary directory should exist");
    let checkout = directory.path().join("checkout");
    let linked = directory.path().join("linked");
    let git_dir = checkout.join(".git");
    fs::create_dir_all(&git_dir).expect("git directory should exist");
    fs::create_dir(&linked).expect("worktree directory should exist");
    fs::write(linked.join(".git"), "gitdir: ../checkout/.git\n")
        .expect("gitdir file should be written");

    // Act / Assert
    assert!(
        !is_rebase_in_progress(checkout.clone())
            .await
            .expect("probe should succeed")
    );
    for metadata in ["rebase-merge", "rebase-apply"] {
        fs::create_dir(git_dir.join(metadata)).expect("rebase metadata should exist");
        assert!(
            is_rebase_in_progress(checkout.clone())
                .await
                .expect("probe should succeed")
        );
        assert!(
            is_rebase_in_progress(linked.clone())
                .await
                .expect("probe should succeed")
        );
        fs::remove_dir(git_dir.join(metadata)).expect("rebase metadata should be removed");
    }
}

#[test]
fn test_run_git_command_with_index_lock_retry_retries_and_sleeps_before_success() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let mut sleeper = MockSleeper::new();
    let repo_path = Path::new(".");
    let args = ["rebase", "main"];
    let environment: [(&str, &str); 0] = [];

    command_runner
        .expect_run_git_command_output_with_env()
        .times(1)
        .returning(|_, _, _| Ok(git_index_lock_output()));
    command_runner
        .expect_run_git_command_output_with_env()
        .times(1)
        .returning(|_, _, _| Ok(success_output()));

    sleeper
        .expect_sleep()
        .with(eq(GIT_INDEX_LOCK_RETRY_DELAY))
        .times(1)
        .return_once(|_| {});

    // Act
    let output = run_git_command_with_index_lock_retry_with_dependencies(
        repo_path,
        &args,
        &environment,
        &command_runner,
        &sleeper,
    )
    .expect("retry helper should return command output");

    // Assert
    assert!(output.status.success());
}

#[test]
fn test_run_git_command_with_index_lock_retry_passes_owned_args_and_environment() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let mut sleeper = MockSleeper::new();
    let repo_path = Path::new(".");
    let args = ["-c", "core.editor=true", "rebase", "main"];
    let environment = [("GIT_EDITOR", "true")];

    command_runner
        .expect_run_git_command_output_with_env()
        .withf(|repo_path, args, environment| {
            repo_path == Path::new(".")
                && args
                    .iter()
                    .map(String::as_str)
                    .eq(["-c", "core.editor=true", "rebase", "main"])
                && environment
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .eq([("GIT_EDITOR", "true")])
        })
        .times(1)
        .returning(|_, _, _| Ok(success_output()));
    sleeper.expect_sleep().times(0);

    // Act
    let output = run_git_command_with_index_lock_retry_with_dependencies(
        repo_path,
        &args,
        &environment,
        &command_runner,
        &sleeper,
    )
    .expect("retry helper should return command output");

    // Assert
    assert!(output.status.success());
}

#[test]
fn test_run_git_command_with_index_lock_retry_returns_last_lock_failure() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let mut sleeper = MockSleeper::new();
    let repo_path = Path::new(".");
    let args = ["rebase", "main"];
    let environment: [(&str, &str); 0] = [];

    command_runner
        .expect_run_git_command_output_with_env()
        .times(GIT_INDEX_LOCK_RETRY_ATTEMPTS)
        .returning(|_, _, _| Ok(git_index_lock_output()));
    sleeper
        .expect_sleep()
        .with(eq(GIT_INDEX_LOCK_RETRY_DELAY))
        .times(GIT_INDEX_LOCK_RETRY_ATTEMPTS - 1)
        .returning(|_| {});

    // Act
    let output = run_git_command_with_index_lock_retry_with_dependencies(
        repo_path,
        &args,
        &environment,
        &command_runner,
        &sleeper,
    )
    .expect("retry helper should return command output");

    // Assert
    assert!(!output.status.success());
    assert!(command_output_detail(&output.stdout, &output.stderr).contains("index.lock"));
}

#[test]
fn test_run_git_command_with_index_lock_retry_returns_command_error_without_sleeping() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let mut sleeper = MockSleeper::new();
    let repo_path = Path::new(".");
    let args = ["rebase", "main"];
    let environment: [(&str, &str); 0] = [];

    command_runner
        .expect_run_git_command_output_with_env()
        .times(1)
        .return_once(|_, _, _| {
            Err(GitError::CommandFailed {
                command: "git".to_string(),
                stderr: "git execution failed".to_string(),
            })
        });
    sleeper.expect_sleep().times(0);

    // Act
    let error = run_git_command_with_index_lock_retry_with_dependencies(
        repo_path,
        &args,
        &environment,
        &command_runner,
        &sleeper,
    )
    .expect_err("retry helper should surface command execution errors");

    // Assert
    assert_eq!(error.to_string(), "git: git execution failed");
}

#[test]
fn test_run_git_command_with_index_lock_retry_does_not_sleep_for_non_lock_errors() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let mut sleeper = MockSleeper::new();
    let repo_path = Path::new(".");
    let args = ["rebase", "main"];
    let environment: [(&str, &str); 0] = [];

    command_runner
        .expect_run_git_command_output_with_env()
        .times(1)
        .returning(|_, _, _| Ok(non_lock_failure_output()));
    sleeper.expect_sleep().times(0);

    // Act
    let output = run_git_command_with_index_lock_retry_with_dependencies(
        repo_path,
        &args,
        &environment,
        &command_runner,
        &sleeper,
    )
    .expect("retry helper should return command output");

    // Assert
    assert!(!output.status.success());
}

#[test]
fn test_is_rebase_conflict_matches_unmerged_files_message() {
    // Arrange
    let detail = "Committing is not possible because you have unmerged files.";

    // Act
    let is_conflict = is_rebase_conflict(detail);

    // Assert
    assert!(is_conflict);
}

#[test]
fn abort_rebase_succeeds_through_injected_boundaries() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let metadata_cleaner = MockRebaseMetadataCleaner::new();
    let mut sleeper = MockSleeper::new();
    command_runner
        .expect_run_git_command_output_with_env()
        .withf(|repo_path, args, environment| {
            repo_path == Path::new("session-worktree")
                && args == ["rebase", "--abort"]
                && environment.is_empty()
        })
        .once()
        .returning(|_, _, _| Ok(success_output()));
    sleeper.expect_sleep().times(0);

    // Act
    let result = abort_rebase_with_dependencies(
        Path::new("session-worktree"),
        &command_runner,
        &sleeper,
        &metadata_cleaner,
    );

    // Assert
    assert!(result.is_ok());
}

#[test]
fn abort_rebase_preserves_command_runner_error() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let metadata_cleaner = MockRebaseMetadataCleaner::new();
    let mut sleeper = MockSleeper::new();
    command_runner
        .expect_run_git_command_output_with_env()
        .once()
        .return_once(|_, _, _| {
            Err(GitError::CommandFailed {
                command: "git rebase --abort".to_string(),
                stderr: "failed to spawn git".to_string(),
            })
        });
    sleeper.expect_sleep().times(0);

    // Act
    let error = abort_rebase_with_dependencies(
        Path::new("session-worktree"),
        &command_runner,
        &sleeper,
        &metadata_cleaner,
    )
    .expect_err("command runner failure should be preserved");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git rebase --abort" && stderr == "failed to spawn git"
    ));
}

#[test]
fn abort_rebase_preserves_actionable_command_failure() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let metadata_cleaner = MockRebaseMetadataCleaner::new();
    let mut sleeper = MockSleeper::new();
    command_runner
        .expect_run_git_command_output_with_env()
        .once()
        .returning(|_, _, _| {
            let mut output = non_lock_failure_output();
            output.stderr = b"fatal: cannot open .git/rebase-merge/head-name".to_vec();

            Ok(output)
        });
    sleeper.expect_sleep().times(0);

    // Act
    let error = abort_rebase_with_dependencies(
        Path::new("session-worktree"),
        &command_runner,
        &sleeper,
        &metadata_cleaner,
    )
    .expect_err("failed abort should preserve the git error");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git rebase --abort"
                && stderr.contains(".git/rebase-merge/head-name")
    ));
}

#[test]
fn abort_rebase_recovers_when_stale_metadata_is_removed() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let git_dir = temp_dir.path().join(".git");
    let stale_metadata = git_dir.join("rebase-merge");
    fs::create_dir(&git_dir).expect("git dir should be created");
    fs::create_dir(&stale_metadata).expect("stale metadata should be created");
    let mut command_runner = MockGitCommandRunner::new();
    let metadata_cleaner = FilesystemRebaseMetadataCleaner;
    let mut sleeper = MockSleeper::new();
    command_runner
        .expect_run_git_command_output_with_env()
        .once()
        .returning(|_, _, _| Ok(stale_rebase_failure_output()));
    sleeper.expect_sleep().times(0);

    // Act
    let result = abort_rebase_with_dependencies(
        temp_dir.path(),
        &command_runner,
        &sleeper,
        &metadata_cleaner,
    );

    // Assert
    assert!(result.is_ok());
    assert!(!stale_metadata.exists());
}

#[test]
fn abort_rebase_preserves_stale_error_when_no_metadata_is_removed() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let mut metadata_cleaner = MockRebaseMetadataCleaner::new();
    let mut sleeper = MockSleeper::new();
    command_runner
        .expect_run_git_command_output_with_env()
        .once()
        .returning(|_, _, _| Ok(stale_rebase_failure_output()));
    metadata_cleaner
        .expect_clean_stale_metadata()
        .once()
        .returning(|_| Ok(false));
    sleeper.expect_sleep().times(0);

    // Act
    let error = abort_rebase_with_dependencies(
        Path::new("session-worktree"),
        &command_runner,
        &sleeper,
        &metadata_cleaner,
    )
    .expect_err("missing stale metadata should preserve the abort failure");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git rebase --abort" && stderr.contains("No rebase in progress")
    ));
}

#[test]
fn abort_rebase_appends_stale_metadata_cleanup_failure() {
    // Arrange
    let mut command_runner = MockGitCommandRunner::new();
    let mut metadata_cleaner = MockRebaseMetadataCleaner::new();
    let mut sleeper = MockSleeper::new();
    command_runner
        .expect_run_git_command_output_with_env()
        .once()
        .returning(|_, _, _| Ok(stale_rebase_failure_output()));
    metadata_cleaner
        .expect_clean_stale_metadata()
        .once()
        .returning(|_| {
            Err(GitError::Io(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "metadata is read-only",
            )))
        });
    sleeper.expect_sleep().times(0);

    // Act
    let error = abort_rebase_with_dependencies(
        Path::new("session-worktree"),
        &command_runner,
        &sleeper,
        &metadata_cleaner,
    )
    .expect_err("cleanup failure should preserve both error contexts");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git rebase --abort"
                && stderr.contains("No rebase in progress")
                && stderr.contains("metadata is read-only")
    ));
}

#[test]
fn stale_rebase_error_detection_matches_only_known_diagnostics() {
    // Arrange
    let stale_diagnostics = [
        "fatal: No rebase in progress?",
        "fatal: It seems that there is already a rebase-merge directory",
        "fatal: It seems that there is already a rebase-apply directory",
        "fatal: It seems that I cannot tell whether you are in the middle of another rebase",
    ];

    // Act
    let stale_results = stale_diagnostics.map(is_stale_or_inactive_rebase_error);
    let unrelated_result =
        is_stale_or_inactive_rebase_error("fatal: cannot read rebase-merge/head-name");

    // Assert
    assert!(stale_results.into_iter().all(|is_stale| is_stale));
    assert!(!unrelated_result);
}

#[test]
fn filesystem_metadata_cleaner_removes_exact_rebase_entries() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let git_dir = temp_dir.path().join(".git");
    let rebase_merge = git_dir.join("rebase-merge");
    let rebase_apply = git_dir.join("rebase-apply");
    let unrelated_metadata = git_dir.join("MERGE_HEAD");
    fs::create_dir(&git_dir).expect("git dir should be created");
    fs::create_dir(&rebase_merge).expect("rebase-merge should be created");
    fs::write(rebase_merge.join("head-name"), "refs/heads/main")
        .expect("rebase-merge metadata should be written");
    fs::write(&rebase_apply, "apply state").expect("rebase-apply should be written");
    fs::write(&unrelated_metadata, "merge state").expect("merge metadata should be written");
    let metadata_cleaner = FilesystemRebaseMetadataCleaner;

    // Act
    let removed = metadata_cleaner
        .clean_stale_metadata(temp_dir.path())
        .expect("stale metadata cleanup should succeed");

    // Assert
    assert!(removed);
    assert!(!rebase_merge.exists());
    assert!(!rebase_apply.exists());
    assert!(unrelated_metadata.exists());
}

#[test]
fn filesystem_metadata_cleaner_reports_no_change_without_rebase_entries() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    fs::create_dir(temp_dir.path().join(".git")).expect("git dir should be created");
    let metadata_cleaner = FilesystemRebaseMetadataCleaner;

    // Act
    let removed = metadata_cleaner
        .clean_stale_metadata(temp_dir.path())
        .expect("empty metadata cleanup should succeed");

    // Assert
    assert!(!removed);
}

#[test]
fn filesystem_metadata_cleaner_rejects_missing_git_directory() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let metadata_cleaner = FilesystemRebaseMetadataCleaner;

    // Act
    let error = metadata_cleaner
        .clean_stale_metadata(temp_dir.path())
        .expect_err("repository without git metadata should fail");

    // Assert
    assert!(matches!(
        error,
        GitError::OutputParse(message) if message == "Failed to resolve git directory"
    ));
}

#[test]
fn remove_stale_metadata_preserves_non_not_found_io_error() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let parent_file = temp_dir.path().join("parent-file");
    fs::write(&parent_file, "not a directory").expect("parent file should be written");

    // Act
    let error = remove_stale_rebase_metadata_path(&parent_file.join("rebase-merge"))
        .expect_err("non-directory parent should remain an I/O error");

    // Assert
    assert!(matches!(error, GitError::Io(_)));
}

#[cfg(unix)]
#[test]
fn filesystem_metadata_cleaner_does_not_follow_directory_symlink() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let git_dir = temp_dir.path().join(".git");
    let external_dir = temp_dir.path().join("external-rebase-data");
    let external_marker = external_dir.join("marker");
    fs::create_dir(&git_dir).expect("git dir should be created");
    fs::create_dir(&external_dir).expect("external directory should be created");
    fs::write(&external_marker, "preserve").expect("external marker should be written");
    unix_fs::symlink(&external_dir, git_dir.join("rebase-merge"))
        .expect("metadata symlink should be created");
    let metadata_cleaner = FilesystemRebaseMetadataCleaner;

    // Act
    let removed = metadata_cleaner
        .clean_stale_metadata(temp_dir.path())
        .expect("symlink cleanup should succeed");

    // Assert
    assert!(removed);
    assert!(!git_dir.join("rebase-merge").exists());
    assert!(external_marker.exists());
}

#[test]
fn test_in_progress_operation_detects_rebase_metadata() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let git_dir = temp_dir.path().join(".git");
    fs::create_dir(&git_dir).expect("git dir should be created");
    fs::create_dir(git_dir.join("rebase-merge")).expect("rebase metadata should be created");

    // Act
    let operation =
        in_progress_operation_sync(temp_dir.path()).expect("operation should be detected");

    // Assert
    assert_eq!(operation, Some(InProgressGitOperation::Rebase));
}

#[test]
fn test_in_progress_operation_detects_merge_metadata() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let git_dir = temp_dir.path().join(".git");
    fs::create_dir(&git_dir).expect("git dir should be created");
    fs::write(git_dir.join("MERGE_HEAD"), "merge").expect("merge metadata should be created");

    // Act
    let operation =
        in_progress_operation_sync(temp_dir.path()).expect("operation should be detected");

    // Assert
    assert_eq!(operation, Some(InProgressGitOperation::Merge));
}

#[test]
fn test_in_progress_operation_detects_cherry_pick_metadata() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let git_dir = temp_dir.path().join(".git");
    fs::create_dir(&git_dir).expect("git dir should be created");
    fs::write(git_dir.join("CHERRY_PICK_HEAD"), "cherry-pick")
        .expect("cherry-pick metadata should be created");

    // Act
    let operation =
        in_progress_operation_sync(temp_dir.path()).expect("operation should be detected");

    // Assert
    assert_eq!(operation, Some(InProgressGitOperation::CherryPick));
}

#[test]
fn test_in_progress_operation_detects_revert_metadata() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    let git_dir = temp_dir.path().join(".git");
    fs::create_dir(&git_dir).expect("git dir should be created");
    fs::write(git_dir.join("REVERT_HEAD"), "revert").expect("revert metadata should be created");

    // Act
    let operation =
        in_progress_operation_sync(temp_dir.path()).expect("operation should be detected");

    // Assert
    assert_eq!(operation, Some(InProgressGitOperation::Revert));
}

#[test]
fn test_in_progress_operation_returns_none_for_clean_git_dir() {
    // Arrange
    let temp_dir = tempdir().expect("tempdir should be created");
    fs::create_dir(temp_dir.path().join(".git")).expect("git dir should be created");

    // Act
    let operation =
        in_progress_operation_sync(temp_dir.path()).expect("operation should be detected");

    // Assert
    assert_eq!(operation, None);
}

/// Returns a successful git command output.
fn success_output() -> Output {
    Command::new("git")
        .arg("--version")
        .output()
        .expect("failed to run git --version")
}

/// Returns a failing git command output that matches index lock contention.
fn git_index_lock_output() -> Output {
    let mut output = Command::new("git")
        .arg("definitely-invalid-subcommand")
        .output()
        .expect("failed to run git invalid command");
    output.stdout = vec![];
    output.stderr = b"fatal: Unable to create '.git/index.lock': File exists.".to_vec();

    output
}

/// Returns a failing git command output that is unrelated to index locking.
fn non_lock_failure_output() -> Output {
    let mut output = Command::new("git")
        .arg("definitely-invalid-subcommand")
        .output()
        .expect("failed to run git invalid command");
    output.stdout = vec![];
    output.stderr = b"fatal: not a git repository".to_vec();

    output
}

/// Returns a failing git command output for known stale rebase metadata.
fn stale_rebase_failure_output() -> Output {
    let mut output = non_lock_failure_output();
    output.stderr = b"fatal: No rebase in progress?".to_vec();

    output
}
