use super::*;

#[tokio::test]
async fn current_branch_name_returns_error_for_detached_head() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(temp_dir.path(), &["checkout", "--detach"]);
    let command_runner = ProcessAsyncGitCommandRunner;

    // Act
    let result = current_branch_name(temp_dir.path(), &command_runner).await;

    // Assert
    let error = result.expect_err("detached HEAD should fail");
    assert!(error.to_string().contains("detached HEAD"));
}

#[tokio::test]
async fn current_branch_remote_name_returns_none_when_remote_is_not_configured() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    let command_runner = ProcessAsyncGitCommandRunner;

    // Act
    let remote_name = current_branch_remote_name(temp_dir.path(), &command_runner)
        .await
        .expect("missing branch remote should not be a command failure");

    // Assert
    assert_eq!(remote_name, None);
}

#[tokio::test]
async fn current_branch_remote_name_returns_configured_non_origin_remote() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(
        temp_dir.path(),
        &["config", "branch.main.remote", "review-remote"],
    );
    let command_runner = ProcessAsyncGitCommandRunner;

    // Act
    let remote_name = current_branch_remote_name(temp_dir.path(), &command_runner)
        .await
        .expect("configured branch remote should resolve");

    // Assert
    assert_eq!(remote_name, Some("review-remote".to_string()));
}

#[test]
fn parse_current_branch_remote_output_preserves_fatal_config_error() {
    // Arrange
    let output = AsyncGitCommandOutput {
        exit_code: Some(128),
        stderr: b"fatal: bad config line".to_vec(),
        stdout: Vec::new(),
    };

    // Act
    let error = parse_current_branch_remote_output(&output, "branch.main.remote")
        .expect_err("malformed config should remain an error");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git config --get branch.main.remote"
                && stderr.contains("Failed to resolve current branch remote")
    ));
}

#[tokio::test]
async fn primary_upstream_reference_uses_first_non_empty_line() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
    run_git_command(
        temp_dir.path(),
        &[
            "config",
            "--replace-all",
            "branch.main.merge",
            "refs/heads/main",
        ],
    );
    run_git_command(
        temp_dir.path(),
        &["config", "--add", "branch.main.merge", "refs/heads/feature"],
    );
    let command_runner = ProcessAsyncGitCommandRunner;

    // Act
    let upstream_reference = primary_upstream_reference(temp_dir.path(), &command_runner)
        .await
        .expect("failed to resolve upstream");

    // Assert
    assert_eq!(upstream_reference, "origin/main");
}

#[tokio::test]
async fn pull_rebase_retries_index_lock_through_async_runner() {
    // Arrange
    let repo_path = PathBuf::from("test-repo");
    let mut command_runner = MockAsyncGitCommandRunner::new();
    let mut sequence = Sequence::new();
    command_runner
        .expect_run()
        .with(function(|command: &AsyncGitCommand| {
            command.arguments == ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]
        }))
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(|_| Box::pin(async { Ok(async_git_output(0, "origin/main\n", Vec::new())) }));
    for output in [
        async_git_output(
            128,
            Vec::new(),
            "fatal: Unable to create '.git/index.lock': File exists.",
        ),
        async_git_output(0, Vec::new(), Vec::new()),
    ] {
        command_runner
            .expect_run()
            .with(function(|command: &AsyncGitCommand| {
                command.arguments == ["pull", "--rebase", "origin", "main"]
                    && command.environment
                        == [
                            ("GIT_EDITOR".to_string(), ":".to_string()),
                            ("GIT_SEQUENCE_EDITOR".to_string(), ":".to_string()),
                        ]
            }))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_| Box::pin(async move { Ok(output) }));
    }

    // Act
    let result = pull_rebase_with_runner(repo_path, &command_runner, Duration::ZERO).await;

    // Assert
    assert!(matches!(result, Ok(PullRebaseResult::Completed)));
}

