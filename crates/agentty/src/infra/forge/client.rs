//! Public review-request trait boundary and production client wiring.

use std::sync::Arc;

use super::{
    CreateReviewRequestInput, ForgeCommandRunner, ForgeFuture, ForgeKind, ForgeRemote,
    GitHubReviewRequestAdapter, GitLabReviewRequestAdapter, RealForgeCommandRunner,
    ReviewRequestError, ReviewRequestSummary, detect_remote,
};

/// Async boundary used by app orchestration for forge review requests.
///
/// The app layer depends on this narrow contract so provider-specific request
/// formats remain isolated inside concrete adapters.
#[cfg_attr(test, mockall::automock)]
pub trait ReviewRequestClient: Send + Sync {
    /// Detects whether `repo_url` belongs to one supported forge.
    ///
    /// # Errors
    /// Returns [`ReviewRequestError::UnsupportedRemote`] when the remote does
    /// not map to GitHub or GitLab.
    fn detect_remote(&self, repo_url: String) -> Result<ForgeRemote, ReviewRequestError>;

    /// Finds an existing review request for `source_branch`.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when the forge lookup
    /// cannot be completed.
    fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>>;

    /// Creates a new review request from `input`.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when creation fails.
    fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Refreshes one existing review request by provider display id.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when refresh fails.
    fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Returns the browser-openable URL for one review request.
    ///
    /// # Errors
    /// Returns [`ReviewRequestError::OperationFailed`] when the summary does
    /// not carry a web URL.
    fn review_request_web_url(
        &self,
        review_request: &ReviewRequestSummary,
    ) -> Result<String, ReviewRequestError>;
}

/// Production [`ReviewRequestClient`] that routes to forge-specific adapters.
pub struct RealReviewRequestClient {
    command_runner: Arc<dyn ForgeCommandRunner>,
}

impl RealReviewRequestClient {
    /// Builds one review-request client from a forge command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self { command_runner }
    }
}

impl Default for RealReviewRequestClient {
    fn default() -> Self {
        Self::new(Arc::new(RealForgeCommandRunner))
    }
}

impl ReviewRequestClient for RealReviewRequestClient {
    fn detect_remote(&self, repo_url: String) -> Result<ForgeRemote, ReviewRequestError> {
        detect_remote(&repo_url)
    }

    fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>> {
        match remote.forge_kind {
            ForgeKind::GitHub => {
                let adapter = GitHubReviewRequestAdapter::new(Arc::clone(&self.command_runner));

                Box::pin(async move { adapter.find_by_source_branch(remote, source_branch).await })
            }
            ForgeKind::GitLab => {
                let adapter = GitLabReviewRequestAdapter::new(Arc::clone(&self.command_runner));

                Box::pin(async move { adapter.find_by_source_branch(remote, source_branch).await })
            }
        }
    }

    fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        match remote.forge_kind {
            ForgeKind::GitHub => {
                let adapter = GitHubReviewRequestAdapter::new(Arc::clone(&self.command_runner));

                Box::pin(async move { adapter.create_review_request(remote, input).await })
            }
            ForgeKind::GitLab => {
                let adapter = GitLabReviewRequestAdapter::new(Arc::clone(&self.command_runner));

                Box::pin(async move { adapter.create_review_request(remote, input).await })
            }
        }
    }

    fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        match remote.forge_kind {
            ForgeKind::GitHub => {
                let adapter = GitHubReviewRequestAdapter::new(Arc::clone(&self.command_runner));

                Box::pin(async move { adapter.refresh_review_request(remote, display_id).await })
            }
            ForgeKind::GitLab => {
                let adapter = GitLabReviewRequestAdapter::new(Arc::clone(&self.command_runner));

                Box::pin(async move { adapter.refresh_review_request(remote, display_id).await })
            }
        }
    }

    fn review_request_web_url(
        &self,
        review_request: &ReviewRequestSummary,
    ) -> Result<String, ReviewRequestError> {
        if review_request.web_url.trim().is_empty() {
            return Err(ReviewRequestError::OperationFailed {
                forge_kind: review_request.forge_kind,
                message: "review request summary is missing a web URL".to_string(),
            });
        }

        Ok(review_request.web_url.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::forge::{ForgeKind, ReviewRequestState};

    #[test]
    fn review_request_web_url_returns_error_when_summary_is_missing_url() {
        // Arrange
        let client = RealReviewRequestClient::default();
        let review_request = ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Mergeable".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge boundary".to_string(),
            web_url: String::new(),
        };

        // Act
        let error = client
            .review_request_web_url(&review_request)
            .expect_err("missing URL should be rejected");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "review request summary is missing a web URL".to_string(),
            }
        );
    }
}
