//! Forge review-request module router.

mod client;
mod command;
mod github;
mod gitlab;
mod model;
mod remote;

pub use client::{RealReviewRequestClient, ReviewRequestClient};
#[cfg(test)]
pub(crate) use command::MockForgeCommandRunner;
pub(crate) use command::{
    ForgeCommand, ForgeCommandError, ForgeCommandOutput, ForgeCommandRunner,
    RealForgeCommandRunner, command_output_detail,
};
pub(crate) use github::GitHubReviewRequestAdapter;
pub(crate) use gitlab::GitLabReviewRequestAdapter;
pub use model::{
    CreateReviewRequestInput, ForgeFuture, ForgeKind, ForgeRemote, ReviewRequestError,
    ReviewRequestState, ReviewRequestSummary,
};
pub(crate) use remote::{detect_remote, parse_remote_url, strip_port};
