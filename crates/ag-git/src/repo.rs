use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use tokio::process::Command as AsyncCommand;
use tokio::task::spawn_blocking;
use tokio::time;

use super::error::GitError;

/// Maximum time one asynchronous git subprocess may run.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// One asynchronous git invocation with owned process inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AsyncGitCommand {
    /// Git arguments excluding the executable name.
    pub(super) arguments: Vec<String>,
    /// Environment overrides applied after noninteractive defaults.
    pub(super) environment: Vec<(OsString, OsString)>,
    /// Repository or worktree used as the process working directory.
    pub(super) repo_path: PathBuf,
}

impl AsyncGitCommand {
    /// Builds one git command without environment overrides.
    pub(super) fn new(repo_path: PathBuf, arguments: Vec<String>) -> Self {
        Self {
            arguments,
            environment: Vec::new(),
            repo_path,
        }
    }

    /// Applies environment overrides to this command.
    pub(super) fn with_environment(mut self, environment: Vec<(OsString, OsString)>) -> Self {
        self.environment = environment;

        self
    }
}

/// Captured output from one asynchronous git subprocess.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AsyncGitCommandOutput {
    /// Process exit code, or `None` when terminated by a signal.
    pub(super) exit_code: Option<i32>,
    /// Captured standard error bytes.
    pub(super) stderr: Vec<u8>,
    /// Captured standard output bytes.
    pub(super) stdout: Vec<u8>,
}

