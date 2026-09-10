use super::*;

#[tokio::test]
async fn read_worktree_file_returns_text_for_safe_nested_path() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let docs_dir = temp_dir.path().join("docs");
    fs::create_dir(&docs_dir).expect("failed to create docs directory");
    fs::write(docs_dir.join("README.md"), "# Preview\n").expect("failed to write markdown file");

    // Act
    let result =
        read_worktree_file(temp_dir.path().to_path_buf(), "docs/README.md".to_string()).await;

    // Assert
    assert_eq!(
        result.expect("worktree read should succeed"),
        WorktreeFileContent::Text("# Preview\n".to_string())
    );
}

#[tokio::test]
async fn read_worktree_file_classifies_missing_binary_and_oversize_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    fs::write(temp_dir.path().join("binary.md"), [0xff, 0xfe])
        .expect("failed to write binary file");
    fs::write(
        temp_dir.path().join("large.md"),
        vec![b'a'; MAX_WORKTREE_FILE_BYTE_COUNT + 1],
    )
    .expect("failed to write oversize file");

    // Act
    let missing = read_worktree_file(temp_dir.path().to_path_buf(), "missing.md".to_string()).await;
    let binary = read_worktree_file(temp_dir.path().to_path_buf(), "binary.md".to_string()).await;
    let too_large = read_worktree_file(temp_dir.path().to_path_buf(), "large.md".to_string()).await;

    // Assert
    assert_eq!(
        missing.expect("missing read should succeed"),
        WorktreeFileContent::Missing
    );
    assert_eq!(
        binary.expect("binary read should succeed"),
        WorktreeFileContent::Binary
    );
    assert_eq!(
        too_large.expect("oversize read should succeed"),
        WorktreeFileContent::TooLarge
    );
}

#[tokio::test]
async fn read_worktree_file_rejects_unsafe_relative_paths() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let absolute_path = temp_dir.path().join("README.md");

    // Act
    let empty = read_worktree_file(temp_dir.path().to_path_buf(), String::new()).await;
    let parent =
        read_worktree_file(temp_dir.path().to_path_buf(), "../README.md".to_string()).await;
    let absolute = read_worktree_file(
        temp_dir.path().to_path_buf(),
        absolute_path.to_string_lossy().into_owned(),
    )
    .await;

    // Assert
    for result in [empty, parent, absolute] {
        assert!(
            matches!(result, Err(GitError::OutputParse(message)) if message.contains("Unsafe worktree file path"))
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn read_worktree_file_rejects_symlinks_outside_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let outside_dir = tempdir().expect("failed to create outside temp dir");
    let outside_file = outside_dir.path().join("outside.md");
    fs::write(&outside_file, "outside").expect("failed to write outside file");
    std::os::unix::fs::symlink(&outside_file, temp_dir.path().join("link.md"))
        .expect("failed to create outside symlink");

    // Act
    let result = read_worktree_file(temp_dir.path().to_path_buf(), "link.md".to_string()).await;

    // Assert
    assert!(
        matches!(result, Err(GitError::OutputParse(message)) if message.contains("resolves outside repository"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_worktree_file_maps_non_missing_path_resolution_errors() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    std::os::unix::fs::symlink("loop.md", temp_dir.path().join("loop.md"))
        .expect("failed to create symlink loop");

    // Act
    let result = read_worktree_file(temp_dir.path().to_path_buf(), "loop.md".to_string()).await;

    // Assert
    assert!(matches!(result, Err(GitError::Io(_))));
}

#[test]
fn copy_git_index_to_temp_maps_path_create_and_copy_failures() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let path_without_parent = Path::new("/");
    let missing_parent_index = temp_dir.path().join("missing-parent").join("index");
    let missing_index = temp_dir.path().join("missing-index");

    // Act
    let parent_error = copy_git_index_to_temp(path_without_parent);
    let create_error = copy_git_index_to_temp(&missing_parent_index);
    let copy_error = copy_git_index_to_temp(&missing_index);

    // Assert
    assert!(matches!(parent_error, Err(GitError::OutputParse(_))));
    assert!(matches!(
        create_error,
        Err(GitError::CommandFailed { ref command, .. })
            if command == "create temporary git index"
    ));
    assert!(matches!(
        copy_error,
        Err(GitError::CommandFailed { ref command, .. }) if command == "copy git index"
    ));
}

#[test]
fn run_git_command_with_index_sync_maps_process_and_command_failures() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let index_path = temp_dir.path().join("index");
    let missing_repo_path = temp_dir.path().join("missing-repository");
    fs::write(&index_path, []).expect("failed to create temporary index");

    // Act
    let process_error = run_git_command_with_index_sync(
        &missing_repo_path,
        &["status"],
        &index_path,
        "Expected process failure",
    );
    let command_error = run_git_command_with_index_sync(
        temp_dir.path(),
        &["definitely-not-a-git-command"],
        &index_path,
        "Expected git failure",
    );

    // Assert
    assert!(matches!(
        process_error,
        Err(GitError::CommandFailed { ref command, .. }) if command == "git status"
    ));
    assert!(matches!(
        command_error,
        Err(GitError::CommandFailed {
            ref command,
            ref stderr,
        }) if command == "git definitely-not-a-git-command"
            && stderr.starts_with("Expected git failure:")
    ));
}
