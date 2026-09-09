//! Git client trait boundary and production adapter implementation.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use super::error::GitError;
use super::merge::SquashMergeOutcome;
use super::rebase::{InProgressGitOperation, RebaseStepResult};
use super::sync::{
    BranchTrackingMap, PullRebaseResult, SingleCommitMessageStrategy, WorktreeFileContent,
};
use super::{
    abort_rebase, branch_tracking_statuses, check_pre_commit_hook_ready, commit_all,
    commit_all_preserving_single_commit, create_worktree, current_upstream_reference,
    delete_branch, detect_git_info, diff, diff_changed_files, fetch_remote, find_git_repo_root,
    get_ahead_behind, get_ref_ahead_behind, has_commits_since, has_merge_conflicts,
    has_unmerged_paths, head_commit_message, head_hash, head_short_hash, in_progress_operation,
    is_rebase_in_progress, is_worktree_clean, list_conflicted_files, list_local_commit_titles,
    list_staged_conflict_marker_files, list_upstream_commit_titles, main_checkout_working_tree,
    main_repo_root, pull_rebase, push_current_branch, push_current_branch_to_new_remote_branch,
    push_current_branch_to_remote_branch, rebase, rebase_continue, rebase_onto_start, rebase_start,
    ref_hash, remote_branch_exists, remove_worktree, repo_url, run_pre_commit_hook, squash_merge,
    squash_merge_diff, stage_all, sync, tracked_worktree_status, worktree_status,
};

