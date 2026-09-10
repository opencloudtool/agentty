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
    // A custom temporary directory may itself be inside a checkout.
    let non_repository_path = Path::new(std::path::MAIN_SEPARATOR_STR);

    // Act
    let result = find_git_repo_root_sync(non_repository_path);

    // Assert — walks up to filesystem root and returns None
    assert!(result.is_none());
}

#[test]
fn find_git_repo_root_sync_stops_at_invalid_git_file() {
    for contents in [
        "not a gitdir file\n",
        "gitdir: missing\n",
        "gitdir: ordinary-file\n",
    ] {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        fs::create_dir(temp_dir.path().join(".git")).expect("should create parent repo");
        let boundary = temp_dir.path().join("fixture");
        let nested = boundary.join("project");
        fs::create_dir_all(&nested).expect("should create nested fixture");
        fs::write(boundary.join(".git"), contents).expect("should write invalid git file");
        fs::write(boundary.join("ordinary-file"), "not a directory")
            .expect("should create non-directory target");

        // Act
        let result = find_git_repo_root_sync(&nested);

        // Assert
        assert_eq!(result, None, "invalid git file: {contents}");
    }
}

#[test]
fn find_git_repo_root_sync_accepts_relative_and_absolute_git_files() {
    for absolute_target in [false, true] {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        let metadata = temp_dir.path().join("metadata");
        let nested = temp_dir.path().join("src");
        fs::create_dir(&metadata).expect("should create metadata directory");
        fs::create_dir(&nested).expect("should create nested directory");
        fs::write(metadata.join("HEAD"), "ref: refs/heads/main\n").expect("should write HEAD");
        let target = if absolute_target {
            metadata
        } else {
            PathBuf::from("metadata")
        };
        fs::write(
            temp_dir.path().join(".git"),
            format!("gitdir: {}\n", target.display()),
        )
        .expect("should write git file");

        // Act
        let result = find_git_repo_root_sync(&nested);

        // Assert
        assert_eq!(result.as_deref(), Some(temp_dir.path()));
    }
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
    // A custom temporary directory may itself be inside a checkout.
    let non_repository_path = Path::new(std::path::MAIN_SEPARATOR_STR);

    // Act
    let result = detect_git_info_sync(non_repository_path);

    // Assert
    assert!(result.is_none());
}
