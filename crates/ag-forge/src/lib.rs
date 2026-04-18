//! Forge review-request adapters, normalized types, and remote detection.

mod adapter_common;
mod client;
mod command;
mod github;
mod gitlab;
mod model;
mod remote;

pub(crate) use adapter_common::{
    looks_like_authentication_failure, looks_like_host_resolution_failure, map_spawn_error,
    normalize_provider_label, status_summary_parts,
};
#[cfg(any(test, feature = "test-utils"))]
pub use client::MockReviewRequestClient;
pub use client::{RealReviewRequestClient, ReviewRequestClient};
pub(crate) use command::{
    ForgeCommand, ForgeCommandError, ForgeCommandOutput, ForgeCommandRunner,
    RealForgeCommandRunner, command_output_detail,
};
pub(crate) use github::GitHubReviewRequestAdapter;
pub(crate) use gitlab::GitLabReviewRequestAdapter;
pub use model::{
    CreateReviewRequestInput, ForgeFuture, ForgeKind, ForgeRemote, ReviewRequestError,
    ReviewRequestState, ReviewRequestSummary, is_gitlab_host,
};
pub use remote::detect_remote;
pub(crate) use remote::{parse_remote_url, strip_port};