impl AsyncGitCommandOutput {
    /// Returns whether the process exited successfully.
    pub(super) fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Mockable boundary for cancellable asynchronous git subprocesses.
#[cfg_attr(test, mockall::automock)]
pub(super) trait AsyncGitCommandRunner: Send + Sync {
    /// Runs one owned git command and captures its output.
    fn run(
        &self,
        command: AsyncGitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<AsyncGitCommandOutput, GitError>> + Send>>;
}

/// Production asynchronous git runner backed by `tokio::process`.
pub(super) struct ProcessAsyncGitCommandRunner;

impl AsyncGitCommandRunner for ProcessAsyncGitCommandRunner {
    fn run(
        &self,
        command: AsyncGitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<AsyncGitCommandOutput, GitError>> + Send>> {
        Box::pin(async move { run_git_command_with_timeout(command, GIT_COMMAND_TIMEOUT).await })
    }
}

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
pub(crate) async fn repo_url(repo_path: PathBuf) -> Result<String, GitError> {
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
pub(crate) async fn main_repo_root(repo_path: PathBuf) -> Result<PathBuf, GitError> {
    match resolve_shared_repo(&repo_path).await? {
        SharedRepo::Working(path) | SharedRepo::Bare(path) => Ok(path),
    }
}

/// Resolves the main working checkout for a repository or linked worktree.
///
/// Returns `Some(path)` with the main working checkout root for non-bare shared
/// repositories, and `None` when the shared repository is bare because a bare
/// repository has no main working checkout.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(Some(path)) with the main working checkout, Ok(None) when the shared
/// repository is bare, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns an error if git metadata cannot be queried from `repo_path`.
pub(crate) async fn main_checkout_working_tree(
    repo_path: PathBuf,
) -> Result<Option<PathBuf>, GitError> {
    spawn_blocking(move || main_checkout_working_tree_sync(&repo_path)).await?
}

/// Resolves the main working checkout for `repo_path` in synchronous code.
///
/// Returns `Some(path)` for non-bare shared repositories and `None` for bare
/// shared repositories, which have no main working checkout.
pub(super) fn main_checkout_working_tree_sync(
    repo_path: &Path,
) -> Result<Option<PathBuf>, GitError> {
    match resolve_shared_repo_sync(repo_path)? {
        SharedRepo::Working(path) => Ok(Some(path)),
        SharedRepo::Bare(_) => Ok(None),
    }
}

/// Classifies the shared repository backing a worktree.
pub(super) enum SharedRepo {
    /// Non-bare shared repository. The path is the main working checkout root.
    Working(PathBuf),
    /// Bare shared repository. The path is the bare common git directory, which
    /// has no main working checkout but is a valid administrative working
    /// directory for `git worktree` and branch commands.
    Bare(PathBuf),
}

/// Resolves the shared repository backing `repo_path`.
///
/// Reads the git and common git directories, detects whether the shared
/// repository is bare by running `git rev-parse --is-bare-repository` inside
/// the common git directory, and returns [`SharedRepo::Bare`] with the bare
/// common git directory when bare. Otherwise returns [`SharedRepo::Working`]
/// with the main working checkout root, matching the async administrative-root
/// resolution for non-bare layouts.
///
/// # Errors
/// Returns an error if git metadata cannot be queried from `repo_path`.
pub(super) fn resolve_shared_repo_sync(repo_path: &Path) -> Result<SharedRepo, GitError> {
    let (git_dir, git_common_dir) = git_directory_paths(repo_path)?;

    // Probe the common git directory directly by running git there, so the
    // shared repository's bareness is detected even from a linked worktree
    // (where `--is-bare-repository` reports the worktree, not the shared repo).
    // Passing the directory through `current_dir` keeps non-UTF-8 paths intact,
    // unlike converting it to a `--git-dir` string argument.
    let is_bare = run_git_command_sync(
        &git_common_dir,
        &["rev-parse", "--is-bare-repository"],
        "Git rev-parse --is-bare-repository failed",
    )?;
    if is_bare.trim() == "true" {
        return Ok(SharedRepo::Bare(git_common_dir));
    }

    if git_dir == git_common_dir {
        return Ok(SharedRepo::Working(repo_root_from_git_dir(
            repo_path, &git_dir,
        )?));
    }

    Ok(SharedRepo::Working(repo_root_from_git_dir(
        repo_path,
        &git_common_dir,
    )?))
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

/// Runs a cancellable git command and returns stdout text.
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
/// Returns [`GitError::CommandTimedOut`] if the command exceeds its runtime
/// bound, or [`GitError::CommandFailed`] if spawning fails or the command
/// exits with a non-zero status.
pub(super) async fn run_git_command(
    repo_path: PathBuf,
    args: Vec<String>,
    error_context: String,
) -> Result<String, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    run_git_command_with_runner(
        AsyncGitCommand::new(repo_path, args),
        &error_context,
        &command_runner,
    )
    .await
}

/// Runs one asynchronous git command through an injected runner.
pub(super) async fn run_git_command_with_runner(
    command: AsyncGitCommand,
    error_context: &str,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let stdout = run_git_command_bytes_with_runner(command, error_context, command_runner).await?;

    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Runs a git command while preserving stdout bytes for native path consumers.
pub(super) async fn run_git_command_bytes_with_runner(
    command: AsyncGitCommand,
    error_context: &str,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<Vec<u8>, GitError> {
    let git_invocation = format_git_invocation_from_strings(&command.arguments);
    let output = command_runner.run(command).await?;
    if !output.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(GitError::CommandFailed {
            command: git_invocation,
            stderr: format!("{error_context}: {detail}"),
        });
    }

    Ok(output.stdout)
}

/// Runs a cancellable git subprocess with an explicit runtime bound.
async fn run_git_command_with_timeout(
    command: AsyncGitCommand,
    timeout: Duration,
) -> Result<AsyncGitCommandOutput, GitError> {
    let git_invocation = format_git_invocation_from_strings(&command.arguments);
    let mut process = AsyncCommand::new("git");
    process
        .args(&command.arguments)
        .current_dir(&command.repo_path)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    apply_non_interactive_environment_async(&mut process);
    for (key, value) in &command.environment {
        process.env(key, value);
    }

    let output = time::timeout(timeout, process.output())
        .await
        .map_err(|_| GitError::CommandTimedOut {
            command: git_invocation.clone(),
            timeout,
        })?
        .map_err(|error| GitError::CommandFailed {
            command: git_invocation.clone(),
            stderr: error.to_string(),
        })?;

    Ok(AsyncGitCommandOutput {
        exit_code: output.status.code(),
        stderr: output.stderr,
        stdout: output.stdout,
    })
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

/// Formats a git invocation from owned command arguments.
pub(super) fn format_git_invocation_from_strings(args: &[String]) -> String {
    let argument_refs = args.iter().map(String::as_str).collect::<Vec<_>>();

    format_git_invocation(&argument_refs)
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
    run_git_command_output_with_env_sync(repo_path, args, &[] as &[(&str, &str)])
}

/// Runs a git command in `repo_path` with environment overrides and returns
/// raw process output.
///
/// Applies non-interactive defaults (`GIT_TERMINAL_PROMPT=0`,
/// `GCM_INTERACTIVE=never`, and SSH batch mode) and closes stdin so
/// credential failures do not block waiting for terminal input.
/// Caller-provided `environment` pairs are then applied and can override
/// these defaults.
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
pub(super) fn run_git_command_output_with_env_sync<Key, Value>(
    repo_path: &Path,
    args: &[&str],
    environment: &[(Key, Value)],
) -> Result<Output, GitError>
where
    Key: AsRef<std::ffi::OsStr>,
    Value: AsRef<std::ffi::OsStr>,
{
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(repo_path)
        .stdin(Stdio::null());
    apply_non_interactive_environment(&mut command);

    for (key, value) in environment {
        command.env(key, value);
    }

    command.output().map_err(|error| GitError::CommandFailed {
        command: format_git_invocation(args),
        stderr: error.to_string(),
    })
}

/// Disables optional index writes and interactive prompts for Git subprocesses.
/// Required locks for staging, commits, and other mutations remain enabled.
fn apply_non_interactive_environment(command: &mut Command) {
    let git_ssh_command = std::env::var("GIT_SSH_COMMAND").map_or_else(
        |_| "ssh -o BatchMode=yes".to_string(),
        |configured_command| format!("{configured_command} -o BatchMode=yes"),
    );

    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_SSH_COMMAND", git_ssh_command);
}

/// Disables optional index writes and interactive prompts for async Git.
fn apply_non_interactive_environment_async(command: &mut AsyncCommand) {
    let git_ssh_command = std::env::var("GIT_SSH_COMMAND").map_or_else(
        |_| "ssh -o BatchMode=yes".to_string(),
        |configured_command| format!("{configured_command} -o BatchMode=yes"),
    );

    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_SSH_COMMAND", git_ssh_command);
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

/// Resolves the shared repository through cancellable async git commands.
async fn resolve_shared_repo(repo_path: &Path) -> Result<SharedRepo, GitError> {
    let (git_dir, git_common_dir) = git_directory_paths_async(repo_path).await?;
    let is_bare = run_git_command(
        git_common_dir.clone(),
        vec!["rev-parse".to_string(), "--is-bare-repository".to_string()],
        "Git rev-parse --is-bare-repository failed".to_string(),
    )
    .await?;
    if is_bare.trim() == "true" {
        return Ok(SharedRepo::Bare(git_common_dir));
    }

    let shared_git_dir = if git_dir == git_common_dir {
        git_dir
    } else {
        git_common_dir
    };
    let repo_root = repo_root_from_git_dir_async(repo_path, &shared_git_dir).await?;

    Ok(SharedRepo::Working(repo_root))
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
async fn git_directory_paths_async(repo_path: &Path) -> Result<(PathBuf, PathBuf), GitError> {
    let stdout = run_git_command(
        repo_path.to_path_buf(),
        vec![
            "rev-parse".to_string(),
            "--git-dir".to_string(),
            "--git-common-dir".to_string(),
        ],
        "Git rev-parse failed".to_string(),
    )
    .await?;

    parse_git_directory_paths(repo_path, &stdout)
}

/// Reads absolute git and common git directory paths synchronously.
fn git_directory_paths(repo_path: &Path) -> Result<(PathBuf, PathBuf), GitError> {
    let stdout = run_git_command_sync(
        repo_path,
        &["rev-parse", "--git-dir", "--git-common-dir"],
        "Git rev-parse failed",
    )?;

    parse_git_directory_paths(repo_path, &stdout)
}

/// Parses the two paths emitted by `git rev-parse`.
fn parse_git_directory_paths(
    repo_path: &Path,
    stdout: &str,
) -> Result<(PathBuf, PathBuf), GitError> {
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
async fn repo_root_from_git_dir_async(
    repo_path: &Path,
    git_dir: &Path,
) -> Result<PathBuf, GitError> {
    if let Some(repo_root) = repo_root_from_dot_git_dir(git_dir)? {
        return Ok(repo_root);
    }

    let root = run_git_command(
        repo_path.to_path_buf(),
        vec!["rev-parse".to_string(), "--show-toplevel".to_string()],
        "Git rev-parse --show-toplevel failed".to_string(),
    )
    .await?;

    parse_repo_root(&root)
}

/// Converts a git directory path into a repository root synchronously.
fn repo_root_from_git_dir(repo_path: &Path, git_dir: &Path) -> Result<PathBuf, GitError> {
    if let Some(repo_root) = repo_root_from_dot_git_dir(git_dir)? {
        return Ok(repo_root);
    }

    let root = run_git_command_sync(
        repo_path,
        &["rev-parse", "--show-toplevel"],
        "Git rev-parse --show-toplevel failed",
    )?;

    parse_repo_root(&root)
}

/// Returns the parent of a conventional `.git` directory when applicable.
fn repo_root_from_dot_git_dir(git_dir: &Path) -> Result<Option<PathBuf>, GitError> {
    if git_dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_none_or(|name| name != ".git")
    {
        return Ok(None);
    }

    git_dir
        .parent()
        .map(Path::to_path_buf)
        .map(Some)
        .ok_or_else(|| {
            GitError::OutputParse(format!(
                "Git directory has no parent: {}",
                git_dir.display()
            ))
        })
}

/// Parses a non-empty `git rev-parse --show-toplevel` response.
fn parse_repo_root(root: &str) -> Result<PathBuf, GitError> {
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
        let empty_tree =
            run_setup_git(&bare_dir, &["hash-object", "-w", "-t", "tree", "/dev/null"]);
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
