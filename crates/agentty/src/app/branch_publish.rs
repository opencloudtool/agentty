//! Branch-publish workflow helpers for session review branches.

use std::path::PathBuf;
use std::sync::Arc;

use ag_forge as forge;

use super::session::{self, Clock, unix_timestamp_from_system_time};
use crate::app::review_request;
use crate::domain::session::{PublishBranchAction, ReviewRequest, Session, Status};
use crate::infra::db;
use crate::infra::git::GitClient;
use crate::ui::state::app_mode::ConfirmationViewMode;

/// Session snapshot cloned into a branch-publish background task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchPublishTaskSession {
    /// Base branch used as the review-request target when a forge link is
    /// generated after push.
    pub(crate) base_branch: String,
    /// Session worktree used for git push and remote inspection.
    pub(crate) folder: PathBuf,
    /// Stable session identifier.
    pub(crate) id: String,
    /// Persisted linked review request, when the session already tracks one.
    pub(crate) review_request: Option<ReviewRequest>,
    /// Current session lifecycle state checked before push.
    pub(crate) status: Status,
}

impl BranchPublishTaskSession {
    /// Builds one background-task snapshot from a live session row.
    pub(crate) fn from_session(session: &Session) -> Self {
        Self {
            base_branch: session.base_branch.clone(),
            folder: session.folder.clone(),
            id: session.id.clone(),
            review_request: session.review_request.clone(),
            status: session.status,
        }
    }
}

/// Final reducer payload for a completed branch-publish background action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchPublishActionUpdate {
    /// Restore-view payload used to rebuild the previous session UI.
    pub(crate) restore_view: ConfirmationViewMode,
    /// Branch-publish task result routed through the reducer.
    pub(crate) result: BranchPublishTaskResult,
    /// Session id targeted by the completed action.
    pub(crate) session_id: String,
}

/// Error payload shown inside the session-view info popup for branch-publish
/// failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchPublishTaskFailure {
    /// Popup body text describing the failure.
    pub(crate) message: String,
    /// Popup title shown for the failure.
    pub(crate) title: String,
}

impl BranchPublishTaskFailure {
    /// Builds one blocked-state popup payload from an actionable message.
    pub(crate) fn blocked(publish_branch_action: PublishBranchAction, message: String) -> Self {
        Self {
            message,
            title: match publish_branch_action {
                PublishBranchAction::Push => "Branch push blocked".to_string(),
                PublishBranchAction::PublishPullRequest => {
                    "GitHub pull request publish blocked".to_string()
                }
            },
        }
    }

    /// Builds one failure-state popup payload from an execution error.
    pub(crate) fn failed(publish_branch_action: PublishBranchAction, message: String) -> Self {
        Self {
            message,
            title: match publish_branch_action {
                PublishBranchAction::Push => "Branch push failed".to_string(),
                PublishBranchAction::PublishPullRequest => {
                    "GitHub pull request publish failed".to_string()
                }
            },
        }
    }
}

/// Successful outcome returned by a branch-publish background action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BranchPublishTaskSuccess {
    /// Carries the pushed branch name and persisted upstream reference.
    Pushed {
        /// Remote branch name that was pushed successfully.
        branch_name: String,
        /// Optional forge-native URL that opens a new review-request flow.
        review_request_creation_url: Option<String>,
        /// Persisted upstream ref recorded after the successful push.
        upstream_reference: String,
    },
    /// Carries the pushed branch name, linked pull request, and upstream ref.
    PullRequestPublished {
        /// Remote branch name that was pushed successfully.
        branch_name: String,
        /// Persisted review-request summary refreshed or created by the action.
        review_request: ReviewRequest,
        /// Persisted upstream ref recorded after the successful push.
        upstream_reference: String,
    },
}

/// Reducer-friendly result for a completed branch-publish background action.
pub(crate) type BranchPublishTaskResult =
    Result<BranchPublishTaskSuccess, BranchPublishTaskFailure>;

/// Returns the loading popup title for one branch-publish action.
pub(crate) fn branch_publish_loading_title(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Pushing branch".to_string(),
        PublishBranchAction::PublishPullRequest => "Publishing GitHub pull request".to_string(),
    }
}

