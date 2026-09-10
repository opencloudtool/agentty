use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::time::Duration;

#[cfg(unix)]
use rustix::fs::{self as rustix_fs, Access};
use tokio::task::spawn_blocking;
use tokio::time;

use super::error::GitError;
use super::rebase::{
    GIT_INDEX_LOCK_RETRY_ATTEMPTS, GIT_INDEX_LOCK_RETRY_DELAY, is_git_index_lock_error,
    is_rebase_conflict, run_git_command_with_index_lock_retry,
};
use super::repo::{
    AsyncGitCommand, AsyncGitCommandOutput, AsyncGitCommandRunner, ProcessAsyncGitCommandRunner,
    command_output_detail, run_git_command, run_git_command_output_sync,
    run_git_command_output_with_env_sync, run_git_command_sync, run_git_command_with_runner,
};

/// Map of local branch names to their ahead/behind counts relative to their
/// tracked upstream branch. `None` indicates no upstream or a gone upstream.
pub type BranchTrackingMap = HashMap<String, Option<(u32, u32)>>;

const COMMIT_ALL_HOOK_RETRY_ATTEMPTS: usize = 5;
const MAX_WORKTREE_FILE_BYTE_COUNT: usize = 1024 * 1024;
const PRE_COMMIT_CONFIG_FILES: [&str; 2] = [".pre-commit-config.yaml", ".pre-commit-config.yml"];

/// Bounded content returned when reading a worktree file for presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeFileContent {
    /// The file contains valid UTF-8 text within the preview byte limit.
    Text(String),
    /// The file does not exist in the current worktree.
    Missing,
    /// The file is not valid UTF-8 text.
    Binary,
    /// The file exceeds the preview byte limit.
    TooLarge,
}

/// Controls how single-commit session branches treat the commit message when
/// amending `HEAD`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SingleCommitMessageStrategy {
    /// Replaces the existing `HEAD` message with the newly generated message.
    Replace,
    /// Keeps the current `HEAD` message while amending file content only.
    Reuse,
}

/// Result of attempting `git pull --rebase`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PullRebaseResult {
    /// Pull and rebase completed successfully.
    Completed,
    /// Pull stopped because of merge conflicts.
    Conflict {
        /// Git diagnostic describing the conflict state.
        detail: String,
    },
}

/// Stages all changes and commits them with the given message.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `commit_message` - Message for the commit
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if staging or committing changes fails.
pub(crate) async fn commit_all(repo_path: PathBuf, commit_message: String) -> Result<(), GitError> {
    commit_all_with_retry(
        repo_path,
        commit_message,
        SingleCommitMessageStrategy::Replace,
        false,
    )
    .await
}

/// Stages all changes and keeps a single commit for the provided message.
///
/// Creates a new commit when `HEAD` has no commits beyond `base_branch`.
/// Otherwise, amends `HEAD` so the branch keeps one evolving session commit.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `base_branch` - Branch used to detect whether a session commit already
///   exists on `HEAD`
/// * `commit_message` - Message that identifies the session commit
/// * `message_strategy` - Whether amends replace or reuse the existing `HEAD`
///   message
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if staging, commit lookup, or committing changes
/// fails.
pub(crate) async fn commit_all_preserving_single_commit(
    repo_path: PathBuf,
    base_branch: String,
    commit_message: String,
    message_strategy: SingleCommitMessageStrategy,
) -> Result<(), GitError> {
    let amend_existing_commit = has_commits_since(repo_path.clone(), base_branch).await?;

    commit_all_with_retry(
        repo_path,
        commit_message,
        message_strategy,
        amend_existing_commit,
    )
    .await
}

/// Stages all changes in the repository or worktree.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if `git add -A` fails.
pub(crate) async fn stage_all(repo_path: PathBuf) -> Result<(), GitError> {
    spawn_blocking(move || stage_all_sync(&repo_path)).await?
}

/// Verifies that configured pre-commit validation has an executable Git hook.
///
/// # Errors
/// Returns [`GitError::PreCommitHookMissing`] when a supported configuration
/// exists without an executable hook, or a command error when the effective
/// hook path cannot be resolved.
pub(crate) async fn check_pre_commit_hook_ready(repo_path: PathBuf) -> Result<(), GitError> {
    spawn_blocking(move || ensure_pre_commit_hook_ready(&repo_path)).await?
}

/// Runs the effective Git `pre-commit` hook against the current index.
///
/// Missing hooks are accepted so repositories without configured validation
/// retain Git's normal commit behavior. Hook failures are returned with their
/// captured output.
///
/// # Errors
/// Returns a [`GitError`] when Git cannot run the hook or the hook rejects the
/// staged changes.
pub(crate) async fn run_pre_commit_hook(repo_path: PathBuf) -> Result<(), GitError> {
    spawn_blocking(move || {
        pre_commit_hook_result(run_git_command_output_sync(
            &repo_path,
            &["hook", "run", "--ignore-missing", "pre-commit"],
        ))
    })
    .await?
}

/// Interprets the output of an effective pre-commit hook invocation.
fn pre_commit_hook_result(output: Result<Output, GitError>) -> Result<(), GitError> {
    let output = output?;
    if output.status.success() {
        return Ok(());
    }

    Err(GitError::CommandFailed {
        command: "git hook run pre-commit".to_string(),
        stderr: command_output_detail(&output.stdout, &output.stderr),
    })
}

/// Returns the short hash of the current `HEAD` commit.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// The short commit hash as a string.
///
/// # Errors
/// Returns a [`GitError`] if resolving `HEAD` fails.
pub(crate) async fn head_short_hash(repo_path: PathBuf) -> Result<String, GitError> {
    let hash = run_git_command(
        repo_path,
        vec![
            "rev-parse".to_string(),
            "--short".to_string(),
            "HEAD".to_string(),
        ],
        "Failed to resolve HEAD hash".to_string(),
    )
    .await?;
    let hash = hash.trim().to_string();
    if hash.is_empty() {
        return Err(GitError::OutputParse(
            "Failed to resolve HEAD hash: empty output".to_string(),
        ));
    }

    Ok(hash)
}