#[tokio::test]
async fn pull_rebase_preserves_non_conflict_command_failure() {
    // Arrange
    let repo_path = PathBuf::from("test-repo");
    let mut command_runner = MockAsyncGitCommandRunner::new();
    let mut sequence = Sequence::new();
    command_runner
        .expect_run()
        .with(function(|command: &AsyncGitCommand| {
            command.arguments == ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]
        }))
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(|_| Box::pin(async { Ok(async_git_output(0, "origin/main\n", Vec::new())) }));
    command_runner
        .expect_run()
        .with(function(|command: &AsyncGitCommand| {
            command.arguments == ["pull", "--rebase", "origin", "main"]
        }))
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(|_| {
            Box::pin(async { Ok(async_git_output(128, Vec::new(), "fatal: transport failed")) })
        });

    // Act
    let error = pull_rebase_with_runner(repo_path, &command_runner, Duration::ZERO)
        .await
        .expect_err("non-conflict pull failure should remain an error");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git pull --rebase" && stderr == "fatal: transport failed"
    ));
}

#[tokio::test]
async fn pull_rebase_rejects_local_upstream_without_configured_remote() {
    // Arrange
    let repo_path = PathBuf::from("test-repo");
    let mut command_runner = MockAsyncGitCommandRunner::new();
    let mut sequence = Sequence::new();
    let expectations = [
        (
            vec!["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            async_git_output(0, "main\n", Vec::new()),
        ),
        (
            vec!["rev-parse", "--abbrev-ref", "HEAD"],
            async_git_output(0, "main\n", Vec::new()),
        ),
        (
            vec!["config", "--get", "branch.main.remote"],
            async_git_output(1, Vec::new(), Vec::new()),
        ),
    ];
    for (arguments, output) in expectations {
        let arguments = arguments
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        command_runner
            .expect_run()
            .with(function(move |command: &AsyncGitCommand| {
                command.arguments == arguments
            }))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_| Box::pin(async move { Ok(output) }));
    }

    // Act
    let error = pull_rebase_with_runner(repo_path, &command_runner, Duration::ZERO)
        .await
        .expect_err("local upstream without a remote should fail");

    // Assert
    assert!(matches!(
        error,
        GitError::OutputParse(message)
            if message == "Failed to resolve current branch remote: not configured"
    ));
}

#[tokio::test]
async fn pull_rebase_returns_last_index_lock_failure_after_retry_exhaustion() {
    // Arrange
    let repo_path = PathBuf::from("test-repo");
    let mut command_runner = MockAsyncGitCommandRunner::new();
    let mut sequence = Sequence::new();
    command_runner
        .expect_run()
        .with(function(|command: &AsyncGitCommand| {
            command.arguments == ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]
        }))
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(|_| Box::pin(async { Ok(async_git_output(0, "origin/main\n", Vec::new())) }));
    command_runner
        .expect_run()
        .with(function(|command: &AsyncGitCommand| {
            command.arguments == ["pull", "--rebase", "origin", "main"]
        }))
        .times(GIT_INDEX_LOCK_RETRY_ATTEMPTS)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Box::pin(async {
                Ok(async_git_output(
                    128,
                    Vec::new(),
                    "fatal: Unable to create '.git/index.lock': File exists.",
                ))
            })
        });

    // Act
    let error = pull_rebase_with_runner(repo_path, &command_runner, Duration::ZERO)
        .await
        .expect_err("exhausted index-lock retries should return the last failure");

    // Assert
    assert!(matches!(
        error,
        GitError::CommandFailed { command, stderr }
            if command == "git pull --rebase" && stderr.contains("index.lock")
    ));
}

#[tokio::test]
async fn remote_branch_lookup_uses_origin_fallback_through_async_runner() {
    // Arrange
    let repo_path = PathBuf::from("test-repo");
    let mut command_runner = MockAsyncGitCommandRunner::new();
    let mut sequence = Sequence::new();
    let expectations = [
        (
            vec!["rev-parse", "--abbrev-ref", "HEAD"],
            async_git_output(0, "main\n", Vec::new()),
        ),
        (
            vec!["config", "--get", "branch.main.remote"],
            async_git_output(1, Vec::new(), Vec::new()),
        ),
        (
            vec!["ls-remote", "--heads", "origin", "review/topic"],
            async_git_output(0, "abc123\trefs/heads/review/topic\n", Vec::new()),
        ),
    ];
    for (arguments, output) in expectations {
        let arguments = arguments
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        command_runner
            .expect_run()
            .with(function(move |command: &AsyncGitCommand| {
                command.arguments == arguments
            }))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_| Box::pin(async move { Ok(output) }));
    }

    // Act
    let exists =
        remote_branch_exists_with_runner(repo_path, "review/topic".to_string(), &command_runner)
            .await
            .expect("remote branch lookup should succeed");

    // Assert
    assert!(exists);
}