/// Returns the loading popup body for one branch-publish action.
pub(crate) fn branch_publish_loading_message(
    publish_branch_action: PublishBranchAction,
    remote_branch_name: Option<&str>,
) -> String {
    match (publish_branch_action, remote_branch_name) {
        (PublishBranchAction::Push, Some(remote_branch_name)) => format!(
            "Publishing the session branch to `{remote_branch_name}` on the configured Git remote."
        ),
        (PublishBranchAction::Push, None) => {
            "Publishing the session branch to the configured Git remote.".to_string()
        }
        (PublishBranchAction::PublishPullRequest, Some(remote_branch_name)) => format!(
            "Pushing the session branch to `{remote_branch_name}` and creating or refreshing the \
             GitHub pull request."
        ),
        (PublishBranchAction::PublishPullRequest, None) => {
            "Pushing the session branch and creating or refreshing the GitHub pull request."
                .to_string()
        }
    }
}

/// Returns the loading spinner label for one branch-publish action.
pub(crate) fn branch_publish_loading_label(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Pushing branch...".to_string(),
        PublishBranchAction::PublishPullRequest => "Publishing pull request...".to_string(),
    }
}

/// Returns the success popup title for a completed branch-publish action.
pub(crate) fn branch_publish_success_title(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Branch pushed".to_string(),
        PublishBranchAction::PublishPullRequest => "GitHub pull request published".to_string(),
    }
}

/// Returns the success popup body for one completed branch push.
pub(crate) fn branch_push_success_message(
    branch_name: &str,
    review_request_creation_url: Option<&str>,
) -> String {
    match review_request_creation_url {
        Some(review_request_creation_url) => format!(
            "Pushed session branch `{branch_name}`.\n\nOpen this link to create the pull \
             request:\n{review_request_creation_url}"
        ),
        None => format!(
            "Pushed session branch `{branch_name}`.\n\nCreate the pull request manually from your \
             forge UI."
        ),
    }
}

/// Returns the success popup body for one completed pull-request publish.
pub(crate) fn pull_request_publish_success_message(
    branch_name: &str,
    review_request: &ReviewRequest,
) -> String {
    format!(
        "Published session branch `{branch_name}`.\n\nGitHub pull request {} is ready:\n{}",
        review_request.summary.display_id, review_request.summary.web_url
    )
}

/// Executes one background branch-publish action for a session snapshot.
pub(crate) async fn run_branch_publish_action(
    publish_branch_action: PublishBranchAction,
    branch_publish_session: BranchPublishTaskSession,
    db: db::Database,
    clock: Arc<dyn Clock>,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    remote_branch_name: Option<String>,
) -> BranchPublishTaskResult {
    match publish_branch_action {
        PublishBranchAction::Push => {
            push_session_branch(
                publish_branch_action,
                &branch_publish_session,
                db,
                git_client,
                remote_branch_name.as_deref(),
            )
            .await
        }
        PublishBranchAction::PublishPullRequest => {
            publish_pull_request(
                &branch_publish_session,
                db,
                clock,
                git_client,
                review_request_client,
                remote_branch_name.as_deref(),
            )
            .await
        }
    }
}

/// Returns whether error output looks like a git push authentication failure.
pub(crate) fn is_git_push_authentication_error(detail_message: &str) -> bool {
    let normalized_detail = detail_message.to_ascii_lowercase();

    let is_push_context = normalized_detail.contains("git push failed")
        || (normalized_detail.contains("push")
            && (normalized_detail.contains("remote") || normalized_detail.contains("origin")));
    if !is_push_context {
        return false;
    }

    normalized_detail.contains("authentication failed")
        || normalized_detail.contains("terminal prompts disabled")
        || normalized_detail.contains("could not read username")
        || normalized_detail.contains("could not read password")
        || normalized_detail.contains("permission denied")
        || normalized_detail.contains("access denied")
        || normalized_detail.contains("not authorized")
        || normalized_detail.contains("support for password authentication was removed")
        || normalized_detail.contains("the requested url returned error: 403")
        || normalized_detail.contains("repository not found")
}

/// Attempts to infer one forge kind from a git push authentication failure.
pub(crate) fn detected_forge_kind_from_git_push_error(
    detail_message: &str,
) -> Option<forge::ForgeKind> {
    let normalized_detail = detail_message.to_ascii_lowercase();

    if let Some(forge_kind) = detected_forge_kind_from_push_auth_url(&normalized_detail) {
        return Some(forge_kind);
    }

    if normalized_detail.contains("github.com") || normalized_detail.contains(" gh ") {
        return Some(forge::ForgeKind::GitHub);
    }

    None
}

