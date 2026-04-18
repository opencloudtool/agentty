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
    /// Persisted upstream reference from a previous push, when the session
    /// already tracks one.
    pub(crate) published_upstream_ref: Option<String>,
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
            published_upstream_ref: session.published_upstream_ref.clone(),
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
    /// Whether the failure represents a blocked state (e.g. auth required)
    /// rather than an execution error.
    pub(crate) is_blocked: bool,
    /// Popup body text describing the failure.
    pub(crate) message: String,
    /// Popup title shown for the failure.
    pub(crate) title: String,
}

impl BranchPublishTaskFailure {
    /// Builds one blocked-state popup payload from an actionable message.
    pub(crate) fn blocked(publish_branch_action: PublishBranchAction, message: String) -> Self {
        Self {
            is_blocked: true,
            message,
            title: match publish_branch_action {
                PublishBranchAction::Push => "Branch push blocked".to_string(),
                PublishBranchAction::PublishPullRequest => {
                    "Review request publish blocked".to_string()
                }
            },
        }
    }

    /// Builds one failure-state popup payload from an execution error.
    pub(crate) fn failed(publish_branch_action: PublishBranchAction, message: String) -> Self {
        Self {
            is_blocked: false,
            message,
            title: match publish_branch_action {
                PublishBranchAction::Push => "Branch push failed".to_string(),
                PublishBranchAction::PublishPullRequest => {
                    "Review request publish failed".to_string()
                }
            },
        }
    }

    /// Rebuilds the popup title for a different publish action while
    /// preserving the blocked/failed distinction and original message.
    #[cfg(test)]
    pub(crate) fn with_action(self, publish_branch_action: PublishBranchAction) -> Self {
        if self.is_blocked {
            Self::blocked(publish_branch_action, self.message)
        } else {
            Self::failed(publish_branch_action, self.message)
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
        /// Optional forge-native metadata that can open or describe the new
        /// review-request flow.
        review_request_creation: Option<ReviewRequestCreationInfo>,
        /// Persisted upstream ref recorded after the successful push.
        upstream_reference: String,
    },
    /// Carries the pushed branch name, linked review request, and upstream
    /// ref.
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

/// Forge-specific metadata used to describe one review-request creation path
/// after a branch push.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestCreationInfo {
    /// Forge family that can open or create the follow-up review request.
    pub(crate) forge_kind: forge::ForgeKind,
    /// Optional forge-native URL for starting the review-request flow.
    pub(crate) web_url: Option<String>,
}

/// Returns the loading popup title for one branch-publish action.
pub(crate) fn branch_publish_loading_title(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Pushing branch".to_string(),
        PublishBranchAction::PublishPullRequest => "Publishing review request".to_string(),
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
             active forge review request."
        ),
        (PublishBranchAction::PublishPullRequest, None) => {
            "Pushing the session branch and creating or refreshing the active forge review request."
                .to_string()
        }
    }
}

/// Returns the loading spinner label for one branch-publish action.
pub(crate) fn branch_publish_loading_label(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Pushing branch...".to_string(),
        PublishBranchAction::PublishPullRequest => "Publishing review request...".to_string(),
    }
}

/// Returns the success popup title for a completed branch-publish action.
pub(crate) fn branch_publish_success_title(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Branch pushed".to_string(),
        PublishBranchAction::PublishPullRequest => "Review request published".to_string(),
    }
}

/// Returns the success popup body for one completed branch push.
pub(crate) fn branch_push_success_message(
    branch_name: &str,
    review_request_creation: Option<&ReviewRequestCreationInfo>,
) -> String {
    match review_request_creation {
        Some(ReviewRequestCreationInfo {
            forge_kind,
            web_url: Some(review_request_creation_url),
        }) => format!(
            "Pushed session branch `{branch_name}`.\n\nOpen this link to create the {}:\n{}",
            forge_kind.review_request_name(),
            review_request_creation_url
        ),
        Some(ReviewRequestCreationInfo {
            forge_kind,
            web_url: None,
        }) => format!(
            "Pushed session branch `{branch_name}`.\n\nCreate the {} manually from your forge UI.",
            forge_kind.review_request_name()
        ),
        None => format!(
            "Pushed session branch `{branch_name}`.\n\nCreate the review request manually from \
             your forge UI."
        ),
    }
}

