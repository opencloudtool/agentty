use tempfile::tempdir;

use super::*;

#[test]
fn test_apply_non_interactive_environment_sets_git_prompt_controls() {
    // Arrange
    let mut command = Command::new("git");

    // Act
    apply_non_interactive_environment(&mut command);

    // Assert
    let env_pairs: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|resolved_value| {
                (
                    key.to_string_lossy().to_string(),
                    resolved_value.to_string_lossy().to_string(),
                )
            })
        })
        .collect();
    assert!(
        env_pairs
            .iter()
            .any(|(key, value)| key == "GIT_TERMINAL_PROMPT" && value == "0")
    );
    assert!(
        env_pairs
            .iter()
            .any(|(key, value)| key == "GCM_INTERACTIVE" && value == "never")
    );
    assert!(
        env_pairs
            .iter()
            .any(|(key, value)| key == "GIT_SSH_COMMAND" && value.contains("BatchMode=yes"))
    );
}

#[tokio::test]
async fn test_async_git_command_timeout_cancels_process() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temporary repository");
    let init_output = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(temp_dir.path())
        .output()
        .expect("failed to initialize temporary repository");
    assert!(init_output.status.success());
    let timeout = Duration::from_millis(25);
    let command = AsyncGitCommand::new(
        temp_dir.path().to_path_buf(),
        vec![
            "-c".to_string(),
            "alias.agentty-hang=!exec sleep 1".to_string(),
            "agentty-hang".to_string(),
        ],
    );

    // Act
    let error = run_git_command_with_timeout(command, timeout)
        .await
        .expect_err("long-running git command should time out");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandTimedOut {
            ref command,
            timeout: actual_timeout,
        } if command == "git -c alias.agentty-hang=!exec sleep 1 agentty-hang"
            && actual_timeout == timeout
    ));
}

#[test]
fn test_command_output_detail_prefers_stderr_then_stdout_then_unknown() {
    // Arrange

    // Act
    let stderr_detail = command_output_detail(b"stdout detail", b"stderr detail");
    let stdout_detail = command_output_detail(b"stdout detail", b"");
    let unknown_detail = command_output_detail(b"", b"");

    // Assert
    assert_eq!(stderr_detail, "stderr detail");
    assert_eq!(stdout_detail, "stdout detail");
    assert_eq!(unknown_detail, "Unknown git error");
}

#[test]
fn test_normalize_repo_url_converts_supported_github_formats() {
    // Arrange

    // Act
    let ssh_short = normalize_repo_url("git@github.com:agentty-xyz/agentty.git");
    let ssh_long = normalize_repo_url("ssh://git@github.com/agentty-xyz/agentty.git");
    let passthrough = normalize_repo_url("https://example.com/agentty-xyz/agentty.git");

    // Assert
    assert_eq!(ssh_short, "https://github.com/agentty-xyz/agentty");
    assert_eq!(ssh_long, "https://github.com/agentty-xyz/agentty");
    assert_eq!(passthrough, "https://example.com/agentty-xyz/agentty");
}

#[test]
fn test_resolve_git_dir_supports_directories_and_gitdir_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let repo_with_directory = temp_dir.path().join("repo-directory");
    let repo_with_absolute_file = temp_dir.path().join("repo-absolute");
    let repo_with_relative_file = temp_dir.path().join("repo-relative");
    let relative_git_dir = repo_with_relative_file.join(".actual-git");
    let malformed_repo = temp_dir.path().join("repo-malformed");
    fs::create_dir_all(repo_with_directory.join(".git"))
        .expect("failed to create .git directory repo");
    fs::create_dir_all(&repo_with_absolute_file).expect("failed to create absolute repo");
    fs::create_dir_all(&repo_with_relative_file).expect("failed to create relative repo");
    fs::create_dir_all(&relative_git_dir).expect("failed to create relative git dir");
    fs::create_dir_all(&malformed_repo).expect("failed to create malformed repo");
    fs::write(
        repo_with_absolute_file.join(".git"),
        format!("gitdir: {}", temp_dir.path().join("absolute-git").display()),
    )
    .expect("failed to write absolute gitdir file");
    fs::write(
        repo_with_relative_file.join(".git"),
        "gitdir: .actual-git\n",
    )
    .expect("failed to write relative gitdir file");
    fs::write(malformed_repo.join(".git"), "not-a-gitdir-file")
        .expect("failed to write malformed gitdir file");

    // Act
    let directory_git_dir = resolve_git_dir(&repo_with_directory);
    let absolute_git_dir = resolve_git_dir(&repo_with_absolute_file);
    let relative_git_dir_resolved = resolve_git_dir(&repo_with_relative_file);
    let malformed_git_dir = resolve_git_dir(&malformed_repo);

    // Assert
    assert_eq!(directory_git_dir, Some(repo_with_directory.join(".git")));
    assert_eq!(absolute_git_dir, Some(temp_dir.path().join("absolute-git")));
    assert_eq!(relative_git_dir_resolved, Some(relative_git_dir));
    assert_eq!(malformed_git_dir, None);
}