#[tokio::test]
async fn new_remote_branch_push_requires_missing_remote_ref() {
    // Arrange
    let repo_path = PathBuf::from("test-repo");
    let mut command_runner = MockAsyncGitCommandRunner::new();
    let mut sequence = Sequence::new();
    let expectations = [
        (
            vec!["rev-parse", "--abbrev-ref", "HEAD"],
            async_git_output(0, "main\n", Vec::new()),
        ),
        (
            vec!["config", "--get", "branch.main.remote"],
            async_git_output(1, Vec::new(), Vec::new()),
        ),
        (
            vec![
                "push",
                "--force-with-lease=refs/heads/review/topic:",
                "--set-upstream",
                "origin",
                "HEAD:review/topic",
            ],
            async_git_output(0, Vec::new(), Vec::new()),
        ),
    ];
    for (arguments, output) in expectations {
        let arguments = arguments
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        command_runner
            .expect_run()
            .with(function(move |command: &AsyncGitCommand| {
                command.arguments == arguments
            }))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_| Box::pin(async move { Ok(output) }));
    }

    // Act
    let upstream_reference = push_current_branch_to_new_remote_branch_with_runner(
        repo_path,
        "review/topic".to_string(),
        &command_runner,
    )
    .await
    .expect("new remote branch push should succeed");

    // Assert
    assert_eq!(upstream_reference, "origin/review/topic");
}

#[tokio::test]
async fn remote_branch_lookup_checks_isolated_local_remote() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);

    // Act
    let exists = remote_branch_exists(temp_dir.path().to_path_buf(), "main".to_string())
        .await
        .expect("local remote branch lookup should succeed");

    // Assert
    assert!(exists);
}

#[tokio::test]
async fn push_without_upstream_reuses_configured_remote() {
    // Arrange
    let repo_path = PathBuf::from("test-repo");
    let mut command_runner = MockAsyncGitCommandRunner::new();
    let mut sequence = Sequence::new();
    let expectations = [
        (
            vec!["push", "--force-with-lease"],
            async_git_output(128, Vec::new(), "fatal: no upstream branch"),
        ),
        (
            vec!["rev-parse", "--abbrev-ref", "HEAD"],
            async_git_output(0, "main\n", Vec::new()),
        ),
        (
            vec!["config", "--get", "branch.main.remote"],
            async_git_output(0, "review-remote\n", Vec::new()),
        ),
        (
            vec![
                "push",
                "--force-with-lease",
                "--set-upstream",
                "review-remote",
                "HEAD",
            ],
            async_git_output(0, Vec::new(), Vec::new()),
        ),
        (
            vec!["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            async_git_output(0, "review-remote/main\n", Vec::new()),
        ),
    ];
    for (arguments, output) in expectations {
        let arguments = arguments
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        command_runner
            .expect_run()
            .with(function(move |command: &AsyncGitCommand| {
                command.arguments == arguments
            }))
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_| Box::pin(async move { Ok(output) }));
    }

    // Act
    let upstream_reference = push_current_branch_with_runner(repo_path, &command_runner)
        .await
        .expect("configured remote push should succeed");

    // Assert
    assert_eq!(upstream_reference, "review-remote/main");
}

#[test]
fn parse_branch_tracking_statuses_reads_repo_wide_branch_snapshot() {
    // Arrange
    let output = "\
main\torigin/main\tbehind 2\nwt/1234abcd\torigin/wt/1234abcd\tahead 3, behind \
                  1\nfeature/local\t\t\nfeature/gone\torigin/feature/gone\tgone\n";

    // Act
    let branch_tracking_statuses = parse_branch_tracking_statuses(output);

    // Assert
    assert_eq!(branch_tracking_statuses.get("main"), Some(&Some((0, 2))));
    assert_eq!(
        branch_tracking_statuses.get("wt/1234abcd"),
        Some(&Some((3, 1)))
    );
    assert_eq!(branch_tracking_statuses.get("feature/local"), Some(&None));
    assert_eq!(branch_tracking_statuses.get("feature/gone"), Some(&None));
}

