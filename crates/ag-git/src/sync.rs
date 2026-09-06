use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
#[cfg(unix)]
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

#[cfg(unix)]
use rustix::fs::{self as rustix_fs, Access};
use tokio::sync::Semaphore;
use tokio::task::spawn_blocking;
use tokio::time;

use super::error::GitError;
use super::rebase::{
    GIT_INDEX_LOCK_RETRY_ATTEMPTS, GIT_INDEX_LOCK_RETRY_DELAY, is_git_index_lock_error,
    is_rebase_conflict, run_git_command_with_index_lock_retry,
};
use super::repo::{
    AsyncGitCommand, AsyncGitCommandOutput, AsyncGitCommandRunner, ProcessAsyncGitCommandRunner,
    command_output_detail, run_git_command, run_git_command_bytes_with_runner,
    run_git_command_output_sync, run_git_command_sync, run_git_command_with_runner,
};

/// Map of local branch names to their ahead/behind counts relative to their
/// tracked upstream branch. `None` indicates no upstream or a gone upstream.
pub type BranchTrackingMap = HashMap<String, Option<(u32, u32)>>;

const COMMIT_ALL_HOOK_RETRY_ATTEMPTS: usize = 5;
const INDEX_COPY_TIMEOUT: Duration = Duration::from_secs(30);
/// Four path-hashed lanes bound blocking index copies across all requests.
/// Repeated copies of the same path share a lane, including after cancellation.
static INDEX_COPY_LANES: LazyLock<[Arc<Semaphore>; 4]> =
    LazyLock::new(|| std::array::from_fn(|_| Arc::new(Semaphore::new(1))));
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
    let index_path = resolve_diff_index_path(&repo_path, &ProcessAsyncGitCommandRunner).await?;
    let index_path = if index_path.is_absolute() {
        index_path
    } else {
        repo_path.join(index_path)
    };

    diff_output_after_index_resolution(&repo_path, &base_branch, name_only, &index_path).await
}

/// Generates diff output with cancellable, deadline-bound subprocesses. Only
/// copying the isolated index runs on a bounded blocking lane.
async fn diff_output_after_index_resolution(
    repo_path: &Path,
    base_branch: &str,
    name_only: bool,
    index_path: &Path,
) -> Result<String, GitError> {
    let result = async {
        let temporary_index = copy_git_index_with_lane(
            index_path.to_path_buf(),
            index_copy_lane(index_path),
            INDEX_COPY_TIMEOUT,
            copy_git_index_to_temp,
        )
        .await?;
        run_git_command_with_index(
            repo_path,
            &["add", "-A", "--intent-to-add"],
            &temporary_index,
            "Git add --intent-to-add failed",
        )
        .await?;

        let merge_base_output = ProcessAsyncGitCommandRunner
            .run(AsyncGitCommand::new(
                repo_path.to_path_buf(),
                vec!["merge-base".into(), "HEAD".into(), base_branch.into()],
            ))
            .await?;
        let diff_target = if merge_base_output.success() {
            resolve_diff_target(
                repo_path,
                base_branch,
                String::from_utf8_lossy(&merge_base_output.stdout).trim(),
            )
            .await?
        } else {
            base_branch.to_string()
        };
        let args = if name_only {
            vec!["diff", "--name-only", diff_target.as_str()]
        } else {
            vec!["diff", diff_target.as_str()]
        };

        run_git_command_with_index(repo_path, &args, &temporary_index, "Git diff failed").await
    }
    .await;

    match result {
        Ok(output) => Ok(output),
        Err(error) => Err(classify_diff_repository_error(repo_path, error).await),
    }
}

