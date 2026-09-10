use super::*;

/// Builds captured asynchronous git output for command-runner tests.
pub(super) fn async_git_output(
    exit_code: i32,
    stdout: impl Into<Vec<u8>>,
    stderr: impl Into<Vec<u8>>,
) -> AsyncGitCommandOutput {
    AsyncGitCommandOutput {
        exit_code: Some(exit_code),
        stderr: stderr.into(),
        stdout: stdout.into(),
    }
}

/// Runs `git` in `repo_path` and asserts the command succeeds.
pub(super) fn run_git_command(repo_path: &Path, args: &[&str]) {
    let output = git_command_output(repo_path, args);

    assert!(
        output.status.success(),
        "git command {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs `git` in `repo_path` and returns the captured command output.
pub(super) fn git_command_output(repo_path: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .expect("failed to run git command")
}

/// Runs `git` in `repo_path`, asserts success, and returns trimmed stdout.
pub(super) fn git_command_stdout(repo_path: &Path, args: &[&str]) -> String {
    let output = git_command_output(repo_path, args);

    assert!(
        output.status.success(),
        "git command {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("git stdout should be valid utf-8")
        .trim()
        .to_string()
}

/// Creates a committed repository rooted at `repo_path`.
pub(super) fn setup_test_git_repo(repo_path: &Path) {
    run_git_command(repo_path, &["init", "-b", "main"]);
    run_git_command(repo_path, &["config", "user.name", "Test User"]);
    run_git_command(repo_path, &["config", "user.email", "test@example.com"]);
    fs::write(repo_path.join("README.md"), "base\n").expect("failed to write base file");
    run_git_command(repo_path, &["add", "README.md"]);
    run_git_command(repo_path, &["commit", "-m", "Initial commit"]);
}

#[cfg(unix)]
pub(super) fn write_executable_pre_commit_hook(hook_path: &Path) {
    write_executable_hook(hook_path, "#!/bin/sh\nexit 0\n");
}

#[cfg(unix)]
pub(super) fn write_executable_hook(hook_path: &Path, contents: &str) {
    fs::create_dir_all(
        hook_path
            .parent()
            .expect("pre-commit hook should have a parent directory"),
    )
    .expect("failed to create hooks directory");
    fs::write(hook_path, contents).expect("failed to write Git hook");
    let mut permissions = fs::metadata(hook_path)
        .expect("failed to read Git hook metadata")
        .permissions();
    permissions.set_mode(0o750);
    fs::set_permissions(hook_path, permissions).expect("failed to make Git hook executable");
}