#[test]
fn test_run_git_command_sync_returns_command_failed_on_invalid_subcommand() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = run_git_command_sync(
        temp_dir.path(),
        &["definitely-not-a-git-subcommand"],
        "Git command failed",
    );

    // Assert
    let error = result.expect_err("invalid git command should fail");
    assert!(
        matches!(&error, GitError::CommandFailed { command, stderr }
                if command == "git definitely-not-a-git-subcommand"
                    && stderr.contains("Git command failed")),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn repo_root_from_git_dir_async_falls_back_to_git_toplevel() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    run_setup_git(temp_dir.path(), &["init", "--quiet"]);
    let nonstandard_git_dir = temp_dir.path().join("custom-admin");

    // Act
    let repo_root = repo_root_from_git_dir_async(temp_dir.path(), &nonstandard_git_dir)
        .await
        .expect("repository root fallback should succeed");

    // Assert
    assert_eq!(
        repo_root,
        fs::canonicalize(temp_dir.path()).expect("repository root should canonicalize")
    );
}

#[tokio::test]
async fn test_main_repo_root_returns_command_failed_outside_git_repository() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");

    // Act
    let result = main_repo_root(temp_dir.path().to_path_buf()).await;

    // Assert
    let error = result.expect_err("non-repo should fail");
    assert!(
        matches!(&error, GitError::CommandFailed { command, stderr }
                if command.starts_with("git rev-parse")
                    && stderr.contains("Git rev-parse failed")),
        "unexpected error: {error:?}"
    );
}

/// Runs a setup git command in `cwd`, asserting success and returning
/// trimmed stdout.
fn run_setup_git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run setup git command");
    assert!(
        output.status.success(),
        "git command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[tokio::test]
async fn test_bare_layout_resolves_bare_admin_root_and_no_working_checkout() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let root = temp_dir.path();
    let bare_dir = root.join(".bare");
    run_setup_git(root, &["init", "--bare", ".bare"]);
    run_setup_git(&bare_dir, &["config", "user.name", "Test User"]);
    run_setup_git(&bare_dir, &["config", "user.email", "test@example.com"]);
    let empty_tree = run_setup_git(&bare_dir, &["hash-object", "-w", "-t", "tree", "/dev/null"]);
    let commit = run_setup_git(&bare_dir, &["commit-tree", &empty_tree, "-m", "init"]);
    run_setup_git(&bare_dir, &["update-ref", "refs/heads/main", &commit]);
    run_setup_git(&bare_dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let main_worktree = root.join("main");
    let session_worktree = root.join("session");
    run_setup_git(
        &bare_dir,
        &[
            "worktree",
            "add",
            main_worktree.to_str().expect("main worktree path is utf-8"),
            "main",
        ],
    );
    run_setup_git(
        &bare_dir,
        &[
            "worktree",
            "add",
            "-b",
            "session",
            session_worktree
                .to_str()
                .expect("session worktree path is utf-8"),
            "main",
        ],
    );

    // Act
    let admin_root = main_repo_root(session_worktree.clone())
        .await
        .expect("failed to resolve admin root");
    let working_checkout = main_checkout_working_tree_sync(&session_worktree)
        .expect("failed to resolve working checkout");

    // Assert
    assert_eq!(
        admin_root,
        fs::canonicalize(&bare_dir).expect("failed to canonicalize bare dir")
    );
    assert_eq!(working_checkout, None);
}

#[test]
fn test_non_bare_layout_resolves_main_working_checkout_for_linked_worktree() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let root = temp_dir.path();
    let main_checkout = root.join("main");
    fs::create_dir_all(&main_checkout).expect("failed to create main checkout dir");
    run_setup_git(&main_checkout, &["init", "-b", "main"]);
    run_setup_git(&main_checkout, &["config", "user.name", "Test User"]);
    run_setup_git(
        &main_checkout,
        &["config", "user.email", "test@example.com"],
    );
    fs::write(main_checkout.join("README.md"), "test repo").expect("failed to write file");
    run_setup_git(&main_checkout, &["add", "README.md"]);
    run_setup_git(&main_checkout, &["commit", "-m", "Initial commit"]);
    let session_worktree = root.join("session");
    run_setup_git(
        &main_checkout,
        &[
            "worktree",
            "add",
            "-b",
            "session",
            session_worktree
                .to_str()
                .expect("session worktree path is utf-8"),
            "main",
        ],
    );

    // Act
    let working_checkout = main_checkout_working_tree_sync(&session_worktree)
        .expect("failed to resolve working checkout");

    // Assert
    assert_eq!(
        working_checkout,
        Some(fs::canonicalize(&main_checkout).expect("failed to canonicalize main checkout"))
    );
}

#[test]
fn test_format_git_invocation_returns_bare_git_for_empty_args() {
    // Arrange / Act / Assert
    assert_eq!(format_git_invocation(&[]), "git");
}

#[test]
fn test_format_git_invocation_joins_args_after_git() {
    // Arrange / Act / Assert
    assert_eq!(
        format_git_invocation(&["diff", "--cached", "--quiet"]),
        "git diff --cached --quiet"
    );
}