/// Returns the success popup title for one completed review-request publish.
pub(crate) fn review_request_publish_success_title(review_request: &ReviewRequest) -> String {
    format!(
        "{} published",
        review_request
            .summary
            .forge_kind
            .review_request_display_name()
    )
}

/// Returns the success popup body for one completed review-request publish.
pub(crate) fn pull_request_publish_success_message(
    branch_name: &str,
    review_request: &ReviewRequest,
) -> String {
    format!(
        "Published session branch `{branch_name}`.\n\n{} {} is ready:\n{}",
        review_request
            .summary
            .forge_kind
            .review_request_display_name(),
        review_request.summary.display_id,
        review_request.summary.web_url
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
/// Returns whether `normalized_detail` (already lower-cased) contains any
/// credential- or authentication-related keywords produced by git remote
/// operations.
fn has_authentication_error_keywords(normalized_detail: &str) -> bool {
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

pub(crate) fn is_git_push_authentication_error(detail_message: &str) -> bool {
    let normalized_detail = detail_message.to_ascii_lowercase();

    let is_push_context = normalized_detail.contains("git push failed")
        || (normalized_detail.contains("push")
            && (normalized_detail.contains("remote") || normalized_detail.contains("origin")));
    if !is_push_context {
        return false;
    }

    has_authentication_error_keywords(&normalized_detail)
}

/// Attempts to infer one forge kind from a git push authentication failure.
pub(crate) fn detected_forge_kind_from_git_push_error(
    detail_message: &str,
) -> Option<forge::ForgeKind> {
    let normalized_detail = detail_message.to_ascii_lowercase();

    if let Some(forge_kind) = detected_forge_kind_from_push_auth_url(&normalized_detail) {
        return Some(forge_kind);
    }

    if let Some(forge_kind) = detected_forge_kind_from_text(detail_message) {
        return Some(forge_kind);
    }

    if normalized_detail.contains(" gh ") {
        return Some(forge::ForgeKind::GitHub);
    }

    if normalized_detail.contains(" glab ") {
        return Some(forge::ForgeKind::GitLab);
    }

    None
}

/// Returns the user-facing retry guidance phrase for one publish action.
fn retry_action_text(publish_branch_action: PublishBranchAction) -> &'static str {
    match publish_branch_action {
        PublishBranchAction::Push => "push the branch again",
        PublishBranchAction::PublishPullRequest => "publish the review request again",
    }
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
        Some(forge::ForgeKind::GitLab) => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nRun `glab auth login`, or configure credentials with a PAT/SSH key."
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
        &db,
        branch_publish_session.folder.clone(),
        git_client.clone(),
        publish_branch_action,
        &branch_publish_session.id,
        remote_branch_name,
        branch_publish_session.published_upstream_ref.as_deref(),
    )
    .await?;
    let review_request_creation =
        branch_review_request_creation_info(branch_publish_session, git_client, &branch_name).await;

    Ok(BranchPublishTaskSuccess::Pushed {
        branch_name,
        review_request_creation,
        upstream_reference,
    })
}

/// Pushes one session branch, then creates or refreshes its forge review
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
            "Session must be in review to publish the review request.".to_string(),
        ));
    }

    let branch_name = remote_branch_name.map_or_else(
        || session::session_branch(&branch_publish_session.id),
        str::to_string,
    );
    let upstream_reference = push_session_branch_to_remote(
        &db,
        branch_publish_session.folder.clone(),
        git_client.clone(),
        PublishBranchAction::PublishPullRequest,
        &branch_publish_session.id,
        remote_branch_name,
        branch_publish_session.published_upstream_ref.as_deref(),
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
///
/// When `remote_branch_name` is supplied and the session has no prior
/// `published_upstream_ref`, a pre-flight `git ls-remote` check blocks
/// the push if the remote branch already exists.
pub(crate) async fn push_session_branch_to_remote(
    db: &db::Database,
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    publish_branch_action: PublishBranchAction,
    session_id: &str,
    remote_branch_name: Option<&str>,
    published_upstream_ref: Option<&str>,
) -> Result<String, BranchPublishTaskFailure> {
    let retry_text = retry_action_text(publish_branch_action);

    if let Some(target_branch) = remote_branch_name
        && published_upstream_ref.is_none()
    {
        let already_exists = git_client
            .remote_branch_exists(folder.clone(), target_branch.to_string())
            .await
            .map_err(|error| {
                let detail = error.to_string();
                let normalized = detail.to_ascii_lowercase();

                if has_authentication_error_keywords(&normalized) {
                    BranchPublishTaskFailure::blocked(
                        publish_branch_action,
                        git_push_authentication_message(
                            detected_forge_kind_from_git_push_error(&detail),
                            retry_text,
                        ),
                    )
                } else {
                    BranchPublishTaskFailure::failed(
                        publish_branch_action,
                        format!("Failed to check remote branch existence: {error}"),
                    )
                }
            })?;

        if already_exists {
            return Err(BranchPublishTaskFailure::blocked(
                publish_branch_action,
                format!(
                    "Remote branch `{target_branch}` already exists. Choose a different name or \
                     use the default session branch."
                ),
            ));
        }
    }

    let upstream_reference = match remote_branch_name {
        Some(remote_branch_name) => {
            git_client
                .push_current_branch_to_remote_branch(folder, remote_branch_name.to_string())
                .await
        }
        None => git_client.push_current_branch(folder).await,
    }
    .map_err(|error| {
        let detail = error.to_string();
        let normalized = detail.to_ascii_lowercase();

        if has_authentication_error_keywords(&normalized) {
            BranchPublishTaskFailure::blocked(
                publish_branch_action,
                git_push_authentication_message(
                    detected_forge_kind_from_git_push_error(&detail),
                    retry_text,
                ),
            )
        } else {
            BranchPublishTaskFailure::failed(
                publish_branch_action,
                format!("Failed to publish session branch: {error}"),
            )
        }
    })?;

    db.update_session_published_upstream_ref(session_id, Some(&upstream_reference))
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

/// Resolves one forge remote for review-request publishing.
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
                format!("Failed to resolve repository remote for review request: {error}"),
            )
        })?;

    review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(branch_publish_session.folder.clone()))
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                error.detail_message(),
            )
        })
}