/// Returns the full hash of the current `HEAD` commit.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// The full commit hash as a string.
///
/// # Errors
/// Returns a [`GitError`] if resolving `HEAD` fails.
pub(crate) async fn head_hash(repo_path: PathBuf) -> Result<String, GitError> {
    let hash = run_git_command(
        repo_path,
        vec!["rev-parse".to_string(), "HEAD".to_string()],
        "Failed to resolve HEAD hash".to_string(),
    )
    .await?;
    let hash = hash.trim().to_string();
    if hash.is_empty() {
        return Err(GitError::OutputParse(
            "Failed to resolve HEAD hash: empty output".to_string(),
        ));
    }

    Ok(hash)
}

/// Returns the full commit hash for a git reference.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree.
/// * `reference` - Branch, tag, or commit-ish to resolve.
///
/// # Returns
/// The full commit hash as a string.
///
/// # Errors
/// Returns a [`GitError`] if the reference cannot be resolved to a commit.
pub(crate) async fn ref_hash(repo_path: PathBuf, reference: String) -> Result<String, GitError> {
    let hash = run_git_command(
        repo_path,
        vec![
            "rev-parse".to_string(),
            "--verify".to_string(),
            format!("{reference}^{{commit}}"),
        ],
        format!("Failed to resolve `{reference}` hash"),
    )
    .await?;
    let hash = hash.trim().to_string();
    if hash.is_empty() {
        return Err(GitError::OutputParse(format!(
            "Failed to resolve `{reference}` hash: empty output"
        )));
    }

    Ok(hash)
}

/// Returns the full `HEAD` commit message, or `None` when no commits exist.
///
/// # Errors
/// Returns a [`GitError`] if `HEAD` cannot be inspected.
pub(crate) async fn head_commit_message(repo_path: PathBuf) -> Result<Option<String>, GitError> {
    spawn_blocking(move || head_commit_message_sync(&repo_path)).await?
}

/// Deletes a git branch.
///
/// Uses -D to force deletion even if not merged.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
/// * `branch_name` - Name of the branch to delete
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if the branch delete command fails or exceeds its
/// runtime bound.
pub(crate) async fn delete_branch(repo_path: PathBuf, branch_name: String) -> Result<(), GitError> {
    run_git_command(
        repo_path,
        vec!["branch".to_string(), "-D".to_string(), branch_name],
        "Git branch deletion failed".to_string(),
    )
    .await?;

    Ok(())
}

/// Returns the output of `git diff` for the given repository path, showing
/// all changes (committed and uncommitted) relative to the base branch.
///
/// Copies the repository index into a temporary index and uses
/// `git add --intent-to-add` there to make untracked files visible, then
/// finds the merge-base between `HEAD` and `base_branch` to diff against the
/// fork point. To avoid re-showing squash-merged/cherry-picked session commits
/// on non-rebased branches, this also checks `git cherry` and, when applicable,
/// diffs from the last leading commit already applied to `base_branch`.
/// The real repository index is never modified.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `base_branch` - Branch to diff against (e.g., `main`)
///
/// # Returns
/// The diff output as a string.
///
/// # Errors
/// Returns a [`GitError`] if preparing the temporary index or generating the
/// diff fails.
pub(crate) async fn diff(repo_path: PathBuf, base_branch: String) -> Result<String, GitError> {
    diff_output(repo_path, base_branch, false).await
}

