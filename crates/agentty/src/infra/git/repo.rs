use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tokio::task::spawn_blocking;

use super::error::GitError;

/// Returns the origin repository URL normalized to HTTPS form when possible.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(url) on success, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns an error if the remote URL cannot be read via `git remote get-url`.
pub async fn repo_url(repo_path: PathBuf) -> Result<String, GitError> {
    let remote = run_git_command(
        repo_path,
        vec![
            "remote".to_string(),
            "get-url".to_string(),
            "origin".to_string(),
        ],
        "Git remote get-url failed".to_string(),
    )
    .await?;

    Ok(normalize_repo_url(remote.trim()))
}

/// Resolves the main repository root for a repository or linked worktree.
///
/// Uses `git rev-parse --git-dir --git-common-dir`, normalizes both paths to
/// absolute form, detects whether `repo_path` is a linked worktree (`git-dir`
/// differs from `git-common-dir`), and then returns the shared repository
/// root.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(path) containing the main repository root, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns an error if git metadata cannot be queried from `repo_path`.
pub async fn main_repo_root(repo_path: PathBuf) -> Result<PathBuf, GitError> {
    spawn_blocking(move || main_repo_root_sync(&repo_path)).await?
}

/// Resolves the main repository root for `repo_path` in synchronous code.
pub(super) fn main_repo_root_sync(repo_path: &Path) -> Result<PathBuf, GitError> {
    let (git_dir, git_common_dir) = git_directory_paths(repo_path)?;

    if git_dir == git_common_dir {
        return repo_root_from_git_dir(repo_path, &git_dir);
    }

    repo_root_from_git_dir(repo_path, &git_common_dir)
}

/// Resolves the git directory path for a repository root or worktree root.
pub(super) fn resolve_git_dir(repo_dir: &Path) -> Option<PathBuf> {
    let dot_git = repo_dir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if dot_git.is_file() {
        let content = fs::read_to_string(&dot_git).ok()?;
        let git_dir_line = content.lines().find(|line| line.starts_with("gitdir:"))?;
        let git_dir_path = git_dir_line.trim_start_matches("gitdir:").trim();
        let git_dir = PathBuf::from(git_dir_path);

        if git_dir.is_absolute() {
            return Some(git_dir);
        }

        return Some(repo_dir.join(git_dir));
    }

    None
}

/// Runs a git command in a blocking task and returns stdout text.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
/// * `error_context` - Prefix used for command failure messages
///
/// # Returns
/// The command stdout on success.
///
/// # Errors
/// Returns [`GitError::Join`] if the blocking task panics, or
/// [`GitError::CommandFailed`] if spawning fails or the command exits with
/// a non-zero status.
pub(super) async fn run_git_command(
    repo_path: PathBuf,
    args: Vec<String>,
    error_context: String,
) -> Result<String, GitError> {
    spawn_blocking(move || {
        let argument_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        run_git_command_sync(&repo_path, &argument_refs, &error_context)
    })
    .await?
}

/// Runs a git command in `repo_path` and returns stdout text.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
/// * `error_context` - Human-readable label prepended to the stderr detail on
///   failure (e.g. `"Failed to read squash merge diff"`)
///
/// # Returns
/// The command stdout on success.
///
/// # Errors
/// Returns [`GitError::CommandFailed`] with the concrete git invocation in
/// `command` and the `error_context` plus stderr/stdout detail in `stderr`.
pub(super) fn run_git_command_sync(
    repo_path: &Path,
    args: &[&str],
    error_context: &str,
) -> Result<String, GitError> {
    let output = run_git_command_output_sync(repo_path, args)?;
    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);
        let git_invocation = format_git_invocation(args);

        return Err(GitError::CommandFailed {
            command: git_invocation,
            stderr: format!("{error_context}: {detail}"),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Formats the full git invocation string from command arguments.
fn format_git_invocation(args: &[&str]) -> String {
    if args.is_empty() {
        return "git".to_string();
    }

    format!("git {}", args.join(" "))
}

/// Runs a git command in `repo_path` and returns raw process output.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
///
/// # Returns
/// The process output, including status, stdout, and stderr.
///
/// # Errors
/// Returns [`GitError::CommandFailed`] if spawning the command fails.
pub(super) fn run_git_command_output_sync(
    repo_path: &Path,
    args: &[&str],
) -> Result<Output, GitError> {
    run_git_command_output_with_env_sync(repo_path, args, &[])
}

/// Runs a git command in `repo_path` with environment overrides and returns
/// raw process output.
///
/// Applies non-interactive defaults (`GIT_TERMINAL_PROMPT=0`,
/// `GCM_INTERACTIVE=never`) so credential failures do not block waiting for
/// terminal input. Caller-provided `environment` pairs are then applied and
/// can override these defaults.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
/// * `environment` - Environment variables applied to the git process
///
/// # Returns
/// The process output, including status, stdout, and stderr.
///
/// # Errors
/// Returns [`GitError::CommandFailed`] if spawning the command fails.
pub(super) fn run_git_command_output_with_env_sync(
    repo_path: &Path,
    args: &[&str],
    environment: &[(&str, &str)],
) -> Result<Output, GitError> {
    let mut command = Command::new("git");
    command.args(args).current_dir(repo_path);
    apply_non_interactive_environment(&mut command);

    for (key, value) in environment {
        command.env(key, value);
    }

    command.output().map_err(|error| GitError::CommandFailed {
        command: format_git_invocation(args),
        stderr: error.to_string(),
    })
}

/// Applies non-interactive defaults so git failures return immediately instead
/// of waiting for terminal credential prompts.
fn apply_non_interactive_environment(command: &mut Command) {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
}

/// Extracts the best human-readable error detail from command output.
pub(super) fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return stderr_text;
    }

    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    if !stdout_text.is_empty() {
        return stdout_text;
    }

    "Unknown git error".to_string()
}

