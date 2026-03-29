//! Forge review-request adapters, normalized types, and remote detection.

mod client;
mod command;
mod github;
mod model;
mod remote;

#[cfg(any(test, feature = "test-utils"))]
pub use client::MockReviewRequestClient;
pub use client::{RealReviewRequestClient, ReviewRequestClient};
pub(crate) use command::{
    ForgeCommand, ForgeCommandError, ForgeCommandOutput, ForgeCommandRunner,
    RealForgeCommandRunner, command_output_detail,
};
pub(crate) use github::GitHubReviewRequestAdapter;
pub use model::{
    CreateReviewRequestInput, ForgeFuture, ForgeKind, ForgeRemote, ReviewRequestError,
    ReviewRequestState, ReviewRequestSummary,
};
pub use remote::detect_remote;
pub(crate) use remote::{parse_remote_url, strip_port};