/// Returns repository-relative changed paths using the same isolated index and
/// fork-point semantics as [`diff`].
pub(crate) async fn diff_changed_files(
    repo_path: PathBuf,
    base_branch: String,
) -> Result<Vec<String>, GitError> {
    let output = diff_output(repo_path, base_branch, true).await?;

    Ok(output
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

/// Generates either a patch or name-only output without mutating the real
/// repository index.
async fn diff_output(
    repo_path: PathBuf,
    base_branch: String,
    name_only: bool,
) -> Result<String, GitError> {
    spawn_blocking(move || -> Result<String, GitError> {
        let index_path = resolve_diff_index_path(&repo_path)?;
        let index_path = PathBuf::from(index_path.trim());
        let index_path = if index_path.is_absolute() {
            index_path
        } else {
            repo_path.join(index_path)
        };

        diff_output_after_index_resolution(&repo_path, &base_branch, name_only, &index_path)
    })
    .await?
}

/// Generates diff output after the real index path has been resolved.
fn diff_output_after_index_resolution(
    repo_path: &Path,
    base_branch: &str,
    name_only: bool,
    index_path: &Path,
) -> Result<String, GitError> {
    let result = (|| -> Result<String, GitError> {
        let temporary_index = copy_git_index_to_temp(index_path)?;

        run_git_command_with_index_sync(
            repo_path,
            &["add", "-A", "--intent-to-add"],
            &temporary_index,
            "Git add --intent-to-add failed",
        )?;

        let merge_base_output =
            run_git_command_output_sync(repo_path, &["merge-base", "HEAD", base_branch])?;

        let diff_target = if merge_base_output.status.success() {
            resolve_diff_target(
                repo_path,
                base_branch,
                String::from_utf8_lossy(&merge_base_output.stdout).trim(),
            )?
        } else {
            base_branch.to_string()
        };

        let args = if name_only {
            vec!["diff", "--name-only", diff_target.as_str()]
        } else {
            vec!["diff", diff_target.as_str()]
        };

        run_git_command_with_index_sync(repo_path, &args, &temporary_index, "Git diff failed")
    })();

    result.map_err(|error| classify_diff_repository_error(repo_path, error))
}

/// Resolves the real index path and classifies a reclaimed repository.
fn resolve_diff_index_path(repo_path: &Path) -> Result<String, GitError> {
    run_git_command_sync(
        repo_path,
        &["rev-parse", "--git-path", "index"],
        "Git index path resolution failed",
    )
    .map_err(|error| classify_diff_repository_error(repo_path, error))
}

/// Preserves ordinary Git failures while typing unavailable repository paths.
fn classify_diff_repository_error(repo_path: &Path, error: GitError) -> GitError {
    if !diff_repository_is_unavailable(repo_path) {
        return error;
    }

    GitError::RepositoryUnavailable {
        detail: error.to_string(),
    }
}

/// Probes repository discovery without interpreting localized Git output.
fn diff_repository_is_unavailable(repo_path: &Path) -> bool {
    if !repo_path.is_dir() {
        return true;
    }

    diff_repository_probe_is_unavailable(run_git_command_output_sync(
        repo_path,
        &["rev-parse", "--git-dir"],
    ))
}

/// Treats only a completed, unsuccessful discovery probe as unavailable.
fn diff_repository_probe_is_unavailable(probe: Result<Output, GitError>) -> bool {
    match probe {
        Ok(output) => !output.status.success(),
        Err(_) => false,
    }
}

/// Reads one repository-relative worktree file with a fixed memory bound.
///
/// The path must contain only normal relative components. Canonical path
/// validation also rejects symlinks that resolve outside `repo_path`.
///
/// # Errors
/// Returns a [`GitError`] when the path is unsafe, repository path resolution
/// fails, or the selected file cannot be read.
pub(crate) async fn read_worktree_file(
    repo_path: PathBuf,
    relative_path: String,
) -> Result<WorktreeFileContent, GitError> {
    spawn_blocking(move || read_worktree_file_sync(&repo_path, &relative_path)).await?
}

/// Performs the bounded worktree read on a blocking worker thread.
fn read_worktree_file_sync(
    repo_path: &Path,
    relative_path: &str,
) -> Result<WorktreeFileContent, GitError> {
    let relative_file_path = Path::new(relative_path);
    if relative_path.is_empty()
        || relative_file_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(GitError::OutputParse(format!(
            "Unsafe worktree file path: {relative_path}"
        )));
    }

    let canonical_repo_path = std::fs::canonicalize(repo_path)?;
    let candidate_path = repo_path.join(relative_file_path);
    let canonical_file_path = match std::fs::canonicalize(candidate_path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WorktreeFileContent::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    if !canonical_file_path.starts_with(canonical_repo_path) {
        return Err(GitError::OutputParse(format!(
            "Worktree file resolves outside repository: {relative_path}"
        )));
    }

    let file = std::fs::File::open(canonical_file_path)?;
    let mut bytes = Vec::with_capacity(MAX_WORKTREE_FILE_BYTE_COUNT.min(8192));
    file.take((MAX_WORKTREE_FILE_BYTE_COUNT as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;

    Ok(worktree_file_content(bytes))
}

/// Classifies bytes read through the bounded worktree-file reader.
fn worktree_file_content(bytes: Vec<u8>) -> WorktreeFileContent {
    if bytes.len() > MAX_WORKTREE_FILE_BYTE_COUNT {
        return WorktreeFileContent::TooLarge;
    }

    match String::from_utf8(bytes) {
        Ok(content) => WorktreeFileContent::Text(content),
        Err(_) => WorktreeFileContent::Binary,
    }
}

/// Copies one repository index beside its source and returns the temporary
/// path used by isolated read-only diff commands.
fn copy_git_index_to_temp(index_path: &Path) -> Result<tempfile::TempPath, GitError> {
    let index_parent = index_path.parent().ok_or_else(|| {
        GitError::OutputParse(format!(
            "Git index path has no parent: {}",
            index_path.display()
        ))
    })?;
    let temporary_index =
        tempfile::NamedTempFile::new_in(index_parent).map_err(|error| GitError::CommandFailed {
            command: "create temporary git index".to_string(),
            stderr: error.to_string(),
        })?;
    std::fs::copy(index_path, temporary_index.path()).map_err(|error| GitError::CommandFailed {
        command: "copy git index".to_string(),
        stderr: error.to_string(),
    })?;

    Ok(temporary_index.into_temp_path())
}

/// Runs one git command against a temporary index without touching the real
/// index.
fn run_git_command_with_index_sync(
    repo_path: &Path,
    args: &[&str],
    index_path: &Path,
    error_context: &str,
) -> Result<String, GitError> {
    let output = run_git_command_output_with_env_sync(
        repo_path,
        args,
        &[("GIT_INDEX_FILE", index_path.as_os_str())],
    )?;
    if !output.status.success() {
        return Err(GitError::CommandFailed {
            command: format!("git {}", args.join(" ")),
            stderr: format!(
                "{error_context}: {}",
                command_output_detail(&output.stdout, &output.stderr)
            ),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Returns whether a repository or worktree has no uncommitted changes.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// `true` when `git status --porcelain` is empty, `false` otherwise.
///
/// # Errors
/// Returns a [`GitError`] if `git status --porcelain` cannot be executed.
pub(crate) async fn is_worktree_clean(repo_path: PathBuf) -> Result<bool, GitError> {
    let status_output = worktree_status(repo_path).await?;

    Ok(status_output.trim().is_empty())
}

/// Returns a stable porcelain status snapshot for a repository or worktree.
///
/// The snapshot includes untracked files so cleanup and review workflows can
/// detect all local filesystem changes in the worktree.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Raw `git status --porcelain=v1 --untracked-files=all` stdout.
///
/// # Errors
/// Returns a [`GitError`] if the status command cannot be executed.
pub(crate) async fn worktree_status(repo_path: PathBuf) -> Result<String, GitError> {
    run_git_command(
        repo_path,
        vec![
            "status".to_string(),
            "--porcelain=v1".to_string(),
            "--untracked-files=all".to_string(),
        ],
        "Git status --porcelain=v1 failed".to_string(),
    )
    .await
}

/// Returns a stable porcelain status snapshot for tracked worktree files only.
///
/// This omits untracked files so session isolation checks can ignore unrelated
/// editor or build artifacts while still catching modifications, deletions, and
/// staged changes to tracked files in the main checkout.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// Raw `git status --porcelain=v1 --untracked-files=no` stdout.
///
/// # Errors
/// Returns a [`GitError`] if the status command cannot be executed.
pub(crate) async fn tracked_worktree_status(repo_path: PathBuf) -> Result<String, GitError> {
    run_git_command(
        repo_path,
        vec![
            "status".to_string(),
            "--porcelain=v1".to_string(),
            "--untracked-files=no".to_string(),
        ],
        "Git tracked status --porcelain=v1 failed".to_string(),
    )
    .await
}

/// Runs `git pull --rebase` and returns conflict outcome when applicable.
///
/// When an upstream branch can be resolved, this uses an explicit
/// `git pull --rebase <remote> <branch>` target to avoid ambiguous rebase
/// failures caused by multiple configured merge branches.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// A [`PullRebaseResult`] describing whether pull/rebase completed or stopped
/// on conflicts.
///
/// # Errors
/// Returns a [`GitError`] for non-conflict pull/rebase failures.
pub(crate) async fn pull_rebase(repo_path: PathBuf) -> Result<PullRebaseResult, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    pull_rebase_with_runner(repo_path, &command_runner, GIT_INDEX_LOCK_RETRY_DELAY).await
}

/// Runs pull/rebase through an injected asynchronous command boundary.
async fn pull_rebase_with_runner(
    repo_path: PathBuf,
    command_runner: &dyn AsyncGitCommandRunner,
    retry_delay: Duration,
) -> Result<PullRebaseResult, GitError> {
    let pull_arguments = pull_rebase_arguments(&repo_path, command_runner).await?;
    let command = AsyncGitCommand::new(repo_path, pull_arguments).with_environment(vec![
        ("GIT_EDITOR".to_string(), ":".to_string()),
        ("GIT_SEQUENCE_EDITOR".to_string(), ":".to_string()),
    ]);
    let output =
        run_async_git_command_with_index_lock_retry(command, command_runner, retry_delay).await?;

    if output.success() {
        return Ok(PullRebaseResult::Completed);
    }

    let detail = command_output_detail(&output.stdout, &output.stderr);
    if is_rebase_conflict(&detail) {
        return Ok(PullRebaseResult::Conflict { detail });
    }

    Err(GitError::CommandFailed {
        command: "git pull --rebase".to_string(),
        stderr: detail,
    })
}

/// Builds pull arguments that target a single upstream branch when available.
///
/// Resolves an explicit `<remote> <branch>` pull target for both remote and
/// local upstreams so git does not need to infer one from branch config.
async fn pull_rebase_arguments(
    repo_path: &Path,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<Vec<String>, GitError> {
    let upstream_reference = primary_upstream_reference(repo_path, command_runner).await?;

    if let Some((remote_name, branch_name)) = upstream_reference.split_once('/') {
        return Ok(vec![
            "pull".to_string(),
            "--rebase".to_string(),
            remote_name.to_string(),
            branch_name.to_string(),
        ]);
    }

    let remote_name = current_branch_remote_name(repo_path, command_runner)
        .await?
        .ok_or_else(|| {
            GitError::OutputParse(
                "Failed to resolve current branch remote: not configured".to_string(),
            )
        })?;

    Ok(vec![
        "pull".to_string(),
        "--rebase".to_string(),
        remote_name,
        upstream_reference,
    ])
}

/// Returns the first upstream reference reported for `HEAD`.
///
/// Git can return multiple lines when multiple merge targets are configured.
/// Pulling with rebase needs one concrete target, so this selects the first
/// non-empty line.
async fn primary_upstream_reference(
    repo_path: &Path,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let upstream_reference = upstream_reference_name(repo_path, command_runner).await?;
    let Some(primary_reference) = upstream_reference
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
    else {
        return Err(GitError::OutputParse(
            "Failed to resolve upstream branch: empty output".to_string(),
        ));
    };

    Ok(primary_reference.to_string())
}

/// Returns the full upstream reference for `HEAD` (for example, `origin/main`).
async fn upstream_reference_name(
    repo_path: &Path,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let upstream_reference = run_git_command_with_runner(
        AsyncGitCommand::new(
            repo_path.to_path_buf(),
            vec![
                "rev-parse".to_string(),
                "--abbrev-ref".to_string(),
                "--symbolic-full-name".to_string(),
                "@{u}".to_string(),
            ],
        ),
        "Failed to resolve upstream branch",
        command_runner,
    )
    .await?;
    let upstream_reference = upstream_reference.trim().to_string();
    if upstream_reference.is_empty() {
        return Err(GitError::OutputParse(
            "Failed to resolve upstream branch: empty output".to_string(),
        ));
    }

    Ok(upstream_reference)
}

/// Returns the configured remote name for the current local branch.
///
/// This is used when the upstream short name omits a remote prefix (for
/// example, `main` with `branch.<name>.remote=.`).
async fn current_branch_remote_name(
    repo_path: &Path,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<Option<String>, GitError> {
    let current_branch_name = current_branch_name(repo_path, command_runner).await?;
    let remote_config_key = format!("branch.{current_branch_name}.remote");
    let output = command_runner
        .run(AsyncGitCommand::new(
            repo_path.to_path_buf(),
            vec![
                "config".to_string(),
                "--get".to_string(),
                remote_config_key.clone(),
            ],
        ))
        .await?;

    parse_current_branch_remote_output(&output, &remote_config_key)
}

/// Parses `git config --get` output for one current-branch remote.
fn parse_current_branch_remote_output(
    output: &AsyncGitCommandOutput,
    remote_config_key: &str,
) -> Result<Option<String>, GitError> {
    if output.exit_code == Some(1) {
        return Ok(None);
    }
    if !output.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(GitError::CommandFailed {
            command: format!("git config --get {remote_config_key}"),
            stderr: format!(
                "Failed to resolve current branch remote `{remote_config_key}`: {detail}"
            ),
        });
    }

    let remote_name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if remote_name.is_empty() {
        return Err(GitError::OutputParse(format!(
            "Failed to resolve current branch remote `{remote_config_key}`: empty output"
        )));
    }

    Ok(Some(remote_name))
}

/// Returns the current local branch name for `HEAD`.
async fn current_branch_name(
    repo_path: &Path,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let branch_name = run_git_command_with_runner(
        AsyncGitCommand::new(
            repo_path.to_path_buf(),
            vec![
                "rev-parse".to_string(),
                "--abbrev-ref".to_string(),
                "HEAD".to_string(),
            ],
        ),
        "Failed to resolve current branch name",
        command_runner,
    )
    .await?;
    let branch_name = branch_name.trim().to_string();
    if branch_name.is_empty() {
        return Err(GitError::OutputParse(
            "Failed to resolve current branch name: empty output".to_string(),
        ));
    }

    if branch_name == "HEAD" {
        return Err(GitError::OutputParse(
            "Failed to resolve current branch name: detached HEAD".to_string(),
        ));
    }

    Ok(branch_name)
}

/// Runs one asynchronous git command and retries transient index lock
/// contention without blocking a Tokio worker.
async fn run_async_git_command_with_index_lock_retry(
    command: AsyncGitCommand,
    command_runner: &dyn AsyncGitCommandRunner,
    retry_delay: Duration,
) -> Result<AsyncGitCommandOutput, GitError> {
    let mut attempt_count = 0;
    loop {
        attempt_count += 1;
        let output = command_runner.run(command.clone()).await?;
        if output.success() {
            return Ok(output);
        }

        let detail = command_output_detail(&output.stdout, &output.stderr);
        let is_last_attempt = attempt_count == GIT_INDEX_LOCK_RETRY_ATTEMPTS;
        if !is_git_index_lock_error(&detail) || is_last_attempt {
            return Ok(output);
        }

        time::sleep(retry_delay).await;
    }
}

/// Pushes the current branch to its upstream remote with
/// `--force-with-lease`.
///
/// Falls back to `git push --force-with-lease --set-upstream origin HEAD`
/// when no upstream branch is configured, then returns the resolved upstream
/// reference.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// The upstream reference on success.
///
/// # Errors
/// Returns a [`GitError`] if `git push` fails or upstream tracking cannot be
/// resolved afterwards.
pub(crate) async fn push_current_branch(repo_path: PathBuf) -> Result<String, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    push_current_branch_with_runner(repo_path, &command_runner).await
}

/// Pushes the current branch through an injected asynchronous command
/// boundary.
async fn push_current_branch_with_runner(
    repo_path: PathBuf,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let push_command = AsyncGitCommand::new(
        repo_path.clone(),
        vec!["push".to_string(), "--force-with-lease".to_string()],
    );
    let push_output = command_runner.run(push_command).await?;

    if push_output.success() {
        return primary_upstream_reference(&repo_path, command_runner).await;
    }

    let push_detail = command_output_detail(&push_output.stdout, &push_output.stderr);
    if !is_no_upstream_error(&push_detail) {
        return Err(GitError::CommandFailed {
            command: "git push --force-with-lease".to_string(),
            stderr: push_detail,
        });
    }

    let remote_name = current_branch_remote_name(&repo_path, command_runner)
        .await?
        .unwrap_or_else(|| "origin".to_string());
    run_git_command_with_runner(
        AsyncGitCommand::new(
            repo_path.clone(),
            vec![
                "push".to_string(),
                "--force-with-lease".to_string(),
                "--set-upstream".to_string(),
                remote_name,
                "HEAD".to_string(),
            ],
        ),
        "Git push failed",
        command_runner,
    )
    .await?;

    primary_upstream_reference(&repo_path, command_runner).await
}

/// Pushes the current branch to one explicit remote branch name with
/// `--force-with-lease` and returns the resulting upstream reference.
///
/// When the current branch already tracks a remote, that remote name is
/// reused. Otherwise this falls back to `origin`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `remote_branch_name` - Target branch name to create or update on the
///   remote
///
/// # Returns
/// The upstream reference on success, for example `origin/feature/review`.
///
/// # Errors
/// Returns a [`GitError`] if `git push` fails.
pub(crate) async fn push_current_branch_to_remote_branch(
    repo_path: PathBuf,
    remote_branch_name: String,
) -> Result<String, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    push_current_branch_to_remote_branch_with_runner(repo_path, remote_branch_name, &command_runner)
        .await
}

/// Pushes the current branch to one explicit remote branch name while
/// requiring that the remote ref does not exist.
///
/// The explicit empty lease ignores stale local remote-tracking refs left
/// behind after the remote branch was deleted, while still refusing to
/// overwrite a branch created concurrently.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `remote_branch_name` - Target branch name to create or update on the
///   remote
///
/// # Returns
/// The upstream reference on success, for example `origin/feature/review`.
///
/// # Errors
/// Returns a [`GitError`] if the remote branch exists or `git push` fails.
pub(crate) async fn push_current_branch_to_new_remote_branch(
    repo_path: PathBuf,
    remote_branch_name: String,
) -> Result<String, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    push_current_branch_to_new_remote_branch_with_runner(
        repo_path,
        remote_branch_name,
        &command_runner,
    )
    .await
}

/// Checks whether a branch already exists on the remote.
///
/// Resolves the remote name from the current branch config, falling back
/// to `origin`, then runs `git ls-remote --heads <remote> <branch>`.
/// Returns `true` when the remote reports at least one matching ref.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `remote_branch_name` - Branch name to look up on the remote
///
/// # Errors
/// Returns a [`GitError`] if the `git ls-remote` command fails.
pub(crate) async fn remote_branch_exists(
    repo_path: PathBuf,
    remote_branch_name: String,
) -> Result<bool, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    remote_branch_exists_with_runner(repo_path, remote_branch_name, &command_runner).await
}

/// Pushes one explicit branch through an injected asynchronous command
/// boundary.
async fn push_current_branch_to_remote_branch_with_runner(
    repo_path: PathBuf,
    remote_branch_name: String,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let remote_name = current_branch_remote_name(&repo_path, command_runner)
        .await?
        .unwrap_or_else(|| "origin".to_string());
    let push_refspec = format!("HEAD:{remote_branch_name}");
    let arguments = vec![
        "push".to_string(),
        "--force-with-lease".to_string(),
        "--set-upstream".to_string(),
        remote_name.clone(),
        push_refspec,
    ];
    run_git_command_with_runner(
        AsyncGitCommand::new(repo_path, arguments),
        "Git push failed",
        command_runner,
    )
    .await?;

    Ok(format!("{remote_name}/{remote_branch_name}"))
}

/// Pushes one explicit branch while requiring its remote ref to be absent.
async fn push_current_branch_to_new_remote_branch_with_runner(
    repo_path: PathBuf,
    remote_branch_name: String,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let remote_name = current_branch_remote_name(&repo_path, command_runner)
        .await?
        .unwrap_or_else(|| "origin".to_string());
    let remote_ref = format!("refs/heads/{remote_branch_name}");
    let lease_argument = format!("--force-with-lease={remote_ref}:");
    let push_refspec = format!("HEAD:{remote_branch_name}");
    let arguments = vec![
        "push".to_string(),
        lease_argument,
        "--set-upstream".to_string(),
        remote_name.clone(),
        push_refspec,
    ];
    run_git_command_with_runner(
        AsyncGitCommand::new(repo_path, arguments),
        "Git push failed",
        command_runner,
    )
    .await?;

    Ok(format!("{remote_name}/{remote_branch_name}"))
}

/// Checks a remote branch through an injected asynchronous command boundary.
async fn remote_branch_exists_with_runner(
    repo_path: PathBuf,
    remote_branch_name: String,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<bool, GitError> {
    let remote_name = current_branch_remote_name(&repo_path, command_runner)
        .await?
        .unwrap_or_else(|| "origin".to_string());
    let arguments = vec![
        "ls-remote".to_string(),
        "--heads".to_string(),
        remote_name,
        remote_branch_name,
    ];
    let stdout = run_git_command_with_runner(
        AsyncGitCommand::new(repo_path, arguments),
        "Git ls-remote failed",
        command_runner,
    )
    .await?;

    Ok(!stdout.trim().is_empty())
}

/// Returns the current upstream reference for `HEAD`.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
///
/// # Returns
/// The configured upstream reference, for example `origin/main`.
///
/// # Errors
/// Returns a [`GitError`] when upstream tracking information cannot be
/// resolved.
pub(crate) async fn current_upstream_reference(repo_path: PathBuf) -> Result<String, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    primary_upstream_reference(&repo_path, &command_runner).await
}

/// Fetches from the configured remote.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Returns
/// Ok(()) on success.
///
/// # Errors
/// Returns a [`GitError`] if `git fetch` cannot be executed successfully.
pub(crate) async fn fetch_remote(repo_path: PathBuf) -> Result<(), GitError> {
    run_git_command(
        repo_path,
        vec!["fetch".to_string()],
        "Git fetch failed".to_string(),
    )
    .await?;

    Ok(())
}

/// Returns the number of commits ahead and behind the upstream branch.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Returns
/// Ok((ahead, behind)) on success.
///
/// # Errors
/// Returns a [`GitError`] if `git rev-list` fails or returns unexpected
/// output.
pub(crate) async fn get_ahead_behind(repo_path: PathBuf) -> Result<(u32, u32), GitError> {
    get_ref_ahead_behind(repo_path, "HEAD".to_string(), "@{u}".to_string()).await
}

/// Returns the number of commits `left_ref` is ahead of and behind `right_ref`.
///
/// The returned tuple is `(ahead, behind)`, where `ahead` counts commits
/// reachable from `left_ref` but not `right_ref`, and `behind` counts commits
/// reachable from `right_ref` but not `left_ref`.
///
/// # Errors
/// Returns a [`GitError`] if `git rev-list` fails or returns unexpected
/// output.
pub(crate) async fn get_ref_ahead_behind(
    repo_path: PathBuf,
    left_ref: String,
    right_ref: String,
) -> Result<(u32, u32), GitError> {
    let rev_list_output = run_git_command(
        repo_path,
        vec![
            "rev-list".to_string(),
            "--left-right".to_string(),
            "--count".to_string(),
            format!("{left_ref}...{right_ref}"),
        ],
        "Git rev-list failed".to_string(),
    )
    .await?;

    parse_ahead_behind_counts(&rev_list_output)
}

/// Parses one `git rev-list --left-right --count` output into `(ahead,
/// behind)`.
fn parse_ahead_behind_counts(rev_list_output: &str) -> Result<(u32, u32), GitError> {
    let parts: Vec<&str> = rev_list_output.split_whitespace().collect();
    if parts.len() >= 2 {
        let ahead = parts[0].parse().unwrap_or(0);
        let behind = parts[1].parse().unwrap_or(0);

        return Ok((ahead, behind));
    }

    Err(GitError::OutputParse(
        "Unexpected output format from git rev-list".to_string(),
    ))
}

/// Returns ahead/behind snapshots for every local branch in `repo_path`.
///
/// The returned map is keyed by local branch name. Branches without an
/// upstream, with a gone upstream, or without ahead/behind markers map to
/// `None`.
///
/// # Errors
/// Returns a [`GitError`] if `git for-each-ref` fails.
pub(crate) async fn branch_tracking_statuses(
    repo_path: PathBuf,
) -> Result<BranchTrackingMap, GitError> {
    let git_output = run_git_command(
        repo_path,
        vec![
            "for-each-ref".to_string(),
            "--format=%(refname:short)\t%(upstream:short)\t%(upstream:track,nobracket)".to_string(),
            "refs/heads".to_string(),
        ],
        "Git for-each-ref failed".to_string(),
    )
    .await?;

    Ok(parse_branch_tracking_statuses(&git_output))
}

/// Returns upstream commit subjects that are not yet in local `HEAD`.
///
/// The returned order is oldest to newest to match pull application order.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Errors
/// Returns a [`GitError`] when `git log` fails or upstream tracking refs are
/// unavailable.
pub(crate) async fn list_upstream_commit_titles(
    repo_path: PathBuf,
) -> Result<Vec<String>, GitError> {
    let git_output = run_git_command(
        repo_path,
        vec![
            "log".to_string(),
            "--reverse".to_string(),
            "--pretty=%s".to_string(),
            "HEAD..@{u}".to_string(),
        ],
        "Git log failed".to_string(),
    )
    .await?;

    Ok(parse_commit_titles(&git_output))
}

/// Returns local commit subjects that are not yet present in upstream.
///
/// The returned order is oldest to newest to match push application order.
///
/// # Arguments
/// * `repo_path` - Path to the git repository root
///
/// # Errors
/// Returns a [`GitError`] when `git log` fails or upstream tracking refs are
/// unavailable.
pub(crate) async fn list_local_commit_titles(repo_path: PathBuf) -> Result<Vec<String>, GitError> {
    let git_output = run_git_command(
        repo_path,
        vec![
            "log".to_string(),
            "--reverse".to_string(),
            "--pretty=%s".to_string(),
            "@{u}..HEAD".to_string(),
        ],
        "Git log failed".to_string(),
    )
    .await?;

    Ok(parse_commit_titles(&git_output))
}

/// Returns whether `HEAD` contains commits that are not reachable from
/// `base_branch`.
///
/// # Errors
/// Returns a [`GitError`] if commit ancestry cannot be queried.
pub(crate) async fn has_commits_since(
    repo_path: PathBuf,
    base_branch: String,
) -> Result<bool, GitError> {
    spawn_blocking(move || -> Result<bool, GitError> {
        let rev_list_output = run_git_command_sync(
            &repo_path,
            &["rev-list", "--count", &format!("{base_branch}..HEAD")],
            "Failed to count commits since base branch",
        )?;
        let commit_count = rev_list_output.trim().parse::<u32>().map_err(|error| {
            GitError::OutputParse(format!(
                "Failed to parse commit count since base branch `{base_branch}`: {error}"
            ))
        })?;

        Ok(commit_count > 0)
    })
    .await?
}

/// Parses newline-delimited commit subjects from `git log` output.
fn parse_commit_titles(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Parses repo-wide branch tracking information from `git for-each-ref`.
fn parse_branch_tracking_statuses(output: &str) -> BranchTrackingMap {
    let mut branch_tracking_statuses = HashMap::new();

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut parts = line.splitn(3, '\t');
        let Some(branch_name) = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let upstream_ref = parts.next().map(str::trim).unwrap_or_default();
        let track = parts.next().map(str::trim).unwrap_or_default();

        let status = if upstream_ref.is_empty() {
            None
        } else {
            parse_branch_tracking_counts(track)
        };
        branch_tracking_statuses.insert(branch_name.to_string(), status);
    }

    branch_tracking_statuses
}

/// Parses one `%(upstream:track,nobracket)` marker into ahead/behind counts.
fn parse_branch_tracking_counts(track: &str) -> Option<(u32, u32)> {
    let normalized_track = track.trim();
    if normalized_track.is_empty() || normalized_track == "gone" {
        return None;
    }

    let mut ahead = 0;
    let mut behind = 0;

    for part in normalized_track.split(',').map(str::trim) {
        if let Some(count) = part.strip_prefix("ahead ") {
            ahead = count.parse().ok()?;
        } else if let Some(count) = part.strip_prefix("behind ") {
            behind = count.parse().ok()?;
        }
    }

    Some((ahead, behind))
}

/// Resolves the commit/tree to use as the `git diff` "before" side.
///
/// Starts from the merge-base fallback and, when `git cherry` reports leading
/// commits already applied to `base_branch`, advances the baseline to the last
/// such commit so squash-merged session changes are not shown again.
fn resolve_diff_target(
    repo_path: &Path,
    base_branch: &str,
    merge_base: &str,
) -> Result<String, GitError> {
    let cherry_output = run_git_command_output_sync(repo_path, &["cherry", base_branch, "HEAD"])?;
    if !cherry_output.status.success() {
        return Ok(merge_base.to_string());
    }

    let cherry_stdout = String::from_utf8_lossy(&cherry_output.stdout);
    let Some(last_leading_applied_commit) = last_leading_applied_commit(&cherry_stdout) else {
        return Ok(merge_base.to_string());
    };

    Ok(last_leading_applied_commit.to_string())
}

/// Returns the last leading commit from `git cherry` marked as already applied.
///
/// `git cherry` prefixes commits with `-` when an equivalent patch exists in
/// the upstream branch and `+` when it does not. This helper only consumes the
/// initial contiguous `-` block and stops at the first `+` to avoid dropping
/// non-merged changes.
fn last_leading_applied_commit(cherry_output: &str) -> Option<&str> {
    let mut last_applied_commit = None;

    for line in cherry_output.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        let mut parts = trimmed_line.split_whitespace();
        let marker = parts.next()?;
        let commit_hash = parts.next()?;

        if marker == "-" {
            last_applied_commit = Some(commit_hash);

            continue;
        }

        if marker == "+" {
            break;
        }

        break;
    }

    last_applied_commit
}

/// Stages all changes and commits or amends with retry behavior for hook
/// rewrites.
///
/// If an amend would make `HEAD` empty, the staged tree has reverted the
/// session commit back to its parent. In that case the helper drops the now
/// empty session commit and reports the standard no-changes sentinel so the
/// app can skip model-assisted commit recovery.
async fn commit_all_with_retry(
    repo_path: PathBuf,
    commit_message: String,
    message_strategy: SingleCommitMessageStrategy,
    amend_existing_commit: bool,
) -> Result<(), GitError> {
    spawn_blocking(move || {
        stage_all_sync(&repo_path)?;

        for _ in 0..COMMIT_ALL_HOOK_RETRY_ATTEMPTS {
            let output = run_commit_command(
                &repo_path,
                &commit_message,
                message_strategy,
                amend_existing_commit,
            )?;

            if output.status.success() {
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            if is_nothing_to_commit_output(&stdout, &stderr) {
                return Err(nothing_to_commit_error());
            }

            if amend_existing_commit && is_empty_amend_output(&stdout, &stderr) {
                reset_empty_amend_sync(&repo_path)?;

                return Err(nothing_to_commit_error());
            }

            if is_hook_modified_error(&stdout, &stderr) {
                stage_all_sync(&repo_path)?;

                continue;
            }

            let detail = command_output_detail(&output.stdout, &output.stderr);

            return Err(GitError::CommandFailed {
                command: "git commit".to_string(),
                stderr: detail,
            });
        }

        Err(GitError::CommandFailed {
            command: "git commit".to_string(),
            stderr: format!(
                "Failed to commit: commit hooks kept modifying files after \
                 {COMMIT_ALL_HOOK_RETRY_ATTEMPTS} attempts"
            ),
        })
    })
    .await?
}

/// Ensures repositories declaring pre-commit validation have an executable
/// hook.
fn ensure_pre_commit_hook_ready(repo_path: &Path) -> Result<(), GitError> {
    let Some(config_file) = PRE_COMMIT_CONFIG_FILES
        .iter()
        .find(|config_file| repo_path.join(config_file).is_file())
    else {
        return Ok(());
    };
    let hook_path = resolve_pre_commit_hook_path(repo_path)?;

    if is_executable_hook(&hook_path) {
        return Ok(());
    }

    Err(GitError::PreCommitHookMissing {
        config_file: (*config_file).to_string(),
    })
}

/// Resolves the pre-commit hook using `core.hooksPath` or Git's default path.
fn resolve_pre_commit_hook_path(repo_path: &Path) -> Result<PathBuf, GitError> {
    let hooks_path_output =
        run_git_command_output_sync(repo_path, &["config", "--path", "--get", "core.hooksPath"])?;
    let hooks_path = if hooks_path_output.status.success() {
        PathBuf::from(String::from_utf8_lossy(&hooks_path_output.stdout).trim())
    } else if hooks_path_output.status.code() == Some(1) {
        let default_hook_path = run_git_command_sync(
            repo_path,
            &["rev-parse", "--git-path", "hooks/pre-commit"],
            "Failed to resolve Git pre-commit hook path",
        )?;

        return Ok(resolve_repo_path(
            repo_path,
            PathBuf::from(default_hook_path.trim()),
        ));
    } else {
        return Err(GitError::CommandFailed {
            command: "git config --path --get core.hooksPath".to_string(),
            stderr: command_output_detail(&hooks_path_output.stdout, &hooks_path_output.stderr),
        });
    };

    Ok(resolve_repo_path(repo_path, hooks_path).join("pre-commit"))
}

fn resolve_repo_path(repo_path: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }

    repo_path.join(path)
}

#[cfg(unix)]
fn is_executable_hook(hook_path: &Path) -> bool {
    hook_path.is_file() && rustix_fs::access(hook_path, Access::EXEC_OK).is_ok()
}

#[cfg(not(unix))]
fn is_executable_hook(hook_path: &Path) -> bool {
    hook_path.is_file()
}

/// Returns the canonical git no-changes error used by app auto-commit flows.
fn nothing_to_commit_error() -> GitError {
    GitError::CommandFailed {
        command: "git commit".to_string(),
        stderr: "Nothing to commit: no changes detected".to_string(),
    }
}

/// Returns whether commit output reports that there was no staged work to
/// commit.
fn is_nothing_to_commit_output(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    combined.contains("nothing to commit")
}

/// Returns whether commit output reports that amending `HEAD` would remove the
/// session commit entirely.
fn is_empty_amend_output(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    let normalized = combined.split_whitespace().collect::<Vec<_>>().join(" ");

    normalized.contains("would make it empty") && normalized.contains("allow-empty")
}

/// Drops an amended session commit whose resulting tree would match its
/// parent, leaving the worktree at the reverted state.
fn reset_empty_amend_sync(repo_path: &Path) -> Result<(), GitError> {
    run_git_command_sync(
        repo_path,
        &["reset", "HEAD^"],
        "Git reset after empty amend failed",
    )?;

    Ok(())
}

/// Stages all changed files in the repository.
///
/// Uses shared git retry behavior for transient `index.lock` contention.
fn stage_all_sync(repo_path: &Path) -> Result<(), GitError> {
    let output = run_git_command_with_index_lock_retry(repo_path, &["add", "-A"], &[])?;

    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(GitError::CommandFailed {
            command: "git add -A".to_string(),
            stderr: format!("Failed to stage changes: {detail}"),
        });
    }

    Ok(())
}

/// Returns the full `HEAD` commit message, or `None` when no commits exist.
fn head_commit_message_sync(repo_path: &Path) -> Result<Option<String>, GitError> {
    if !has_head_commit_sync(repo_path)? {
        return Ok(None);
    }

    let output = run_git_command_sync(
        repo_path,
        &["log", "-1", "--pretty=%B"],
        "Failed to read HEAD commit message",
    )?;

    Ok(Some(output.trim().to_string()))
}

/// Returns whether `HEAD` resolves to an existing commit.
fn has_head_commit_sync(repo_path: &Path) -> Result<bool, GitError> {
    let output = run_git_command_output_sync(repo_path, &["rev-parse", "--verify", "HEAD"])?;

    if output.status.success() {
        return Ok(true);
    }

    let detail = command_output_detail(&output.stdout, &output.stderr);
    let normalized_detail = detail.to_ascii_lowercase();
    if normalized_detail.contains("needed a single revision")
        || normalized_detail.contains("unknown revision")
        || normalized_detail.contains("does not have any commits yet")
    {
        return Ok(false);
    }

    Err(GitError::CommandFailed {
        command: "git rev-parse --verify HEAD".to_string(),
        stderr: detail,
    })
}

/// Runs `git commit` with optional amend and hook settings.
///
/// Uses shared git retry behavior for transient `index.lock` contention.
fn run_commit_command(
    repo_path: &Path,
    commit_message: &str,
    message_strategy: SingleCommitMessageStrategy,
    amend_existing_commit: bool,
) -> Result<Output, GitError> {
    let mut args = vec!["commit"];
    if amend_existing_commit {
        args.push("--amend");
        match message_strategy {
            SingleCommitMessageStrategy::Replace => {
                args.push("-m");
                args.push(commit_message);
            }
            SingleCommitMessageStrategy::Reuse => {
                args.push("--no-edit");
            }
        }
    } else {
        args.push("-m");
        args.push(commit_message);
    }

    run_git_command_with_index_lock_retry(repo_path, &args, &[])
}

/// Returns whether commit output indicates hooks rewrote files.
fn is_hook_modified_error(stdout: &str, stderr: &str) -> bool {
    let combined = format!(
        "{stdout}
{stderr}"
    )
    .to_ascii_lowercase();

    combined.contains("files were modified by this hook")
}

/// Returns whether git push output indicates a missing upstream branch.
pub(super) fn is_no_upstream_error(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("has no upstream branch")
        || normalized_detail.contains("no upstream branch")
        || normalized_detail.contains("set-upstream")
}

#[cfg(test)]
#[path = "sync_test.rs"]
mod tests;
