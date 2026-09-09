//! Reusable git, worktree, sync, rebase, and squash-merge orchestration.

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
/// Sleep boundary for retry behavior.
mod sleeper;
/// Commit, diff, and remote synchronization workflows.
mod sync;
/// Worktree and branch-detection workflows.
mod worktree;

#[cfg(any(test, feature = "test-utils"))]
pub use client::MockGitClient;
pub use client::{GitClient, GitFuture, RealGitClient};
/// Re-exported typed error for git infrastructure operations.
pub use error::GitError;
pub use merge::SquashMergeOutcome;
pub(crate) use merge::{has_merge_conflicts, squash_merge, squash_merge_diff};
pub use rebase::{InProgressGitOperation, RebaseStepResult};
pub(crate) use rebase::{
    abort_rebase, has_unmerged_paths, in_progress_operation, is_rebase_in_progress,
    list_conflicted_files, list_staged_conflict_marker_files, rebase, rebase_continue,
    rebase_onto_start, rebase_start,
};
pub(crate) use repo::{main_checkout_working_tree, main_repo_root, repo_url};
pub(crate) use sleeper::{Sleeper, ThreadSleeper};
pub use sync::{
    BranchTrackingMap, PullRebaseResult, SingleCommitMessageStrategy, WorktreeFileContent,
};
pub(crate) use sync::{
    branch_tracking_statuses, check_pre_commit_hook_ready, commit_all,
    commit_all_preserving_single_commit, current_upstream_reference, delete_branch, diff,
    diff_changed_files, fetch_remote, get_ahead_behind, get_ref_ahead_behind, has_commits_since,
    head_commit_message, head_hash, head_short_hash, is_worktree_clean, list_local_commit_titles,
    list_upstream_commit_titles, pull_rebase, push_current_branch,
    push_current_branch_to_new_remote_branch, push_current_branch_to_remote_branch, ref_hash,
    remote_branch_exists, run_pre_commit_hook, stage_all, tracked_worktree_status, worktree_status,
};
pub(crate) use worktree::{create_worktree, detect_git_info, find_git_repo_root, remove_worktree};