#[tokio::test]
async fn pull_rebase_returns_conflict_detail_for_conflicting_remote_change() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    let contributor_dir = tempdir().expect("failed to create contributor temp dir");
    let contributor_clone_path = contributor_dir.path().join("clone");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    let contributor_clone_path_text = contributor_clone_path.to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
    fs::write(temp_dir.path().join("README.md"), "local change\n")
        .expect("failed to write local change");
    run_git_command(temp_dir.path(), &["add", "README.md"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Local change"]);
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
    fs::write(contributor_clone_path.join("README.md"), "remote change\n")
        .expect("failed to write remote change");
    run_git_command(&contributor_clone_path, &["add", "README.md"]);
    run_git_command(&contributor_clone_path, &["commit", "-m", "Remote change"]);
    run_git_command(&contributor_clone_path, &["push", "origin", "main"]);

    // Act
    let result = pull_rebase(temp_dir.path().to_path_buf()).await;

    // Assert
    assert!(matches!(
        result,
        Ok(PullRebaseResult::Conflict { ref detail })
            if {
                let normalized_detail = detail.to_ascii_lowercase();

                (normalized_detail.contains("conflict")
                    || normalized_detail.contains("could not apply"))
                    && !detail.is_empty()
            }
    ));
}

#[tokio::test]
async fn push_current_branch_returns_rejected_error_for_non_fast_forward_push() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    let contributor_dir = tempdir().expect("failed to create contributor temp dir");
    let contributor_clone_path = contributor_dir.path().join("clone");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    let contributor_clone_path_text = contributor_clone_path.to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
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
        .expect("failed to write remote file");
    run_git_command(&contributor_clone_path, &["add", "remote.txt"]);
    run_git_command(&contributor_clone_path, &["commit", "-m", "Remote change"]);
    run_git_command(&contributor_clone_path, &["push", "origin", "main"]);
    fs::write(temp_dir.path().join("local.txt"), "local change")
        .expect("failed to write local file");
    run_git_command(temp_dir.path(), &["add", "local.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Local change"]);

    // Act
    let result = push_current_branch(temp_dir.path().to_path_buf()).await;

    // Assert
    let error = result
        .expect_err("non-fast-forward push should fail")
        .to_string();
    assert!(error.contains("git push"));
    assert!(
        error.contains("stale info") || error.contains("rejected") || error.contains("fetch first")
    );
}

#[tokio::test]
async fn push_current_branch_force_with_lease_updates_rewritten_history() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
    fs::write(
        temp_dir.path().join("README.md"),
        "first published version\n",
    )
    .expect("failed to write first version");
    run_git_command(temp_dir.path(), &["add", "README.md"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Publish branch change"]);
    push_current_branch(temp_dir.path().to_path_buf())
        .await
        .expect("initial push should succeed");
    fs::write(
        temp_dir.path().join("README.md"),
        "rewritten published version\n",
    )
    .expect("failed to rewrite published version");
    run_git_command(temp_dir.path(), &["add", "README.md"]);
    run_git_command(
        temp_dir.path(),
        &["commit", "--amend", "-m", "Rewrite published branch change"],
    );

    // Act
    let upstream_reference = push_current_branch(temp_dir.path().to_path_buf())
        .await
        .expect("force-with-lease push should update rewritten history");
    let local_head = git_command_stdout(temp_dir.path(), &["rev-parse", "HEAD"]);
    let remote_head = git_command_stdout(remote_dir.path(), &["rev-parse", "refs/heads/main"]);

    // Assert
    assert_eq!(upstream_reference, "origin/main");
    assert_eq!(local_head, remote_head);
}

#[tokio::test]
async fn push_current_branch_to_remote_branch_returns_custom_upstream_reference() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);

    // Act
    let upstream_reference = push_current_branch_to_remote_branch(
        temp_dir.path().to_path_buf(),
        "review/custom-branch".to_string(),
    )
    .await
    .expect("failed to push current branch to custom remote branch");

    // Assert
    assert_eq!(upstream_reference, "origin/review/custom-branch");
}

