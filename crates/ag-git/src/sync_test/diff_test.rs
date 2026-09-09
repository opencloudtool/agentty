use super::*;

#[tokio::test]
async fn delete_branch_removes_branch_from_isolated_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(temp_dir.path(), &["branch", "review/topic"]);

    // Act
    delete_branch(temp_dir.path().to_path_buf(), "review/topic".to_string())
        .await
        .expect("branch deletion should succeed");

    // Assert
    let branch_lookup = git_command_output(
        temp_dir.path(),
        &["show-ref", "--verify", "--quiet", "refs/heads/review/topic"],
    );
    assert!(!branch_lookup.status.success());
}

#[tokio::test]
async fn diff_preserves_staged_changes_and_includes_untracked_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(temp_dir.path().join("README.md"), "staged change\n")
        .expect("failed to write staged change");
    run_git_command(temp_dir.path(), &["add", "README.md"]);
    fs::write(
        temp_dir.path().join("README.md"),
        "staged change\nunstaged change\n",
    )
    .expect("failed to write unstaged change");
    fs::write(temp_dir.path().join("new.txt"), "untracked change\n")
        .expect("failed to write untracked file");
    let cached_diff_before = git_command_output(temp_dir.path(), &["diff", "--cached"]).stdout;
    let status_before = git_command_output(
        temp_dir.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .stdout;

    // Act
    let result = diff(temp_dir.path().to_path_buf(), "main".to_string()).await;
    let changed_files = diff_changed_files(temp_dir.path().to_path_buf(), "main".to_string()).await;

    // Assert
    let diff_output = result.expect("diff should succeed");
    let cached_diff_after = git_command_output(temp_dir.path(), &["diff", "--cached"]).stdout;
    let status_after = git_command_output(
        temp_dir.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .stdout;
    assert!(diff_output.contains("staged change"));
    assert!(diff_output.contains("unstaged change"));
    assert!(diff_output.contains("untracked change"));
    assert_eq!(
        changed_files.expect("changed files should load"),
        vec!["README.md".to_string(), "new.txt".to_string()]
    );
    assert_eq!(cached_diff_after, cached_diff_before);
    assert_eq!(status_after, status_before);
}

#[tokio::test]
async fn diff_reports_repository_unavailable_outside_git_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = diff(temp_dir.path().to_path_buf(), "main".to_string()).await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::RepositoryUnavailable { detail })
            if detail.to_ascii_lowercase().contains("not a git repository")
    ));
}

#[test]
fn diff_reports_repository_unavailable_when_removed_after_index_resolution() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let preserved_index_dir = tempdir().expect("failed to create preserved index dir");
    setup_test_git_repo(temp_dir.path());
    let index_path = resolve_diff_index_path(temp_dir.path()).expect("index path should resolve");
    let index_path = PathBuf::from(index_path.trim());
    let index_path = temp_dir.path().join(index_path);
    let preserved_index_path = preserved_index_dir.path().join("index");
    fs::copy(index_path, &preserved_index_path).expect("index copy should succeed");
    fs::remove_dir_all(temp_dir.path()).expect("worktree removal should succeed");

    // Act
    let result =
        diff_output_after_index_resolution(temp_dir.path(), "main", false, &preserved_index_path);

    // Assert
    assert!(matches!(
        result,
        Err(GitError::RepositoryUnavailable { detail })
            if detail.contains("git add -A --intent-to-add")
    ));
}

#[tokio::test]
async fn diff_preserves_invalid_base_reference_error() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());

    // Act
    let result = diff(
        temp_dir.path().to_path_buf(),
        "missing-base-reference".to_string(),
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { command, stderr })
            if command == "git diff missing-base-reference"
                && stderr.contains("Git diff failed")
    ));
}

#[test]
fn diff_repository_error_classification_preserves_unrelated_failures() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    let error = GitError::CommandFailed {
        command: "git rev-parse --git-path index".to_string(),
        stderr: "fatal: ambiguous argument".to_string(),
    };

    // Act
    let classified = classify_diff_repository_error(temp_dir.path(), error);

    // Assert
    assert!(matches!(
        classified,
        GitError::CommandFailed { command, stderr }
            if command == "git rev-parse --git-path index"
                && stderr == "fatal: ambiguous argument"
    ));
}

#[test]
fn diff_repository_error_classification_ignores_localized_diagnostic() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let error = GitError::CommandFailed {
        command: "git rev-parse --git-path index".to_string(),
        stderr: "fatal: kein Git-Repository".to_string(),
    };

    // Act
    let classified = classify_diff_repository_error(temp_dir.path(), error);

    // Assert
    assert!(matches!(
        classified,
        GitError::RepositoryUnavailable { detail }
            if detail == "git rev-parse --git-path index: fatal: kein Git-Repository"
    ));
}

#[test]
fn diff_repository_probe_preserves_spawn_failure() {
    // Arrange
    let probe = Err(GitError::CommandFailed {
        command: "git rev-parse --git-dir".to_string(),
        stderr: "git executable unavailable".to_string(),
    });

    // Act
    let unavailable = diff_repository_probe_is_unavailable(probe);

    // Assert
    assert!(!unavailable);
}

#[test]
fn diff_repository_error_classification_types_missing_directory() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let missing_path = temp_dir.path().join("removed-worktree");
    let error = GitError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "worktree removed",
    ));

    // Act
    let classified = classify_diff_repository_error(&missing_path, error);

    // Assert
    assert!(matches!(
        classified,
        GitError::RepositoryUnavailable { detail } if detail == "worktree removed"
    ));
}