/// Boxed async result used by [`GitClient`] trait methods.
pub type GitFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Low-level async git boundary used by app orchestration code.
///
/// Production uses [`RealGitClient`], while tests can inject
/// `MockGitClient` to avoid flaky multi-command process workflows.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub trait GitClient: Send + Sync {
    /// Detects the current branch name for the repository containing `dir`.
    ///
    /// Returns `None` when `dir` is not inside a git repository or no branch
    /// can be determined.
    fn detect_git_info(&self, dir: PathBuf) -> GitFuture<Option<String>>;

    /// Resolves the repository root directory that contains `dir`.
    ///
    /// Returns `None` when `dir` is not in a git repository.
    fn find_git_repo_root(&self, dir: PathBuf) -> GitFuture<Option<PathBuf>>;

    /// Verifies that configured pre-commit validation has an executable hook.
    ///
    /// # Errors
    /// Returns an error when a supported pre-commit configuration exists but
    /// its effective Git hook is missing or cannot be executed.
    fn check_pre_commit_hook_ready(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>>;

    /// Runs the effective Git `pre-commit` hook against the current index.
    ///
    /// Missing hooks are accepted, matching normal Git commit behavior.
    ///
    /// # Errors
    /// Returns an error when Git cannot run the hook or the hook rejects the
    /// staged changes.
    fn run_pre_commit_hook(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>>;

    /// Creates a new worktree at `worktree_path` on `branch_name` from
    /// `start_ref` inside `repo_path`.
    ///
    /// # Errors
    /// Returns an error when any underlying git command fails, when branches
    /// cannot be resolved, or when the target worktree path cannot be created.
    fn create_worktree(
        &self,
        repo_path: PathBuf,
        worktree_path: PathBuf,
        branch_name: String,
        start_ref: String,
    ) -> GitFuture<Result<(), GitError>>;

    /// Removes the existing worktree at `worktree_path`.
    ///
    /// # Errors
    /// Returns an error when the path is not a registered worktree or git
    /// cannot remove it.
    fn remove_worktree(&self, worktree_path: PathBuf) -> GitFuture<Result<(), GitError>>;

    /// Returns the staged squash-merge preview diff from `source_branch` into
    /// `target_branch` within `repo_path`.
    ///
    /// # Errors
    /// Returns an error when either branch is missing or diff generation fails.
    fn squash_merge_diff(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
    ) -> GitFuture<Result<String, GitError>>;

    /// Performs a squash merge of `source_branch` into `target_branch` inside
    /// `repo_path` using `commit_message`.
    ///
    /// # Errors
    /// Returns an error when checkout, merge, or commit operations fail.
    fn squash_merge(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
        commit_message: String,
    ) -> GitFuture<Result<SquashMergeOutcome, GitError>>;

    /// Runs `git rebase <target_branch>` in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when rebase setup fails or git reports a fatal error.
    fn rebase(&self, repo_path: PathBuf, target_branch: String) -> GitFuture<Result<(), GitError>>;

    /// Starts a rebase onto `target_branch` and reports whether it completed
    /// immediately or stopped for conflicts.
    ///
    /// # Errors
    /// Returns an error when rebase cannot be started.
    fn rebase_start(
        &self,
        repo_path: PathBuf,
        target_branch: String,
    ) -> GitFuture<Result<RebaseStepResult, GitError>>;

    /// Starts `git rebase --onto new_base old_base` in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when rebase cannot be started.
    fn rebase_onto_start(
        &self,
        repo_path: PathBuf,
        new_base: String,
        old_base: String,
    ) -> GitFuture<Result<RebaseStepResult, GitError>>;

    /// Continues an in-progress rebase in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when there is no rebase to continue or git fails.
    fn rebase_continue(&self, repo_path: PathBuf) -> GitFuture<Result<RebaseStepResult, GitError>>;

    /// Aborts an in-progress rebase in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when abort fails or no rebase state exists.
    fn abort_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>>;

    /// Returns whether rebase metadata exists in `repo_path`.
    ///
    /// # Errors
    /// Returns [`GitError::RepositoryUnavailable`] when the repository folder
    /// is missing, or another error when git state cannot be inspected.
    fn is_rebase_in_progress(&self, repo_path: PathBuf) -> GitFuture<Result<bool, GitError>>;

    /// Returns detected in-progress git operation metadata in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when git state cannot be inspected.
    fn in_progress_operation(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Option<InProgressGitOperation>, GitError>>;

    /// Returns whether unmerged index entries remain in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when index status cannot be read.
    fn has_unmerged_paths(&self, repo_path: PathBuf) -> GitFuture<Result<bool, GitError>>;

    /// Filters `paths` to files that are staged and still contain conflict
    /// markers in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when staged content cannot be inspected.
    fn list_staged_conflict_marker_files(
        &self,
        repo_path: PathBuf,
        paths: Vec<String>,
    ) -> GitFuture<Result<Vec<String>, GitError>>;

    /// Lists files currently marked conflicted in the index for `repo_path`.
    ///
    /// # Errors
    /// Returns an error when conflict state cannot be queried.
    fn list_conflicted_files(&self, repo_path: PathBuf)
    -> GitFuture<Result<Vec<String>, GitError>>;

    /// Stages and commits all changes in `repo_path` using `message`.
    ///
    /// # Errors
    /// Returns an error when staging or commit creation fails.
    fn commit_all(&self, repo_path: PathBuf, message: String) -> GitFuture<Result<(), GitError>>;

    /// Commits all changes while preserving one evolving session commit in
    /// `repo_path`.
    ///
    /// Uses `commit_message` for new or amended commit content.
    ///
    /// # Errors
    /// Returns an error when staging, amend/create, or branch inspection fails.
    fn commit_all_preserving_single_commit(
        &self,
        repo_path: PathBuf,
        base_branch: String,
        commit_message: String,
        message_strategy: SingleCommitMessageStrategy,
    ) -> GitFuture<Result<(), GitError>>;

    /// Stages all tracked and untracked changes in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when `git add` fails.
    fn stage_all(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>>;

    /// Returns the short `HEAD` hash for `repo_path`.
    ///
    /// # Errors
    /// Returns an error when `HEAD` cannot be resolved.
    fn head_short_hash(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>>;

    /// Returns the full `HEAD` hash for `repo_path`.
    ///
    /// # Errors
    /// Returns an error when `HEAD` cannot be resolved.
    fn head_hash(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>>;

    /// Returns the full commit hash for a branch, tag, or commit-ish ref.
    ///
    /// # Errors
    /// Returns an error when the reference cannot be resolved to a commit.
    fn ref_hash(
        &self,
        repo_path: PathBuf,
        reference: String,
    ) -> GitFuture<Result<String, GitError>>;

    /// Returns the full `HEAD` commit message for `repo_path`, or `None` when
    /// no commits exist.
    ///
    /// # Errors
    /// Returns an error when `HEAD` cannot be inspected.
    fn head_commit_message(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Option<String>, GitError>>;

    /// Deletes `branch_name` in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when the branch is missing, checked out, or deletion
    /// is rejected by git.
    fn delete_branch(
        &self,
        repo_path: PathBuf,
        branch_name: String,
    ) -> GitFuture<Result<(), GitError>>;

    /// Returns a patch diff from `base_branch` to current `HEAD` in
    /// `repo_path`.
    ///
    /// # Errors
    /// Returns an error when refs are invalid or diff generation fails.
    fn diff(&self, repo_path: PathBuf, base_branch: String) -> GitFuture<Result<String, GitError>>;

    /// Returns repository-relative paths changed from `base_branch` to the
    /// current worktree, including untracked files.
    ///
    /// # Errors
    /// Returns an error when refs are invalid or name-only diff generation
    /// fails.
    fn diff_changed_files(
        &self,
        repo_path: PathBuf,
        base_branch: String,
    ) -> GitFuture<Result<Vec<String>, GitError>>;

    /// Reads one repository-relative worktree file for a bounded text preview.
    ///
    /// # Errors
    /// Returns an error when the path is unsafe or the file cannot be read.
    fn read_worktree_file(
        &self,
        repo_path: PathBuf,
        relative_path: String,
    ) -> GitFuture<Result<WorktreeFileContent, GitError>>;

    /// Returns whether the worktree in `repo_path` has no local changes.
    ///
    /// # Errors
    /// Returns an error when status inspection fails.
    fn is_worktree_clean(&self, repo_path: PathBuf) -> GitFuture<Result<bool, GitError>>;

    /// Returns raw porcelain status for the worktree in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when status inspection fails.
    fn worktree_status(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>>;

    /// Returns raw porcelain status for tracked files in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when tracked-file status inspection fails.
    fn tracked_worktree_status(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>>;

    /// Returns whether `HEAD` contains commits not reachable from
    /// `base_branch`.
    ///
    /// # Errors
    /// Returns an error when commit ancestry cannot be queried.
    fn has_commits_since(
        &self,
        repo_path: PathBuf,
        base_branch: String,
    ) -> GitFuture<Result<bool, GitError>>;

    /// Performs a `pull --rebase` in `repo_path`.
    ///
    /// # Errors
    /// Returns an error when pull/rebase setup fails.
    fn pull_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<PullRebaseResult, GitError>>;

    /// Pushes the currently checked out branch for `repo_path` with
    /// `--force-with-lease` and returns the configured upstream reference
    /// after the successful push.
    ///
    /// # Errors
    /// Returns an error when remote push fails.
    fn push_current_branch(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>>;

    /// Pushes the current branch for `repo_path` to one explicit remote branch
    /// name with `--force-with-lease` and returns the configured upstream
    /// reference after the push.
    ///
    /// # Errors
    /// Returns an error when remote push fails.
    fn push_current_branch_to_remote_branch(
        &self,
        repo_path: PathBuf,
        remote_branch_name: String,
    ) -> GitFuture<Result<String, GitError>>;

    /// Pushes the current branch to one explicit remote branch while requiring
    /// that the remote branch does not exist.
    ///
    /// # Errors
    /// Returns an error when the remote branch exists or the push fails.
    fn push_current_branch_to_new_remote_branch(
        &self,
        repo_path: PathBuf,
        remote_branch_name: String,
    ) -> GitFuture<Result<String, GitError>>;

    /// Checks whether `remote_branch_name` already exists on the remote for
    /// the repository at `repo_path`.
    ///
    /// # Errors
    /// Returns an error when the remote lookup command fails.
    fn remote_branch_exists(
        &self,
        repo_path: PathBuf,
        remote_branch_name: String,
    ) -> GitFuture<Result<bool, GitError>>;

    /// Resolves the current upstream reference for `repo_path`.
    ///
    /// # Errors
    /// Returns an error when upstream tracking information is unavailable.
    fn current_upstream_reference(&self, repo_path: PathBuf)
    -> GitFuture<Result<String, GitError>>;

    /// Fetches remote refs for `repo_path`.
    ///
    /// # Errors
    /// Returns an error when fetch fails.
    fn fetch_remote(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>>;

    /// Reads ahead/behind commit counts for `repo_path`.
    ///
    /// # Errors
    /// Returns an error when upstream tracking information is unavailable.
    fn get_ahead_behind(&self, repo_path: PathBuf) -> GitFuture<Result<(u32, u32), GitError>>;

    /// Reads ahead/behind commit counts between two explicit refs.
    ///
    /// The returned tuple is `(ahead, behind)` from the perspective of
    /// `left_ref`.
    ///
    /// # Errors
    /// Returns an error when either ref cannot be resolved.
    fn get_ref_ahead_behind(
        &self,
        repo_path: PathBuf,
        left_ref: String,
        right_ref: String,
    ) -> GitFuture<Result<(u32, u32), GitError>>;

    /// Returns whether merging `source_branch` into `target_branch` would
    /// produce conflicts without changing the index or worktree.
    ///
    /// # Errors
    /// Returns an error when either ref cannot be resolved or the merge
    /// result cannot be computed.
    fn has_merge_conflicts(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
    ) -> GitFuture<Result<bool, GitError>>;

    /// Reads ahead/behind snapshots for all local branches that track an
    /// upstream.
    ///
    /// The returned map is keyed by local branch name and stores `None` when
    /// a branch has no tracked upstream or its upstream is gone.
    ///
    /// # Errors
    /// Returns an error when branch tracking information cannot be queried.
    fn branch_tracking_statuses(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<BranchTrackingMap, GitError>>;

    /// Returns commit subjects that exist in upstream but not in local
    /// `HEAD`.
    ///
    /// # Errors
    /// Returns an error when upstream tracking data or commit history cannot be
    /// read.
    fn list_upstream_commit_titles(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Vec<String>, GitError>>;

    /// Returns commit subjects that exist in local `HEAD` but not in upstream.
    ///
    /// # Errors
    /// Returns an error when upstream tracking data or commit history cannot be
    /// read.
    fn list_local_commit_titles(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Vec<String>, GitError>>;

    /// Reads the configured origin URL for `repo_path`.
    ///
    /// # Errors
    /// Returns an error when origin is missing or cannot be resolved.
    fn repo_url(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>>;

    /// Resolves the main repository root for a repository or worktree path.
    ///
    /// # Errors
    /// Returns an error when the main repository cannot be resolved.
    fn main_repo_root(&self, repo_path: PathBuf) -> GitFuture<Result<PathBuf, GitError>>;

    /// Resolves the main working checkout for a repository or worktree path.
    ///
    /// Returns `None` when the shared repository is bare, because a bare
    /// repository has no main working checkout.
    ///
    /// # Errors
    /// Returns an error when the shared repository cannot be resolved.
    fn main_checkout_working_tree(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Option<PathBuf>, GitError>>;
}

/// Production [`GitClient`] implementation backed by real git commands.
pub struct RealGitClient;

impl GitClient for RealGitClient {
    fn detect_git_info(&self, dir: PathBuf) -> GitFuture<Option<String>> {
        Box::pin(async move { detect_git_info(dir).await })
    }

    fn find_git_repo_root(&self, dir: PathBuf) -> GitFuture<Option<PathBuf>> {
        Box::pin(async move { find_git_repo_root(dir).await })
    }

    fn check_pre_commit_hook_ready(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { check_pre_commit_hook_ready(repo_path).await })
    }

    fn run_pre_commit_hook(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { run_pre_commit_hook(repo_path).await })
    }

    fn create_worktree(
        &self,
        repo_path: PathBuf,
        worktree_path: PathBuf,
        branch_name: String,
        start_ref: String,
    ) -> GitFuture<Result<(), GitError>> {
        Box::pin(
            async move { create_worktree(repo_path, worktree_path, branch_name, start_ref).await },
        )
    }

    fn remove_worktree(&self, worktree_path: PathBuf) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { remove_worktree(worktree_path).await })
    }

    fn squash_merge_diff(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
    ) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { squash_merge_diff(repo_path, source_branch, target_branch).await })
    }

    fn squash_merge(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
        commit_message: String,
    ) -> GitFuture<Result<SquashMergeOutcome, GitError>> {
        Box::pin(async move {
            squash_merge(repo_path, source_branch, target_branch, commit_message).await
        })
    }

    fn rebase(&self, repo_path: PathBuf, target_branch: String) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { rebase::rebase(repo_path, target_branch).await })
    }

    fn rebase_start(
        &self,
        repo_path: PathBuf,
        target_branch: String,
    ) -> GitFuture<Result<RebaseStepResult, GitError>> {
        Box::pin(async move { rebase_start(repo_path, target_branch).await })
    }

    fn rebase_onto_start(
        &self,
        repo_path: PathBuf,
        new_base: String,
        old_base: String,
    ) -> GitFuture<Result<RebaseStepResult, GitError>> {
        Box::pin(async move { rebase_onto_start(repo_path, new_base, old_base).await })
    }

    fn rebase_continue(&self, repo_path: PathBuf) -> GitFuture<Result<RebaseStepResult, GitError>> {
        Box::pin(async move { rebase_continue(repo_path).await })
    }

    fn abort_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { abort_rebase(repo_path).await })
    }

    fn is_rebase_in_progress(&self, repo_path: PathBuf) -> GitFuture<Result<bool, GitError>> {
        Box::pin(async move { is_rebase_in_progress(repo_path).await })
    }

    fn in_progress_operation(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Option<InProgressGitOperation>, GitError>> {
        Box::pin(async move { in_progress_operation(repo_path).await })
    }

    fn has_unmerged_paths(&self, repo_path: PathBuf) -> GitFuture<Result<bool, GitError>> {
        Box::pin(async move { has_unmerged_paths(repo_path).await })
    }

    fn list_staged_conflict_marker_files(
        &self,
        repo_path: PathBuf,
        paths: Vec<String>,
    ) -> GitFuture<Result<Vec<String>, GitError>> {
        Box::pin(async move { list_staged_conflict_marker_files(repo_path, paths).await })
    }

    fn list_conflicted_files(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Vec<String>, GitError>> {
        Box::pin(async move { list_conflicted_files(repo_path).await })
    }

    fn commit_all(&self, repo_path: PathBuf, message: String) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { commit_all(repo_path, message).await })
    }

    fn commit_all_preserving_single_commit(
        &self,
        repo_path: PathBuf,
        base_branch: String,
        commit_message: String,
        message_strategy: SingleCommitMessageStrategy,
    ) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move {
            commit_all_preserving_single_commit(
                repo_path,
                base_branch,
                commit_message,
                message_strategy,
            )
            .await
        })
    }

    fn stage_all(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { stage_all(repo_path).await })
    }

    fn head_short_hash(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { head_short_hash(repo_path).await })
    }

    fn head_hash(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { head_hash(repo_path).await })
    }

    fn ref_hash(
        &self,
        repo_path: PathBuf,
        reference: String,
    ) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { ref_hash(repo_path, reference).await })
    }

    fn head_commit_message(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Option<String>, GitError>> {
        Box::pin(async move { head_commit_message(repo_path).await })
    }

    fn delete_branch(
        &self,
        repo_path: PathBuf,
        branch_name: String,
    ) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { delete_branch(repo_path, branch_name).await })
    }

    fn diff(&self, repo_path: PathBuf, base_branch: String) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { diff(repo_path, base_branch).await })
    }

    fn diff_changed_files(
        &self,
        repo_path: PathBuf,
        base_branch: String,
    ) -> GitFuture<Result<Vec<String>, GitError>> {
        Box::pin(async move { diff_changed_files(repo_path, base_branch).await })
    }

    fn read_worktree_file(
        &self,
        repo_path: PathBuf,
        relative_path: String,
    ) -> GitFuture<Result<WorktreeFileContent, GitError>> {
        Box::pin(async move { sync::read_worktree_file(repo_path, relative_path).await })
    }

    fn is_worktree_clean(&self, repo_path: PathBuf) -> GitFuture<Result<bool, GitError>> {
        Box::pin(async move { is_worktree_clean(repo_path).await })
    }

    fn worktree_status(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { worktree_status(repo_path).await })
    }

    fn tracked_worktree_status(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { tracked_worktree_status(repo_path).await })
    }

    fn has_commits_since(
        &self,
        repo_path: PathBuf,
        base_branch: String,
    ) -> GitFuture<Result<bool, GitError>> {
        Box::pin(async move { has_commits_since(repo_path, base_branch).await })
    }

    fn pull_rebase(&self, repo_path: PathBuf) -> GitFuture<Result<PullRebaseResult, GitError>> {
        Box::pin(async move { pull_rebase(repo_path).await })
    }

    fn push_current_branch(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { push_current_branch(repo_path).await })
    }

    fn push_current_branch_to_remote_branch(
        &self,
        repo_path: PathBuf,
        remote_branch_name: String,
    ) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move {
            push_current_branch_to_remote_branch(repo_path, remote_branch_name).await
        })
    }

    fn push_current_branch_to_new_remote_branch(
        &self,
        repo_path: PathBuf,
        remote_branch_name: String,
    ) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move {
            push_current_branch_to_new_remote_branch(repo_path, remote_branch_name).await
        })
    }

    fn remote_branch_exists(
        &self,
        repo_path: PathBuf,
        remote_branch_name: String,
    ) -> GitFuture<Result<bool, GitError>> {
        Box::pin(async move { remote_branch_exists(repo_path, remote_branch_name).await })
    }

    fn current_upstream_reference(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { current_upstream_reference(repo_path).await })
    }

    fn fetch_remote(&self, repo_path: PathBuf) -> GitFuture<Result<(), GitError>> {
        Box::pin(async move { fetch_remote(repo_path).await })
    }

    fn get_ahead_behind(&self, repo_path: PathBuf) -> GitFuture<Result<(u32, u32), GitError>> {
        Box::pin(async move { get_ahead_behind(repo_path).await })
    }

    fn get_ref_ahead_behind(
        &self,
        repo_path: PathBuf,
        left_ref: String,
        right_ref: String,
    ) -> GitFuture<Result<(u32, u32), GitError>> {
        Box::pin(async move { get_ref_ahead_behind(repo_path, left_ref, right_ref).await })
    }

    fn has_merge_conflicts(
        &self,
        repo_path: PathBuf,
        source_branch: String,
        target_branch: String,
    ) -> GitFuture<Result<bool, GitError>> {
        Box::pin(async move { has_merge_conflicts(repo_path, source_branch, target_branch).await })
    }

    fn branch_tracking_statuses(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<BranchTrackingMap, GitError>> {
        Box::pin(async move { branch_tracking_statuses(repo_path).await })
    }

    fn list_upstream_commit_titles(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Vec<String>, GitError>> {
        Box::pin(async move { list_upstream_commit_titles(repo_path).await })
    }

    fn list_local_commit_titles(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Vec<String>, GitError>> {
        Box::pin(async move { list_local_commit_titles(repo_path).await })
    }

    fn repo_url(&self, repo_path: PathBuf) -> GitFuture<Result<String, GitError>> {
        Box::pin(async move { repo_url(repo_path).await })
    }

    fn main_repo_root(&self, repo_path: PathBuf) -> GitFuture<Result<PathBuf, GitError>> {
        Box::pin(async move { main_repo_root(repo_path).await })
    }

    fn main_checkout_working_tree(
        &self,
        repo_path: PathBuf,
    ) -> GitFuture<Result<Option<PathBuf>, GitError>> {
        Box::pin(async move { main_checkout_working_tree(repo_path).await })
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