#[tokio::test]
async fn new_remote_branch_push_rejects_existing_remote_branch() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(
        temp_dir.path(),
        &["push", "origin", "HEAD:review/existing-branch"],
    );
    let remote_head_before = git_command_stdout(
        remote_dir.path(),
        &["rev-parse", "refs/heads/review/existing-branch"],
    );
    fs::write(temp_dir.path().join("new.txt"), "new local review\n")
        .expect("failed to write new local review file");
    run_git_command(temp_dir.path(), &["add", "new.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "New local review"]);

    // Act
    let result = push_current_branch_to_new_remote_branch(
        temp_dir.path().to_path_buf(),
        "review/existing-branch".to_string(),
    )
    .await;
    let remote_head_after = git_command_stdout(
        remote_dir.path(),
        &["rev-parse", "refs/heads/review/existing-branch"],
    );

    // Assert
    let error = result.expect_err("existing remote branch should be rejected");
    assert!(error.to_string().contains("stale info"));
    assert_eq!(remote_head_before, remote_head_after);
}

#[tokio::test]
async fn new_remote_branch_push_ignores_stale_remote_tracking_ref() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["checkout", "-b", "previous-review"]);
    fs::write(temp_dir.path().join("previous.txt"), "previous review\n")
        .expect("failed to write previous review file");
    run_git_command(temp_dir.path(), &["add", "previous.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Previous review"]);
    let previous_head = git_command_stdout(temp_dir.path(), &["rev-parse", "HEAD"]);
    run_git_command(
        temp_dir.path(),
        &[
            "push",
            "--set-upstream",
            "origin",
            "HEAD:review/deleted-branch",
        ],
    );
    run_git_command(
        temp_dir.path(),
        &["push", "origin", ":review/deleted-branch"],
    );
    run_git_command(
        temp_dir.path(),
        &[
            "update-ref",
            "refs/remotes/origin/review/deleted-branch",
            &previous_head,
        ],
    );
    run_git_command(temp_dir.path(), &["checkout", "main"]);
    fs::write(
        temp_dir.path().join("replacement.txt"),
        "replacement review\n",
    )
    .expect("failed to write replacement review file");
    run_git_command(temp_dir.path(), &["add", "replacement.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Replacement review"]);

    // Act
    let upstream_reference = push_current_branch_to_new_remote_branch(
        temp_dir.path().to_path_buf(),
        "review/deleted-branch".to_string(),
    )
    .await
    .expect("new branch push should ignore the stale remote-tracking ref");
    let local_head = git_command_stdout(temp_dir.path(), &["rev-parse", "HEAD"]);
    let remote_head = git_command_stdout(
        remote_dir.path(),
        &["rev-parse", "refs/heads/review/deleted-branch"],
    );

    // Assert
    assert_eq!(upstream_reference, "origin/review/deleted-branch");
    assert_eq!(local_head, remote_head);
}

#[tokio::test]
async fn current_upstream_reference_returns_origin_main() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);

    // Act
    let upstream_reference = current_upstream_reference(temp_dir.path().to_path_buf())
        .await
        .expect("failed to resolve upstream reference");

    // Assert
    assert_eq!(upstream_reference, "origin/main");
}

#[tokio::test]
async fn get_ref_ahead_behind_returns_counts_between_two_local_branches() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(temp_dir.path(), &["checkout", "-b", "wt/1234abcd"]);
    fs::write(temp_dir.path().join("session.txt"), "session change\n")
        .expect("failed to write session file");
    run_git_command(temp_dir.path(), &["add", "session.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Session change"]);
    run_git_command(temp_dir.path(), &["checkout", "main"]);
    fs::write(temp_dir.path().join("main.txt"), "main change\n")
        .expect("failed to write main file");
    run_git_command(temp_dir.path(), &["add", "main.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Main change"]);

    // Act
    let status = get_ref_ahead_behind(
        temp_dir.path().to_path_buf(),
        "wt/1234abcd".to_string(),
        "main".to_string(),
    )
    .await
    .expect("failed to compare branch refs");

    // Assert
    assert_eq!(status, (1, 1));
}