/// Returns actionable copy for one git push authentication failure.
pub(crate) fn git_push_authentication_message(
    forge_kind: Option<forge::ForgeKind>,
    retry_action: &str,
) -> String {
    match forge_kind {
        Some(forge::ForgeKind::GitHub) => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nRun `gh auth login`, or configure credentials with a PAT/SSH key."
        ),
        None => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nConfigure Git credentials with a PAT/SSH key or credential helper."
        ),
    }
}

/// Pushes one session branch to the configured Git remote.
pub(crate) async fn push_session_branch(
    publish_branch_action: PublishBranchAction,
    branch_publish_session: &BranchPublishTaskSession,
    db: db::Database,
    git_client: Arc<dyn GitClient>,
    remote_branch_name: Option<&str>,
) -> BranchPublishTaskResult {
    if !branch_publish_session.status.allows_review_actions() {
        return Err(BranchPublishTaskFailure::failed(
            publish_branch_action,
            "Session must be in review to push the branch.".to_string(),
        ));
    }

    let branch_name = remote_branch_name.map_or_else(
        || session::session_branch(&branch_publish_session.id),
        str::to_string,
    );
    let upstream_reference = push_session_branch_to_remote(
        branch_publish_session,
        publish_branch_action,
        &db,
        git_client.clone(),
        remote_branch_name,
    )
    .await?;
    let review_request_creation_url =
        branch_review_request_creation_url(branch_publish_session, git_client, &branch_name).await;

    Ok(BranchPublishTaskSuccess::Pushed {
        branch_name,
        review_request_creation_url,
        upstream_reference,
    })
}

/// Pushes one session branch, then creates or refreshes its GitHub pull
/// request.
async fn publish_pull_request(
    branch_publish_session: &BranchPublishTaskSession,
    db: db::Database,
    clock: Arc<dyn Clock>,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    remote_branch_name: Option<&str>,
) -> BranchPublishTaskResult {
    if !branch_publish_session.status.allows_review_actions() {
        return Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::PublishPullRequest,
            "Session must be in review to publish the pull request.".to_string(),
        ));
    }

    let branch_name = remote_branch_name.map_or_else(
        || session::session_branch(&branch_publish_session.id),
        str::to_string,
    );
    let upstream_reference = push_session_branch_to_remote(
        branch_publish_session,
        PublishBranchAction::PublishPullRequest,
        &db,
        git_client.clone(),
        remote_branch_name,
    )
    .await?;
    let remote = review_request_remote(
        branch_publish_session,
        git_client.clone(),
        review_request_client.as_ref(),
    )
    .await?;
    let review_request = create_or_refresh_review_request(
        branch_publish_session,
        &clock,
        &db,
        git_client.clone(),
        review_request_client,
        remote,
        branch_name.clone(),
    )
    .await?;

    Ok(BranchPublishTaskSuccess::PullRequestPublished {
        branch_name,
        review_request,
        upstream_reference,
    })
}

/// Pushes the session branch to the configured remote and persists the
/// resulting upstream reference.
async fn push_session_branch_to_remote(
    branch_publish_session: &BranchPublishTaskSession,
    publish_branch_action: PublishBranchAction,
    db: &db::Database,
    git_client: Arc<dyn GitClient>,
    remote_branch_name: Option<&str>,
) -> Result<String, BranchPublishTaskFailure> {
    let folder = branch_publish_session.folder.clone();
    let upstream_reference = match remote_branch_name {
        Some(remote_branch_name) => {
            git_client
                .push_current_branch_to_remote_branch(folder, remote_branch_name.to_string())
                .await
        }
        None => git_client.push_current_branch(folder).await,
    }
    .map_err(|error| branch_push_failure(publish_branch_action, &error.to_string()))?;

    db.update_session_published_upstream_ref(&branch_publish_session.id, Some(&upstream_reference))
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                publish_branch_action,
                format!(
                    "Branch push succeeded, but Agentty could not persist the upstream reference: \
                     {error}"
                ),
            )
        })?;

    Ok(upstream_reference)
}

