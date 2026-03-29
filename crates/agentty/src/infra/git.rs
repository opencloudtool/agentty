//! Git infrastructure module router.
//!
//! This parent module intentionally exposes child modules and re-exports the
//! public git API surface.

/// Client boundary and production adapter implementations.
mod client;
/// Typed error types for git infrastructure operations.
mod error;
/// Squash-merge workflows.
mod merge;
/// Rebase and conflict workflows.
mod rebase;
/// Repository-level helpers and metadata operations.
mod repo;
/// Commit, diff, and remote synchronization workflows.
mod sync;
/// Worktree and branch-detection workflows.
mod worktree;

#[cfg(test)]
pub(crate) use client::MockGitClient;
pub use client::{GitClient, GitFuture, RealGitClient};
/// Re-exported typed error for git infrastructure operations.
pub use error::GitError;
/// Re-exported squash-merge APIs.
pub use merge::{SquashMergeOutcome, squash_merge, squash_merge_diff};
/// Re-exported rebase/conflict APIs.
pub use rebase::{
    RebaseStepResult, abort_rebase, has_unmerged_paths, is_rebase_in_progress,
    list_conflicted_files, list_staged_conflict_marker_files, rebase, rebase_continue,
    rebase_start,
};
/// Re-exported repository metadata APIs.
pub use repo::{main_repo_root, repo_url};
/// Re-exported commit/sync/diff APIs.
pub use sync::{
    BranchTrackingMap, PullRebaseResult, SingleCommitMessageStrategy, branch_tracking_statuses,
    commit_all, commit_all_preserving_single_commit, current_upstream_reference, delete_branch,
    diff, fetch_remote, get_ahead_behind, get_ref_ahead_behind, has_commits_since,
    head_commit_message, head_short_hash, is_worktree_clean, list_local_commit_titles,
    list_upstream_commit_titles, pull_rebase, push_current_branch,
    push_current_branch_to_remote_branch, stage_all,
};
/// Re-exported worktree and branch-detection APIs.
pub use worktree::{create_worktree, detect_git_info, find_git_repo_root, remove_worktree};