/// Creates or refreshes one review request for the published session branch and
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
                    "Review-request publish succeeded, but Agentty could not persist the linked \
                     review request: {error}"
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
                "Session branch has no commit message for review-request publishing.".to_string(),
            )
        })?;
    let review_request_commit_message =
        review_request::parse_review_request_commit_message(&commit_message).ok_or_else(|| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                "Session branch commit message must have a non-empty title for review-request \
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

/// Returns one forge-native review-request creation helper for a pushed
/// session.
async fn branch_review_request_creation_info(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    branch_name: &str,
) -> Option<ReviewRequestCreationInfo> {
    let repo_url = git_client
        .repo_url(branch_publish_session.folder.clone())
        .await
        .ok()?;
    let remote = forge::detect_remote(&repo_url).ok()?;

    Some(ReviewRequestCreationInfo {
        forge_kind: remote.forge_kind,
        web_url: remote
            .review_request_creation_url(branch_name, &branch_publish_session.base_branch)
            .ok(),
    })
}

/// Maps one branch-publish failure into blocked or failed popup copy.
#[cfg(test)]
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
                PublishBranchAction::PublishPullRequest => "publish the review request again",
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

    if forge::is_gitlab_host(host) {
        return Some(forge::ForgeKind::GitLab);
    }

    None
}

/// Returns whether `host` is a GitHub-style forge host.
fn is_github_host(host: &str) -> bool {
    host == "github.com" || host.ends_with(".github.com")
}

/// Attempts to infer one forge kind from host-like tokens inside free-form
/// git push error text.
fn detected_forge_kind_from_text(detail_message: &str) -> Option<forge::ForgeKind> {
    for token in detail_message.split_whitespace() {
        let normalized_host = normalized_host_token(token);
        if normalized_host.is_empty() {
            continue;
        }

        if is_github_host(normalized_host) {
            return Some(forge::ForgeKind::GitHub);
        }

        if forge::is_gitlab_host(normalized_host) {
            return Some(forge::ForgeKind::GitLab);
        }
    }

    None
}