#[tokio::test]
async fn branch_tracking_statuses_returns_repo_wide_branch_counts() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let remote_dir = tempdir().expect("failed to create remote temp dir");
    let contributor_dir = tempdir().expect("failed to create contributor temp dir");
    let contributor_clone_path = contributor_dir.path().join("clone");
    setup_test_git_repo(temp_dir.path());
    run_git_command(remote_dir.path(), &["init", "--bare"]);
    let remote_path = remote_dir.path().to_string_lossy().to_string();
    let contributor_clone_path_text = contributor_clone_path.to_string_lossy().to_string();
    run_git_command(temp_dir.path(), &["remote", "add", "origin", &remote_path]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "main"]);
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
        .expect("failed to write remote file");
    run_git_command(&contributor_clone_path, &["add", "remote.txt"]);
    run_git_command(&contributor_clone_path, &["commit", "-m", "Remote change"]);
    run_git_command(&contributor_clone_path, &["push", "origin", "main"]);
    run_git_command(temp_dir.path(), &["checkout", "-b", "wt/1234abcd"]);
    fs::write(temp_dir.path().join("session.txt"), "session change\n")
        .expect("failed to write session file");
    run_git_command(temp_dir.path(), &["add", "session.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Session change"]);
    run_git_command(temp_dir.path(), &["push", "-u", "origin", "wt/1234abcd"]);
    fs::write(
        temp_dir.path().join("session.txt"),
        "session change\nmore local\n",
    )
    .expect("failed to extend session file");
    run_git_command(temp_dir.path(), &["add", "session.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "More session work"]);
    run_git_command(temp_dir.path(), &["fetch"]);

    // Act
    let branch_tracking_statuses = branch_tracking_statuses(temp_dir.path().to_path_buf())
        .await
        .expect("failed to read branch tracking statuses");

    // Assert
    assert_eq!(branch_tracking_statuses.get("main"), Some(&Some((0, 1))));
    assert_eq!(
        branch_tracking_statuses.get("wt/1234abcd"),
        Some(&Some((1, 0)))
    );
}

#[tokio::test]
/// Verifies that amending a session commit whose staged result is identical
/// to the base branch (i.e., all changes were reverted) surfaces the
/// canonical "Nothing to commit" sentinel rather than triggering the assist
/// retry loop with the raw git "allow-empty" error.
async fn test_empty_amend_resets_session_commit_and_returns_no_changes() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    setup_test_git_repo(temp_dir.path());
    run_git_command(temp_dir.path(), &["checkout", "-b", "session-branch"]);
    fs::write(temp_dir.path().join("session.txt"), "session work\n")
        .expect("failed to write session file");
    run_git_command(temp_dir.path(), &["add", "session.txt"]);
    run_git_command(temp_dir.path(), &["commit", "-m", "Session commit"]);
    fs::remove_file(temp_dir.path().join("session.txt")).expect("failed to remove session file");

    // Act - the worktree is dirty (session.txt removed) but amending HEAD
    // would produce a tree identical to the base branch, making the amend
    // result an empty commit.
    let result = commit_all_preserving_single_commit(
        temp_dir.path().to_path_buf(),
        "main".to_string(),
        "Session commit".to_string(),
        SingleCommitMessageStrategy::Replace,
    )
    .await;

    // Assert
    let error = result.expect_err("amend-would-be-empty should fail");
    let commit_count = git_command_stdout(temp_dir.path(), &["rev-list", "--count", "HEAD"]);
    let head_message = git_command_stdout(temp_dir.path(), &["log", "-1", "--pretty=%B"]);
    let status = git_command_stdout(temp_dir.path(), &["status", "--porcelain"]);

    assert!(
        error.to_string().contains("Nothing to commit"),
        "expected 'Nothing to commit' sentinel but got: {error}"
    );
    assert_eq!(commit_count, "1");
    assert_eq!(head_message, "Initial commit");
    assert_eq!(status, "");
}