/// Converts SSH-style GitHub remotes into HTTPS while preserving other URLs.
fn normalize_repo_url(remote: &str) -> String {
    let trimmed = remote.trim_end_matches(".git");
    if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        return format!("https://github.com/{path}");
    }

    if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        return format!("https://github.com/{path}");
    }

    trimmed.to_string()
}

/// Reads absolute git and common git directory paths for `repo_path`.
fn git_directory_paths(repo_path: &Path) -> Result<(PathBuf, PathBuf), GitError> {
    let stdout = run_git_command_sync(
        repo_path,
        &["rev-parse", "--git-dir", "--git-common-dir"],
        "Git rev-parse failed",
    )?;
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let git_dir = lines
        .next()
        .ok_or_else(|| GitError::OutputParse("Git rev-parse output missing git-dir".to_string()))?;
    let git_common_dir = lines.next().ok_or_else(|| {
        GitError::OutputParse("Git rev-parse output missing git-common-dir".to_string())
    })?;

    Ok((
        normalize_git_dir_path(repo_path, git_dir),
        normalize_git_dir_path(repo_path, git_common_dir),
    ))
}

/// Converts a git directory path (typically `.git`) into repository root.
fn repo_root_from_git_dir(repo_path: &Path, git_dir: &Path) -> Result<PathBuf, GitError> {
    if git_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".git")
    {
        return git_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
            GitError::OutputParse(format!(
                "Git directory has no parent: {}",
                git_dir.display()
            ))
        });
    }

    let root = run_git_command_sync(
        repo_path,
        &["rev-parse", "--show-toplevel"],
        "Git rev-parse --show-toplevel failed",
    )?;
    let root = root.trim().to_string();
    if root.is_empty() {
        return Err(GitError::OutputParse(
            "Git rev-parse --show-toplevel returned empty output".to_string(),
        ));
    }

    Ok(PathBuf::from(root))
}

/// Normalizes a git metadata path into absolute form for path comparisons.
fn normalize_git_dir_path(repo_path: &Path, git_path: &str) -> PathBuf {
    let git_path = PathBuf::from(git_path);
    let git_path = if git_path.is_absolute() {
        git_path
    } else {
        repo_path.join(git_path)
    };

    std::fs::canonicalize(&git_path).unwrap_or(git_path)
}

#[cfg(test)]
mod tests {
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
        let passthrough = normalize_repo_url("https://gitlab.com/agentty-xyz/agentty.git");

        // Assert
        assert_eq!(ssh_short, "https://github.com/agentty-xyz/agentty");
        assert_eq!(ssh_long, "https://github.com/agentty-xyz/agentty");
        assert_eq!(passthrough, "https://gitlab.com/agentty-xyz/agentty");
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

    #[test]
    fn test_main_repo_root_sync_returns_command_failed_outside_git_repository() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");

        // Act
        let result = main_repo_root_sync(temp_dir.path());

        // Assert
        let error = result.expect_err("non-repo should fail");
        assert!(
            matches!(&error, GitError::CommandFailed { command, stderr }
                if command.starts_with("git rev-parse")
                    && stderr.contains("Git rev-parse failed")),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn test_format_git_invocation_returns_bare_git_for_empty_args() {
        // Act / Assert
        assert_eq!(format_git_invocation(&[]), "git");
    }

    #[test]
    fn test_format_git_invocation_joins_args_after_git() {
        // Act / Assert
        assert_eq!(
            format_git_invocation(&["diff", "--cached", "--quiet"]),
            "git diff --cached --quiet"
        );
    }
}