/// Resolves one forge remote for pull-request publishing.
async fn review_request_remote(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    review_request_client: &dyn forge::ReviewRequestClient,
) -> Result<forge::ForgeRemote, BranchPublishTaskFailure> {
    let repo_url = git_client
        .repo_url(branch_publish_session.folder.clone())
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to resolve repository remote for pull request: {error}"),
            )
        })?;

    review_request_client
        .detect_remote(repo_url)
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                error.detail_message(),
            )
        })
}

/// Creates or refreshes one pull request for the published session branch and
/// persists the normalized summary.
async fn create_or_refresh_review_request(
    branch_publish_session: &BranchPublishTaskSession,
    clock: &Arc<dyn Clock>,
    db: &db::Database,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    remote: forge::ForgeRemote,
    source_branch: String,
) -> Result<ReviewRequest, BranchPublishTaskFailure> {
    let review_request_summary =
        if let Some(review_request) = &branch_publish_session.review_request {
            review_request_client
                .refresh_review_request(remote, review_request.summary.display_id.clone())
                .await
                .map_err(|error| {
                    BranchPublishTaskFailure::failed(
                        PublishBranchAction::PublishPullRequest,
                        error.detail_message(),
                    )
                })?
        } else if let Some(existing_review_request) = review_request_client
            .find_by_source_branch(remote.clone(), source_branch.clone())
            .await
            .map_err(|error| {
                BranchPublishTaskFailure::failed(
                    PublishBranchAction::PublishPullRequest,
                    error.detail_message(),
                )
            })?
        {
            review_request_client
                .refresh_review_request(remote, existing_review_request.display_id)
                .await
                .map_err(|error| {
                    BranchPublishTaskFailure::failed(
                        PublishBranchAction::PublishPullRequest,
                        error.detail_message(),
                    )
                })?
        } else {
            let create_input =
                load_review_request_create_input(branch_publish_session, git_client, source_branch)
                    .await?;

            review_request_client
                .create_review_request(remote, create_input)
                .await
                .map_err(|error| {
                    BranchPublishTaskFailure::failed(
                        PublishBranchAction::PublishPullRequest,
                        error.detail_message(),
                    )
                })?
        };
    let review_request = ReviewRequest {
        last_refreshed_at: unix_timestamp_from_system_time(clock.now_system_time()),
        summary: review_request_summary,
    };

    db.update_session_review_request(&branch_publish_session.id, Some(&review_request))
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!(
                    "Pull request publish succeeded, but Agentty could not persist the linked \
                     pull request: {error}"
                ),
            )
        })?;

    Ok(review_request)
}

/// Builds one normalized create-request payload from branch-publish session
/// commit message.
async fn load_review_request_create_input(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    source_branch: String,
) -> Result<forge::CreateReviewRequestInput, BranchPublishTaskFailure> {
    let commit_message = git_client
        .head_commit_message(branch_publish_session.folder.clone())
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to load session branch commit message: {error}"),
            )
        })?
        .ok_or_else(|| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                "Session branch has no commit message for pull request publishing.".to_string(),
            )
        })?;
    let review_request_commit_message =
        review_request::parse_review_request_commit_message(&commit_message).ok_or_else(|| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                "Session branch commit message must have a non-empty title for pull request \
                 publishing."
                    .to_string(),
            )
        })?;

    Ok(forge::CreateReviewRequestInput {
        body: review_request_commit_message.body,
        source_branch,
        target_branch: branch_publish_session.base_branch.clone(),
        title: review_request_commit_message.title,
    })
}

/// Returns one forge-native review-request creation URL for a pushed session.
async fn branch_review_request_creation_url(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    branch_name: &str,
) -> Option<String> {
    let repo_url = git_client
        .repo_url(branch_publish_session.folder.clone())
        .await
        .ok()?;
    let remote = forge::detect_remote(&repo_url).ok()?;

    remote
        .review_request_creation_url(branch_name, &branch_publish_session.base_branch)
        .ok()
}

/// Maps one branch-publish failure into blocked or failed popup copy.
pub(crate) fn branch_push_failure(
    publish_branch_action: PublishBranchAction,
    error: &str,
) -> BranchPublishTaskFailure {
    if !is_git_push_authentication_error(error) {
        return BranchPublishTaskFailure::failed(
            publish_branch_action,
            format!("Failed to publish session branch: {error}"),
        );
    }

    BranchPublishTaskFailure::blocked(
        publish_branch_action,
        git_push_authentication_message(
            detected_forge_kind_from_git_push_error(error),
            match publish_branch_action {
                PublishBranchAction::Push => "push the branch again",
                PublishBranchAction::PublishPullRequest => "publish the pull request again",
            },
        ),
    )
}