/// Resolves the native index path and classifies a reclaimed repository.
async fn resolve_diff_index_path(
    repo_path: &Path,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<PathBuf, GitError> {
    let result = run_git_command_bytes_with_runner(
        AsyncGitCommand::new(
            repo_path.to_path_buf(),
            vec!["rev-parse".into(), "--git-path".into(), "index".into()],
        ),
        "Git index path resolution failed",
        command_runner,
    )
    .await
    .and_then(parse_diff_index_path);

    match result {
        Ok(path) => Ok(path),
        Err(error) => Err(classify_diff_repository_error(repo_path, error).await),
    }
}

/// Removes only Git's output terminator, preserving native path bytes and
/// whitespace that belongs to the filename. Non-Unix platforms reject invalid
/// UTF-8 rather than silently substituting a different path.
fn parse_diff_index_path(mut stdout: Vec<u8>) -> Result<PathBuf, GitError> {
    if stdout.ends_with(b"\n") {
        stdout.truncate(stdout.len() - 1);
        #[cfg(not(unix))]
        if stdout.ends_with(b"\r") {
            stdout.truncate(stdout.len() - 1);
        }
    }
    if stdout.is_empty() {
        return Err(GitError::OutputParse("Git index path is empty".to_string()));
    }

    #[cfg(unix)]
    let path = OsString::from_vec(stdout);
    #[cfg(not(unix))]
    let path = String::from_utf8(stdout)
        .map_err(|error| GitError::OutputParse(format!("Invalid Git index path: {error}")))?;

    Ok(PathBuf::from(path))
}

/// Preserves ordinary Git failures while typing unavailable repository paths.
async fn classify_diff_repository_error(repo_path: &Path, error: GitError) -> GitError {
    let probe = ProcessAsyncGitCommandRunner
        .run(AsyncGitCommand::new(
            repo_path.to_path_buf(),
            vec!["rev-parse".into(), "--git-dir".into()],
        ))
        .await;
    let missing_directory = !tokio::fs::try_exists(repo_path).await.unwrap_or(true);
    if !missing_directory && !diff_repository_probe_is_unavailable(probe) {
        return error;
    }

    GitError::RepositoryUnavailable {
        detail: error.to_string(),
    }
}

/// Treats only a completed, unsuccessful discovery probe as unavailable.
fn diff_repository_probe_is_unavailable(probe: Result<AsyncGitCommandOutput, GitError>) -> bool {
    probe.is_ok_and(|output| !output.success())
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

/// Selects a stable lane so the same index cannot accumulate blocking copies.
/// Hash collisions serialize unrelated indexes, keeping the registry bounded.
fn index_copy_lane(index_path: &Path) -> Arc<Semaphore> {
    let mut hasher = DefaultHasher::new();
    index_path.hash(&mut hasher);
    let lane = usize::from(hasher.finish().to_le_bytes()[0]) % INDEX_COPY_LANES.len();

    Arc::clone(&INDEX_COPY_LANES[lane])
}

/// Bounds both queueing and waiting for a copy. Blocking filesystem calls
/// cannot be aborted, so the closure retains its lane after caller cancellation
/// or timeout until the copy actually exits. Waiting requests remain async and
/// cancelable without spawning more blocking work.
async fn copy_git_index_with_lane(
    index_path: PathBuf,
    lane: Arc<Semaphore>,
    timeout: Duration,
    copy: impl FnOnce(&Path) -> Result<tempfile::TempPath, GitError> + Send + 'static,
) -> Result<tempfile::TempPath, GitError> {
    time::timeout(timeout, async {
        let permit = lane
            .acquire_owned()
            .await
            .map_err(|error| GitError::Io(std::io::Error::other(error)))?;

        spawn_blocking(move || {
            let _permit = permit;

            copy(&index_path)
        })
        .await?
    })
    .await
    .map_err(|_| GitError::CommandTimedOut {
        command: "copy git index".to_string(),
        timeout,
    })?
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
async fn run_git_command_with_index(
    repo_path: &Path,
    args: &[&str],
    index_path: &Path,
    error_context: &str,
) -> Result<String, GitError> {
    run_git_command_with_runner(
        AsyncGitCommand::new(
            repo_path.to_path_buf(),
            args.iter().map(|arg| (*arg).to_string()).collect(),
        )
        .with_environment(vec![(
            "GIT_INDEX_FILE".into(),
            index_path.as_os_str().to_os_string(),
        )]),
        error_context,
        &ProcessAsyncGitCommandRunner,
    )
    .await
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
        ("GIT_EDITOR".into(), ":".into()),
        ("GIT_SEQUENCE_EDITOR".into(), ":".into()),
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
async fn resolve_diff_target(
    repo_path: &Path,
    base_branch: &str,
    merge_base: &str,
) -> Result<String, GitError> {
    let cherry_output = ProcessAsyncGitCommandRunner
        .run(AsyncGitCommand::new(
            repo_path.to_path_buf(),
            vec!["cherry".into(), base_branch.into(), "HEAD".into()],
        ))
        .await?;
    if !cherry_output.success() {
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
mod tests {
    use std::fs;
    use std::future::{Future, poll_fn};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;

    use mockall::Sequence;
    use mockall::predicate::function;
    use tempfile::tempdir;

    use super::*;
    use crate::repo::MockAsyncGitCommandRunner;

    /// Builds captured asynchronous git output for command-runner tests.
    fn async_git_output(
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
    fn run_git_command(repo_path: &Path, args: &[&str]) {
        let output = git_command_output(repo_path, args);

        assert!(
            output.status.success(),
            "git command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Runs `git` in `repo_path` and returns the captured command output.
    fn git_command_output(repo_path: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .expect("failed to run git command")
    }

    /// Runs `git` in `repo_path`, asserts success, and returns trimmed stdout.
    fn git_command_stdout(repo_path: &Path, args: &[&str]) -> String {
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
    fn setup_test_git_repo(repo_path: &Path) {
        run_git_command(repo_path, &["init", "-b", "main"]);
        run_git_command(repo_path, &["config", "user.name", "Test User"]);
        run_git_command(repo_path, &["config", "user.email", "test@example.com"]);
        fs::write(repo_path.join("README.md"), "base\n").expect("failed to write base file");
        run_git_command(repo_path, &["add", "README.md"]);
        run_git_command(repo_path, &["commit", "-m", "Initial commit"]);
    }

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
        let changed_files =
            diff_changed_files(temp_dir.path().to_path_buf(), "main".to_string()).await;

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

    #[cfg(unix)]
    #[tokio::test]
    async fn index_resolution_preserves_native_bytes_and_filename_whitespace() {
        // Arrange
        let path_bytes = b" /repo-\xff/.git/worktrees/topic/index \t\r";
        let mut stdout = path_bytes.to_vec();
        stdout.push(b'\n');
        let mut runner = MockAsyncGitCommandRunner::new();
        runner
            .expect_run()
            .once()
            .with(function(|command: &AsyncGitCommand| {
                command.arguments == ["rev-parse", "--git-path", "index"]
            }))
            .return_once(move |_| {
                Box::pin(async move { Ok(async_git_output(0, stdout, Vec::new())) })
            });

        // Act
        let path = resolve_diff_index_path(Path::new("worktree"), &runner).await;

        // Assert
        assert_eq!(
            path.expect("native index path"),
            PathBuf::from(OsString::from_vec(path_bytes.to_vec()))
        );
    }

    #[test]
    fn index_path_parser_preserves_paths_with_and_without_output_terminators() {
        // Arrange
        let cases = [
            (b"relative index\n".to_vec(), "relative index"),
            (b"relative index".to_vec(), "relative index"),
            (b" index \t\n".to_vec(), " index \t"),
            (b"index\n\n".to_vec(), "index\n"),
        ];

        for (stdout, expected) in cases {
            // Act
            let path = parse_diff_index_path(stdout);

            // Assert
            assert_eq!(path.expect("index path"), PathBuf::from(expected));
        }
    }

    #[test]
    fn index_path_parser_rejects_empty_output() {
        // Arrange
        for stdout in [Vec::new(), b"\n".to_vec()] {
            // Act
            let result = parse_diff_index_path(stdout);

            // Assert
            assert!(matches!(result, Err(GitError::OutputParse(_))));
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn index_path_parser_handles_crlf_and_rejects_invalid_utf8() {
        // Arrange
        let valid = b"index\r\n".to_vec();
        let invalid = b"index-\xff\n".to_vec();

        // Act
        let path = parse_diff_index_path(valid);
        let error = parse_diff_index_path(invalid);

        // Assert
        assert_eq!(path.expect("CRLF-terminated path"), PathBuf::from("index"));
        assert!(matches!(error, Err(GitError::OutputParse(_))));
    }

    #[tokio::test]
    async fn diff_in_linked_worktree_preserves_native_repository_paths() {
        // Arrange
        let temp_dir = tempdir().expect("temporary directory");
        let initial_repo_path = temp_dir.path().join("initial-repo");
        fs::create_dir(&initial_repo_path).expect("initial repository directory");
        setup_test_git_repo(&initial_repo_path);

        // Linux exercises a real non-UTF-8 repository name. APFS cannot create
        // such names; other platforms exercise the same linked-worktree flow
        // with spaces, while the Unix runner test checks arbitrary path bytes.
        #[cfg(target_os = "linux")]
        let repo_path = temp_dir
            .path()
            .join(OsString::from_vec(b"repo-\xff".to_vec()));
        #[cfg(not(target_os = "linux"))]
        let repo_path = temp_dir.path().join("repo with spaces");
        fs::rename(initial_repo_path, &repo_path).expect("native repository directory");
        let worktree_path = temp_dir.path().join("linked-worktree");
        crate::create_worktree(
            repo_path.clone(),
            worktree_path.clone(),
            "topic".to_string(),
            "main".to_string(),
        )
        .await
        .expect("linked worktree");
        fs::write(worktree_path.join("README.md"), "staged change\n").expect("staged content");
        run_git_command(&worktree_path, &["add", "README.md"]);
        fs::write(
            worktree_path.join("README.md"),
            "staged change\nunstaged change\n",
        )
        .expect("unstaged content");
        fs::write(worktree_path.join("new.txt"), "untracked change\n").expect("untracked content");
        let main_index = repo_path.join(".git/index");
        let linked_index = repo_path.join(".git/worktrees/linked-worktree/index");
        let main_index_before = fs::read(&main_index).expect("main index");
        let linked_index_before = fs::read(&linked_index).expect("linked index");

        // Act
        let patch = diff(worktree_path.clone(), "main".to_string()).await;
        let paths = diff_changed_files(worktree_path, "main".to_string()).await;

        // Assert
        let patch = patch.expect("linked-worktree diff");
        assert!(patch.contains("staged change"));
        assert!(patch.contains("unstaged change"));
        assert!(patch.contains("untracked change"));
        assert_eq!(paths.expect("changed paths"), ["README.md", "new.txt"]);
        assert_eq!(fs::read(main_index).expect("main index"), main_index_before);
        assert_eq!(
            fs::read(linked_index).expect("linked index"),
            linked_index_before
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn diff_preserves_non_utf8_index_paths_and_the_real_index() {
        // Arrange
        let temp_dir = tempdir().expect("temporary directory");
        setup_test_git_repo(temp_dir.path());
        let index_path = PathBuf::from(std::ffi::OsString::from_vec(b"repo-\xff/index".to_vec()));
        let index_before = fs::read(temp_dir.path().join(".git/index")).expect("real index");
        // Inspect the path bytes inside a real Git child. This also works on
        // filesystems that cannot create non-UTF-8 filenames, including APFS.
        let arguments = [
            "-c",
            r#"alias.check-index=!test "$GIT_INDEX_FILE" = "$(printf 'repo-\377/index')""#,
            "check-index",
        ];

        // Act
        let result = run_git_command_with_index(
            temp_dir.path(),
            &arguments,
            &index_path,
            "Git index path bytes changed",
        )
        .await;

        // Assert
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            fs::read(temp_dir.path().join(".git/index")).expect("real index"),
            index_before,
        );
    }

    /// Builds a copy callback that records when blocking work begins.
    fn recording_index_copy(
        started: Arc<AtomicBool>,
    ) -> impl FnOnce(&Path) -> Result<tempfile::TempPath, GitError> {
        move |path| {
            started.store(true, Ordering::SeqCst);

            copy_git_index_to_temp(path)
        }
    }

    #[tokio::test]
    async fn canceled_index_copy_retains_its_lane_and_discards_waiting_copies() {
        // Arrange
        let temp_dir = tempdir().expect("temporary directory");
        let index_path = temp_dir.path().join("index");
        fs::write(&index_path, "index content").expect("index fixture");
        let lane = index_copy_lane(&index_path);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let request = tokio::spawn(copy_git_index_with_lane(
            index_path.clone(),
            Arc::clone(&lane),
            INDEX_COPY_TIMEOUT,
            move |path| {
                started_tx.send(()).expect("copy started");
                release_rx.recv().expect("release copy");

                copy_git_index_to_temp(path)
            },
        ));
        started_rx.await.expect("blocking copy started");
        let duplicate_started = Arc::new(AtomicBool::new(false));
        let resumed_copy_started = Arc::new(AtomicBool::new(false));

        // Act
        request.abort();
        let cancellation = request.await;
        let mut duplicate = Box::pin(copy_git_index_with_lane(
            index_path.clone(),
            index_copy_lane(&index_path),
            INDEX_COPY_TIMEOUT,
            recording_index_copy(Arc::clone(&duplicate_started)),
        ));
        poll_fn(|context| {
            assert!(duplicate.as_mut().poll(context).is_pending());

            Poll::Ready(())
        })
        .await;
        drop(duplicate);
        let lane_was_held = lane.try_acquire().is_err();
        release_tx.send(()).expect("release original copy");
        let next_copy = copy_git_index_with_lane(
            index_path,
            lane,
            Duration::from_secs(1),
            recording_index_copy(Arc::clone(&resumed_copy_started)),
        )
        .await;

        // Assert
        assert!(resumed_copy_started.load(Ordering::SeqCst));
        assert!(cancellation.expect_err("request canceled").is_cancelled());
        assert!(lane_was_held);
        assert!(!duplicate_started.load(Ordering::SeqCst));
        assert_eq!(
            fs::read(next_copy.expect("lane reusable")).expect("copied index"),
            b"index content"
        );
    }

    #[tokio::test]
    async fn closed_index_copy_lane_returns_an_error_without_copying() {
        // Arrange
        let lane = Arc::new(Semaphore::new(1));
        lane.close();

        // Act
        let result = copy_git_index_with_lane(
            PathBuf::from("index"),
            lane,
            INDEX_COPY_TIMEOUT,
            copy_git_index_to_temp,
        )
        .await;

        // Assert
        assert!(matches!(result, Err(GitError::Io(_))));
    }

    #[tokio::test]
    async fn index_copy_deadline_bounds_running_and_queued_requests() {
        // Arrange
        let temp_dir = tempdir().expect("temporary directory");
        let index_path = temp_dir.path().join("index");
        fs::write(&index_path, "index content").expect("index fixture");
        let lane = Arc::new(Semaphore::new(1));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let request = tokio::spawn(copy_git_index_with_lane(
            index_path.clone(),
            Arc::clone(&lane),
            Duration::from_millis(25),
            move |path| {
                started_tx.send(()).expect("copy started");
                release_rx.recv().expect("release copy");

                copy_git_index_to_temp(path)
            },
        ));
        started_rx.await.expect("blocking copy started");
        let duplicate_started = Arc::new(AtomicBool::new(false));

        // Act
        let running_result = request.await.expect("copy request joined");
        let queued_result = copy_git_index_with_lane(
            index_path,
            Arc::clone(&lane),
            Duration::from_millis(25),
            recording_index_copy(Arc::clone(&duplicate_started)),
        )
        .await;
        let lane_was_held = lane.try_acquire().is_err();
        release_tx.send(()).expect("release timed-out copy");
        let released = time::timeout(Duration::from_secs(1), lane.acquire()).await;

        // Assert
        assert!(matches!(
            running_result,
            Err(GitError::CommandTimedOut { .. })
        ));
        assert!(matches!(
            queued_result,
            Err(GitError::CommandTimedOut { .. })
        ));
        assert!(lane_was_held);
        assert!(!duplicate_started.load(Ordering::SeqCst));
        assert!(released.is_ok());
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

    #[tokio::test]
    async fn diff_reports_repository_unavailable_when_removed_after_index_resolution() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let preserved_index_dir = tempdir().expect("failed to create preserved index dir");
        setup_test_git_repo(temp_dir.path());
        let index_path = resolve_diff_index_path(temp_dir.path(), &ProcessAsyncGitCommandRunner)
            .await
            .expect("index path should resolve");
        let index_path = temp_dir.path().join(index_path);
        let preserved_index_path = preserved_index_dir.path().join("index");
        fs::copy(index_path, &preserved_index_path).expect("index copy should succeed");
        fs::remove_dir_all(temp_dir.path()).expect("worktree removal should succeed");

        // Act
        let result = diff_output_after_index_resolution(
            temp_dir.path(),
            "main",
            false,
            &preserved_index_path,
        )
        .await;

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

    #[tokio::test]
    async fn diff_repository_error_classification_preserves_unrelated_failures() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        setup_test_git_repo(temp_dir.path());
        let error = GitError::CommandFailed {
            command: "git rev-parse --git-path index".to_string(),
            stderr: "fatal: ambiguous argument".to_string(),
        };

        // Act
        let classified = classify_diff_repository_error(temp_dir.path(), error).await;

        // Assert
        assert!(matches!(
            classified,
            GitError::CommandFailed { command, stderr }
                if command == "git rev-parse --git-path index"
                    && stderr == "fatal: ambiguous argument"
        ));
    }

    #[tokio::test]
    async fn diff_repository_error_classification_ignores_localized_diagnostic() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let error = GitError::CommandFailed {
            command: "git rev-parse --git-path index".to_string(),
            stderr: "fatal: kein Git-Repository".to_string(),
        };

        // Act
        let classified = classify_diff_repository_error(temp_dir.path(), error).await;

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

    #[tokio::test]
    async fn diff_repository_error_classification_types_missing_directory() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let missing_path = temp_dir.path().join("removed-worktree");
        let error = GitError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "worktree removed",
        ));

        // Act
        let classified = classify_diff_repository_error(&missing_path, error).await;

        // Assert
        assert!(matches!(
            classified,
            GitError::RepositoryUnavailable { detail } if detail == "worktree removed"
        ));
    }

    #[tokio::test]
    async fn read_worktree_file_returns_text_for_safe_nested_path() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let docs_dir = temp_dir.path().join("docs");
        fs::create_dir(&docs_dir).expect("failed to create docs directory");
        fs::write(docs_dir.join("README.md"), "# Preview\n")
            .expect("failed to write markdown file");

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
        let missing =
            read_worktree_file(temp_dir.path().to_path_buf(), "missing.md".to_string()).await;
        let binary =
            read_worktree_file(temp_dir.path().to_path_buf(), "binary.md".to_string()).await;
        let too_large =
            read_worktree_file(temp_dir.path().to_path_buf(), "large.md".to_string()).await;

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

    #[tokio::test]
    async fn run_git_command_with_index_maps_process_and_command_failures() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let index_path = temp_dir.path().join("index");
        let missing_repo_path = temp_dir.path().join("missing-repository");
        fs::write(&index_path, []).expect("failed to create temporary index");

        // Act
        let process_error = run_git_command_with_index(
            &missing_repo_path,
            &["status"],
            &index_path,
            "Expected process failure",
        )
        .await;
        let command_error = run_git_command_with_index(
            temp_dir.path(),
            &["definitely-not-a-git-command"],
            &index_path,
            "Expected git failure",
        )
        .await;

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

    #[cfg(unix)]
    fn write_executable_pre_commit_hook(hook_path: &Path) {
        write_executable_hook(hook_path, "#!/bin/sh\nexit 0\n");
    }

    #[cfg(unix)]
    fn write_executable_hook(hook_path: &Path, contents: &str) {
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
            .return_once(|_| {
                Box::pin(async { Ok(async_git_output(0, "origin/main\n", Vec::new())) })
            });
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
                                ("GIT_EDITOR".into(), ":".into()),
                                ("GIT_SEQUENCE_EDITOR".into(), ":".into()),
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
            .return_once(|_| {
                Box::pin(async { Ok(async_git_output(0, "origin/main\n", Vec::new())) })
            });
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
            .return_once(|_| {
                Box::pin(async { Ok(async_git_output(0, "origin/main\n", Vec::new())) })
            });
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
        let exists = remote_branch_exists_with_runner(
            repo_path,
            "review/topic".to_string(),
            &command_runner,
        )
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
            error.contains("stale info")
                || error.contains("rejected")
                || error.contains("fetch first")
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
        fs::remove_file(temp_dir.path().join("session.txt"))
            .expect("failed to remove session file");

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
}
