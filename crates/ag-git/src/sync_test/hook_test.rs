use super::*;

#[test]
fn ensure_pre_commit_hook_ready_allows_repositories_without_configuration() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());

    // Act
    let result = ensure_pre_commit_hook_ready(temp_dir.path());

    // Assert
    assert!(result.is_ok());
}

#[test]
fn ensure_pre_commit_hook_ready_rejects_missing_hook() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(
        temp_dir.path().join(".pre-commit-config.yaml"),
        "repos: []\n",
    )
    .expect("failed to write pre-commit configuration");

    // Act
    let result = ensure_pre_commit_hook_ready(temp_dir.path());

    // Assert
    assert!(matches!(
        result,
        Err(GitError::PreCommitHookMissing { ref config_file })
            if config_file == ".pre-commit-config.yaml"
    ));
}

#[cfg(unix)]
#[test]
fn ensure_pre_commit_hook_ready_accepts_default_executable_hook() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(
        temp_dir.path().join(".pre-commit-config.yaml"),
        "repos: []\n",
    )
    .expect("failed to write pre-commit configuration");
    let hook_path = temp_dir.path().join(git_command_stdout(
        temp_dir.path(),
        &["rev-parse", "--git-path", "hooks/pre-commit"],
    ));
    write_executable_pre_commit_hook(&hook_path);

    // Act
    let result = ensure_pre_commit_hook_ready(temp_dir.path());

    // Assert
    assert!(result.is_ok());
}

#[cfg(unix)]
#[test]
fn ensure_pre_commit_hook_ready_accepts_custom_executable_hook() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(
        temp_dir.path().join(".pre-commit-config.yaml"),
        "repos: []\n",
    )
    .expect("failed to write pre-commit configuration");
    run_git_command(
        temp_dir.path(),
        &["config", "core.hooksPath", ".custom-hooks"],
    );
    write_executable_pre_commit_hook(&temp_dir.path().join(".custom-hooks").join("pre-commit"));

    // Act
    let result = ensure_pre_commit_hook_ready(temp_dir.path());

    // Assert
    assert!(result.is_ok());
}

#[cfg(unix)]
#[test]
fn ensure_pre_commit_hook_ready_rejects_hook_inaccessible_to_owner() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(
        temp_dir.path().join(".pre-commit-config.yaml"),
        "repos: []\n",
    )
    .expect("failed to write pre-commit configuration");
    let hook_path = temp_dir.path().join(git_command_stdout(
        temp_dir.path(),
        &["rev-parse", "--git-path", "hooks/pre-commit"],
    ));
    fs::write(&hook_path, "#!/bin/sh\nexit 0\n").expect("failed to write pre-commit hook");
    fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o010))
        .expect("failed to set mismatched execute permissions");

    // Act
    let result = ensure_pre_commit_hook_ready(temp_dir.path());

    // Assert
    assert!(matches!(result, Err(GitError::PreCommitHookMissing { .. })));
}

#[tokio::test]
async fn run_pre_commit_hook_accepts_missing_hook() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());

    // Act
    let result = run_pre_commit_hook(temp_dir.path().to_path_buf()).await;

    // Assert
    assert!(result.is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn run_pre_commit_hook_uses_effective_custom_hook() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(
        temp_dir.path(),
        &["config", "core.hooksPath", ".custom-hooks"],
    );
    write_executable_hook(
        &temp_dir.path().join(".custom-hooks").join("pre-commit"),
        "#!/bin/sh\nprintf 'ran\\n' > pre-commit-ran\n",
    );

    // Act
    let result = run_pre_commit_hook(temp_dir.path().to_path_buf()).await;

    // Assert
    assert!(result.is_ok());
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("pre-commit-ran"))
            .expect("pre-commit marker should exist"),
        "ran\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_pre_commit_hook_returns_hook_failure_output() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    let hook_path = temp_dir.path().join(git_command_stdout(
        temp_dir.path(),
        &["rev-parse", "--git-path", "hooks/pre-commit"],
    ));
    write_executable_hook(
        &hook_path,
        "#!/bin/sh\nprintf 'resolved conflict rejected\\n' >&2\nexit 1\n",
    );

    // Act
    let result = run_pre_commit_hook(temp_dir.path().to_path_buf()).await;

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed {
            ref command,
            ref stderr,
        }) if command == "git hook run pre-commit"
            && stderr.contains("resolved conflict rejected")
    ));
}

#[test]
fn pre_commit_hook_result_preserves_command_launch_error() {
    // Arrange
    let command_error = GitError::CommandFailed {
        command: "git hook run pre-commit".to_string(),
        stderr: "git executable unavailable".to_string(),
    };

    // Act
    let result = pre_commit_hook_result(Err(command_error));

    // Assert
    assert!(matches!(
        result,
        Err(GitError::CommandFailed { ref stderr, .. })
            if stderr == "git executable unavailable"
    ));
}

#[tokio::test]
async fn commit_all_allows_configured_validation_without_hook() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    fs::write(
        temp_dir.path().join(".pre-commit-config.yaml"),
        "repos: []\n",
    )
    .expect("failed to write pre-commit configuration");
    fs::write(temp_dir.path().join("README.md"), "changed\n")
        .expect("failed to write worktree change");

    // Act
    let result = commit_all(temp_dir.path().to_path_buf(), "Change README".to_string()).await;

    // Assert
    assert!(result.is_ok());
    assert_eq!(
        git_command_stdout(temp_dir.path(), &["log", "-1", "--pretty=%s"]),
        "Change README"
    );
}