/// Returns one forge family from the remote host shown in a credential error.
fn detected_forge_kind_from_push_auth_url(detail_message: &str) -> Option<forge::ForgeKind> {
    let host = extract_push_auth_prompt_host(detail_message)?;
    if host.is_empty() {
        return None;
    }

    let host = strip_port(host);
    if is_github_host(host) {
        return Some(forge::ForgeKind::GitHub);
    }

    None
}

/// Returns whether `host` is a GitHub-style forge host.
fn is_github_host(host: &str) -> bool {
    host == "github.com" || host.ends_with(".github.com")
}

/// Extracts one remote host from one `git push` authentication prompt.
fn extract_push_auth_prompt_host(detail_message: &str) -> Option<&str> {
    let username_marker = "could not read username for '";
    let password_marker = "could not read password for '";

    if let Some(host) = extract_host_from_prompt(detail_message, username_marker) {
        return Some(host);
    }

    extract_host_from_prompt(detail_message, password_marker)
}

/// Extracts the host payload from one quoted credential-prompt URL.
fn extract_host_from_prompt<'detail>(
    detail_message: &'detail str,
    marker: &str,
) -> Option<&'detail str> {
    let marker_start = detail_message.find(marker)?;
    let quoted_host = &detail_message[marker_start + marker.len()..];
    let host = quoted_host.split('\'').next()?;
    let host = host.trim().trim_end_matches('/');
    let host = host
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = host.split('/').next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);

    Some(host)
}

/// Removes one explicit host port, if present.
fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Describes one auth-guidance parsing scenario for `branch_push_failure`.
    struct AuthGuidanceCase {
        error: &'static str,
        expect_github_guidance: bool,
        name: &'static str,
    }

    /// Verifies branch-push auth guidance uses detected forge hints when the
    /// error text includes a recognizable host.
    #[test]
    fn branch_push_failure_uses_detected_forge_guidance() {
        // Arrange
        let error =
            "git push failed: could not read username for 'https://github.com/openai/agentty': \
             terminal prompts disabled";

        // Act
        let failure = branch_push_failure(PublishBranchAction::Push, error);

        // Assert
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("gh auth login"));
        assert!(failure.message.contains("push the branch again"));
    }

    /// Verifies auth guidance handles additional git push error formats
    /// without regressing forge detection or fallback messaging.
    #[test]
    fn branch_push_failure_handles_multiple_auth_error_formats() {
        // Arrange
        let cases = vec![
            AuthGuidanceCase {
                name: "mixed-case https url",
                error: "Git push failed: fatal: could not read Username for 'HTTPS://GitHub.com/OpenAI/agentty': terminal prompts disabled",
                expect_github_guidance: true,
            },
            AuthGuidanceCase {
                name: "password prompt without scheme",
                error: "Git push failed: fatal: could not read Password for 'github.com/OpenAI/agentty': terminal prompts disabled",
                expect_github_guidance: true,
            },
            AuthGuidanceCase {
                name: "github url with port and subpath",
                error: "Git push failed: fatal: could not read Username for 'https://user@github.com:443/openai/agentty/path': terminal prompts disabled",
                expect_github_guidance: true,
            },
            AuthGuidanceCase {
                name: "non-github host falls back to generic guidance",
                error: "Git push failed: fatal: could not read Username for 'https://gitlab.com/openai/agentty': terminal prompts disabled",
                expect_github_guidance: false,
            },
        ];

        // Act
        for case in cases {
            let failure = branch_push_failure(PublishBranchAction::Push, case.error);

            // Assert
            assert_eq!(failure.title, "Branch push blocked", "case: {}", case.name);
            assert!(
                failure.message.contains("push the branch again"),
                "case: {}",
                case.name
            );
            if case.expect_github_guidance {
                assert!(
                    failure.message.contains("gh auth login"),
                    "case: {}",
                    case.name
                );
            } else {
                assert!(
                    !failure.message.contains("gh auth login"),
                    "case: {}",
                    case.name
                );
                assert!(
                    failure.message.contains("PAT/SSH key or credential helper"),
                    "case: {}",
                    case.name
                );
            }
        }
    }
}