/// Normalizes one host-like token found in free-form error text so forge
/// detection can inspect just the hostname.
fn normalized_host_token(token: &str) -> &str {
    let token = token
        .trim()
        .trim_matches(|character: char| "\"'`()[]{}<>,;:".contains(character));
    let token = token
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("ssh://");
    let token = token.rsplit_once('@').map_or(token, |(_, host)| host);
    let token = token.split('/').next().unwrap_or(token);

    strip_port(token)
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
    use crate::infra::db::Database;
    use crate::infra::git;

    #[tokio::test]
    async fn review_request_remote_attaches_session_worktree_to_detected_remote() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let branch_publish_session = BranchPublishTaskSession {
            base_branch: "main".to_string(),
            folder: session_folder.clone(),
            id: "session-id".to_string(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::Review,
        };
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_repo_url()
            .once()
            .withf({
                let session_folder = session_folder.clone();
                move |candidate_folder| candidate_folder == &session_folder
            })
            .returning(|_| {
                Box::pin(async { Ok("https://gitlab.com/agentty-xyz/agentty.git".to_string()) })
            });
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .withf(|repo_url| repo_url == "https://gitlab.com/agentty-xyz/agentty.git")
            .returning(|_| {
                Ok(forge::ForgeRemote {
                    command_working_directory: None,
                    forge_kind: forge::ForgeKind::GitLab,
                    host: "gitlab.com".to_string(),
                    namespace: "agentty-xyz".to_string(),
                    project: "agentty".to_string(),
                    repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
                    web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
                })
            });

        // Act
        let remote = review_request_remote(
            &branch_publish_session,
            Arc::new(mock_git_client),
            &mock_review_request_client,
        )
        .await
        .expect("remote should resolve");

        // Assert
        assert_eq!(remote.command_working_directory, Some(session_folder));
        assert_eq!(remote.forge_kind, forge::ForgeKind::GitLab);
    }

    #[tokio::test]
    async fn push_session_branch_to_remote_persists_upstream_reference() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-id", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let expected_session_folder = session_folder.clone();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_remote_branch_exists()
            .once()
            .returning(|_, _| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(move |folder, remote_branch_name| {
                folder == &expected_session_folder && remote_branch_name == "wt/session-id"
            })
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));

        // Act
        let upstream_reference = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("wt/session-id"),
            None,
        )
        .await
        .expect("branch push should succeed");
        let persisted_session = database
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|session| session.id == "session-id")
            .expect("missing session row");

        // Assert
        assert_eq!(upstream_reference, "origin/wt/session-id");
        assert_eq!(
            persisted_session.published_upstream_ref.as_deref(),
            Some("origin/wt/session-id")
        );
    }

    /// Describes one auth-guidance parsing scenario for `branch_push_failure`.
    struct AuthGuidanceCase {
        error: &'static str,
        expected_cli_guidance: Option<&'static str>,
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
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "password prompt without scheme",
                error: "Git push failed: fatal: could not read Password for 'github.com/OpenAI/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "github url with port and subpath",
                error: "Git push failed: fatal: could not read Username for 'https://user@github.com:443/openai/agentty/path': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "gitlab host uses glab guidance",
                error: "Git push failed: fatal: could not read Username for 'https://gitlab.com/openai/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("glab auth login"),
            },
            AuthGuidanceCase {
                name: "self-hosted gitlab token uses glab guidance",
                error: "Git push failed: authentication failed while contacting gitlab.company.org for review branch",
                expected_cli_guidance: Some("glab auth login"),
            },
            AuthGuidanceCase {
                name: "non-forge host falls back to generic guidance",
                error: "Git push failed: fatal: could not read Username for 'https://example.com/openai/agentty': terminal prompts disabled",
                expected_cli_guidance: None,
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
            if let Some(expected_cli_guidance) = case.expected_cli_guidance {
                assert!(
                    failure.message.contains(expected_cli_guidance),
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
                    !failure.message.contains("glab auth login"),
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

    #[test]
    fn branch_push_success_message_uses_gitlab_merge_request_copy() {
        // Arrange
        let review_request_creation = ReviewRequestCreationInfo {
            forge_kind: forge::ForgeKind::GitLab,
            web_url: Some(
                "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/new".to_string(),
            ),
        };

        // Act
        let message = branch_push_success_message("wt/session-1", Some(&review_request_creation));

        // Assert
        assert!(message.contains("create the merge request"));
        assert!(message.contains("gitlab.com/agentty-xyz/agentty/-/merge_requests/new"));
    }

    #[test]
    fn pull_request_publish_success_message_uses_gitlab_display_copy() {
        // Arrange
        let review_request = ReviewRequest {
            last_refreshed_at: 42,
            summary: forge::ReviewRequestSummary {
                display_id: "!24".to_string(),
                forge_kind: forge::ForgeKind::GitLab,
                source_branch: "wt/session-1".to_string(),
                state: forge::ReviewRequestState::Open,
                status_summary: Some("Draft".to_string()),
                target_branch: "main".to_string(),
                title: "Add GitLab support".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24".to_string(),
            },
        };

        // Act
        let title = review_request_publish_success_title(&review_request);
        let message = pull_request_publish_success_message("wt/session-1", &review_request);

        // Assert
        assert_eq!(title, "GitLab merge request published");
        assert!(message.contains("GitLab merge request !24 is ready"));
    }

    #[tokio::test]
    async fn push_blocks_when_custom_remote_branch_already_exists() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-id", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_remote_branch_exists()
            .once()
            .returning(|_, _| Box::pin(async { Ok(true) }));

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("feature/existing"),
            None,
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("already exists"));
    }

    #[tokio::test]
    async fn push_skips_existence_check_when_upstream_ref_already_set() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-id", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let expected_folder = session_folder.clone();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(move |folder, branch| folder == &expected_folder && branch == "feature/existing")
            .returning(|_, _| Box::pin(async { Ok("origin/feature/existing".to_string()) }));

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("feature/existing"),
            Some("origin/feature/existing"),
        )
        .await;

        // Assert
        let upstream = result.expect("push should succeed");
        assert_eq!(upstream, "origin/feature/existing");
    }

    #[tokio::test]
    async fn push_skips_existence_check_when_no_custom_branch_name() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-id", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_push_current_branch()
            .once()
            .returning(|_| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            None,
            None,
        )
        .await;

        // Assert
        let upstream = result.expect("push should succeed");
        assert_eq!(upstream, "origin/wt/session-id");
    }

    #[tokio::test]
    async fn push_shows_auth_guidance_when_ls_remote_returns_auth_error() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-id", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_remote_branch_exists()
            .once()
            .returning(|_, _| {
                Box::pin(async {
                    Err(git::GitError::CommandFailed {
                        command: "git ls-remote".to_string(),
                        stderr: "fatal: could not read Username for \
                                 'https://github.com/org/repo': terminal prompts disabled"
                            .to_string(),
                    })
                })
            });

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("feature/new-branch"),
            None,
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("Git push requires authentication"));
        assert!(failure.message.contains("push the branch again"));
        assert!(failure.message.contains("gh auth login"));
    }

    #[tokio::test]
    async fn push_shows_auth_guidance_when_push_returns_auth_error() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-id", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_push_current_branch()
            .once()
            .returning(|_| {
                Box::pin(async {
                    Err(git::GitError::CommandFailed {
                        command: "git push".to_string(),
                        stderr:
                            "fatal: Authentication failed for 'https://gitlab.com/org/repo.git/'"
                                .to_string(),
                    })
                })
            });

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            None,
            None,
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("Git push requires authentication"));
        assert!(failure.message.contains("push the branch again"));
        assert!(failure.message.contains("glab auth login"));
    }

    #[test]
    fn with_action_preserves_blocked_distinction() {
        // Arrange
        let blocked =
            BranchPublishTaskFailure::blocked(PublishBranchAction::Push, "auth error".to_string());
        let failed = BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "generic error".to_string(),
        );

        // Act
        let adjusted_blocked = blocked.with_action(PublishBranchAction::PublishPullRequest);
        let adjusted_failed = failed.with_action(PublishBranchAction::PublishPullRequest);

        // Assert
        assert_eq!(adjusted_blocked.title, "Review request publish blocked");
        assert_eq!(adjusted_blocked.message, "auth error");
        assert_eq!(adjusted_failed.title, "Review request publish failed");
        assert_eq!(adjusted_failed.message, "generic error");
    }
}
