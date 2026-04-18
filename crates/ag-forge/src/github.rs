//! GitHub review-request adapter routed through the `gh` CLI.

use std::sync::Arc;

use serde::Deserialize;

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandOutput, ForgeCommandRunner, ForgeKind,
    ForgeRemote, ReviewRequestError, ReviewRequestState, ReviewRequestSummary,
    command_output_detail, looks_like_authentication_failure, looks_like_host_resolution_failure,
    map_spawn_error, normalize_provider_label, parse_remote_url, status_summary_parts, strip_port,
};

/// GitHub pull-request adapter that normalizes `gh` command output.
pub(crate) struct GitHubReviewRequestAdapter {
    command_runner: Arc<dyn ForgeCommandRunner>,
}

impl GitHubReviewRequestAdapter {
    /// Builds one GitHub adapter from a forge command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self { command_runner }
    }

    /// Returns normalized GitHub remote metadata when `repo_url` is supported.
    pub(crate) fn detect_remote(repo_url: &str) -> Option<ForgeRemote> {
        let parsed_remote = parse_remote_url(repo_url)?;
        if strip_port(&parsed_remote.host) != "github.com" {
            return None;
        }

        Some(parsed_remote.into_forge_remote(ForgeKind::GitHub))
    }

    /// Finds one existing pull request for `source_branch`.
    pub(crate) async fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> Result<Option<ReviewRequestSummary>, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.find_by_source_branch_after_auth(remote, source_branch)
            .await
    }

    /// Creates one new draft pull request from `input`.
    pub(crate) async fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.create_review_request_after_auth(remote, input).await
    }

    /// Refreshes one existing pull request by display id.
    pub(crate) async fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.refresh_review_request_after_auth(remote, display_id)
            .await
    }

    /// Finds one existing pull request after authentication has been verified.
    async fn find_by_source_branch_after_auth(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> Result<Option<ReviewRequestSummary>, ReviewRequestError> {
        let output = self
            .run_review_command(
                &remote,
                lookup_command(&remote, &source_branch),
                "find pull request",
            )
            .await?;
        let display_id = parse_lookup_display_id(&output.stdout).map_err(|message| {
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message,
            }
        })?;

        let Some(display_id) = display_id else {
            return Ok(None);
        };

        self.refresh_review_request_after_auth(remote, display_id)
            .await
            .map(Some)
    }

    /// Creates one new draft pull request after authentication has been
    /// verified.
    async fn create_review_request_after_auth(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        let source_branch = input.source_branch.clone();
        self.run_review_command(
            &remote,
            create_command(&remote, &input),
            "create pull request",
        )
        .await?;

        self.find_by_source_branch_after_auth(remote, source_branch)
            .await?
            .ok_or_else(|| ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "GitHub pull request was created but could not be reloaded".to_string(),
            })
    }

    /// Refreshes one pull request after authentication has been verified.
    async fn refresh_review_request_after_auth(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        let pull_request_number = parse_display_id(&display_id)?;
        let output = self
            .run_review_command(
                &remote,
                view_command(&remote, &pull_request_number),
                "refresh pull request",
            )
            .await?;

        parse_view_response(&output.stdout).map_err(|message| ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message,
        })
    }

    /// Verifies that `gh` is installed and authenticated for `remote.host`.
    async fn ensure_authenticated(&self, remote: &ForgeRemote) -> Result<(), ReviewRequestError> {
        let output = self
            .command_runner
            .run(auth_status_command(remote))
            .await
            .map_err(|error| map_spawn_error(remote, error))?;
        if output.success() {
            return Ok(());
        }

        if looks_like_host_resolution_failure(&command_output_detail(&output)) {
            return Err(ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitHub,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::AuthenticationRequired {
            detail: Some(command_output_detail(&output)),
            forge_kind: ForgeKind::GitHub,
            host: remote.host.clone(),
        })
    }

    /// Runs one authenticated `gh` command and normalizes common failures.
    async fn run_review_command(
        &self,
        remote: &ForgeRemote,
        command: ForgeCommand,
        operation: &str,
    ) -> Result<ForgeCommandOutput, ReviewRequestError> {
        let output = self
            .command_runner
            .run(command)
            .await
            .map_err(|error| map_spawn_error(remote, error))?;
        if output.success() {
            return Ok(output);
        }

        let detail = command_output_detail(&output);
        if looks_like_host_resolution_failure(&detail) {
            return Err(ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitHub,
                host: remote.host.clone(),
            });
        }

        if looks_like_authentication_failure(&detail, ForgeKind::GitHub) {
            return Err(ReviewRequestError::AuthenticationRequired {
                detail: Some(detail),
                forge_kind: ForgeKind::GitHub,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: format!("{operation}: {detail}"),
        })
    }
}

/// Builds the `gh auth status` command for one GitHub host.
fn auth_status_command(remote: &ForgeRemote) -> ForgeCommand {
    ForgeCommand::new(
        "gh",
        vec![
            "auth".to_string(),
            "status".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
        ],
    )
    .with_environment("CLICOLOR", "0")
    .with_environment("NO_COLOR", "1")
}

/// Builds the `gh api` lookup command for `source_branch`.
fn lookup_command(remote: &ForgeRemote, source_branch: &str) -> ForgeCommand {
    ForgeCommand::new(
        "gh",
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "--method".to_string(),
            "GET".to_string(),
            format!("repos/{}/{}/pulls", remote.namespace, remote.project),
            "-f".to_string(),
            format!("head={}:{}", remote.namespace, source_branch),
            "-f".to_string(),
            "state=all".to_string(),
            "-f".to_string(),
            "sort=created".to_string(),
            "-f".to_string(),
            "direction=desc".to_string(),
            "-f".to_string(),
            "per_page=1".to_string(),
        ],
    )
    .with_environment("CLICOLOR", "0")
    .with_environment("NO_COLOR", "1")
}

/// Builds the `gh pr create` command for `input`.
///
/// GitHub pull requests default to draft so session-published review requests
/// do not appear ready for merge before the user chooses to mark them ready.
fn create_command(remote: &ForgeRemote, input: &CreateReviewRequestInput) -> ForgeCommand {
    ForgeCommand::new(
        "gh",
        vec![
            "pr".to_string(),
            "create".to_string(),
            "--draft".to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--head".to_string(),
            input.source_branch.clone(),
            "--base".to_string(),
            input.target_branch.clone(),
            "--title".to_string(),
            input.title.clone(),
            "--body".to_string(),
            input.body.clone().unwrap_or_default(),
        ],
    )
    .with_environment("CLICOLOR", "0")
    .with_environment("NO_COLOR", "1")
}

/// Builds the `gh pr view` command for one pull-request number.
fn view_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
    ForgeCommand::new(
        "gh",
        vec![
            "pr".to_string(),
            "view".to_string(),
            pull_request_number.to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--json".to_string(),
            "number,title,state,url,baseRefName,headRefName,isDraft,mergeStateStatus,\
             reviewDecision,mergedAt"
                .to_string(),
        ],
    )
    .with_environment("CLICOLOR", "0")
    .with_environment("NO_COLOR", "1")
}

/// Parses one optional display id from a GitHub pull-request lookup response.
fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
    let pull_requests: Vec<GitHubLookupResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request lookup response: {error}"))?;

    Ok(pull_requests
        .first()
        .map(|pull_request| format!("#{}", pull_request.number)))
}

/// Parses one pull-request summary from a `gh pr view` JSON response.
fn parse_view_response(stdout: &str) -> Result<ReviewRequestSummary, String> {
    let pull_request: GitHubViewResponse = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request view response: {error}"))?;
    let state = pull_request.review_request_state();
    let status_summary = pull_request.status_summary();

    Ok(ReviewRequestSummary {
        display_id: format!("#{}", pull_request.number),
        forge_kind: ForgeKind::GitHub,
        source_branch: pull_request.head_ref_name,
        state,
        status_summary,
        target_branch: pull_request.base_ref_name,
        title: pull_request.title,
        web_url: pull_request.url,
    })
}

/// Parses one GitHub pull-request display id into the numeric argument for
/// `gh`.
fn parse_display_id(display_id: &str) -> Result<String, ReviewRequestError> {
    let trimmed = display_id.trim().trim_start_matches('#');
    if trimmed.is_empty() || !trimmed.chars().all(|character| character.is_ascii_digit()) {
        return Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: format!("invalid GitHub pull-request display id: `{display_id}`"),
        });
    }

    Ok(trimmed.to_string())
}

/// Formats one GitHub merge-state label for the UI.
fn merge_state_summary(merge_state_status: Option<&str>) -> Option<String> {
    match merge_state_status {
        Some("BLOCKED") => Some("Blocked".to_string()),
        Some("CLEAN") => Some("Mergeable".to_string()),
        Some("DIRTY") => Some("Conflicts".to_string()),
        Some("HAS_HOOKS") => Some("Hooks pending".to_string()),
        Some("UNSTABLE") => Some("Checks pending".to_string()),
        Some("UNKNOWN") | None => None,
        Some(other) => Some(normalize_provider_label(other)),
    }
}

/// Formats one GitHub review-decision label for the UI.
fn review_decision_summary(review_decision: Option<&str>) -> Option<String> {
    match review_decision {
        Some("APPROVED") => Some("Approved".to_string()),
        Some("CHANGES_REQUESTED") => Some("Changes requested".to_string()),
        Some("REVIEW_REQUIRED") => Some("Review required".to_string()),
        Some(other) => Some(normalize_provider_label(other)),
        None => None,
    }
}

/// Minimal GitHub API lookup payload used to find an existing pull request.
#[derive(Deserialize)]
struct GitHubLookupResponse {
    number: u64,
}

/// GitHub pull-request JSON payload returned by `gh pr view --json`.
#[derive(Deserialize)]
struct GitHubViewResponse {
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
    number: u64,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    state: String,
    title: String,
    url: String,
}

impl GitHubViewResponse {
    /// Maps GitHub state fields into the normalized review-request state.
    fn review_request_state(&self) -> ReviewRequestState {
        if self.merged_at.is_some() || self.state == "MERGED" {
            return ReviewRequestState::Merged;
        }

        if self.state == "CLOSED" {
            return ReviewRequestState::Closed;
        }

        ReviewRequestState::Open
    }

    /// Formats the provider-specific status summary for the UI.
    fn status_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.is_draft {
            parts.push("Draft".to_string());
        }

        if let Some(review_summary) = review_decision_summary(self.review_decision.as_deref()) {
            parts.push(review_summary);
        }

        if let Some(merge_summary) = merge_state_summary(self.merge_state_status.as_deref()) {
            parts.push(merge_summary);
        }

        status_summary_parts(&parts)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mockall::Sequence;

    use super::*;
    use crate::command::MockForgeCommandRunner;

    #[tokio::test]
    async fn find_by_source_branch_builds_lookup_and_refresh_commands() {
        // Arrange
        let remote = github_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &auth_status_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &lookup_command(&remote, "feature/forge")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .find_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("GitHub lookup should succeed");

        // Assert
        assert_eq!(
            review_request,
            Some(ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Approved, Mergeable".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn create_review_request_builds_create_command_and_returns_summary() {
        // Arrange
        let remote = github_remote();
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &auth_status_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();
                let input = input.clone();

                move |command| command == &create_command(&remote, &input)
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(success_output(
                        "https://github.com/agentty-xyz/agentty/pull/42\n".to_string(),
                    ))
                })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &lookup_command(&remote, "feature/forge")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .create_review_request(remote, input)
            .await
            .expect("GitHub create should succeed");

        // Assert
        assert_eq!(review_request.display_id, "#42");
        assert_eq!(
            review_request.status_summary.as_deref(),
            Some("Approved, Mergeable")
        );
    }

    #[test]
    fn create_command_marks_pull_requests_as_draft_by_default() {
        // Arrange
        let remote = github_remote();
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let command = create_command(&remote, &input);

        // Assert
        assert_eq!(command.executable, "gh");
        assert!(
            command
                .arguments
                .iter()
                .any(|argument| argument == "--draft")
        );
    }

    #[tokio::test]
    async fn refresh_review_request_maps_authentication_error() {
        // Arrange
        let remote = github_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &auth_status_command(&remote)
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(failure_output(
                        "You are not logged into any GitHub hosts. Run `gh auth login`."
                            .to_string(),
                    ))
                })
            });
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let error = adapter
            .refresh_review_request(remote, "#42".to_string())
            .await
            .expect_err("missing auth should be normalized");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::AuthenticationRequired {
                detail: Some(
                    "You are not logged into any GitHub hosts. Run `gh auth login`.".to_string()
                ),
                forge_kind: ForgeKind::GitHub,
                host: "github.com".to_string(),
            }
        );
    }

    fn github_remote() -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    fn github_view_json() -> String {
        r#"{
            "number": 42,
            "title": "Add forge review support",
            "state": "OPEN",
            "url": "https://github.com/agentty-xyz/agentty/pull/42",
            "baseRefName": "main",
            "headRefName": "feature/forge",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "reviewDecision": "APPROVED",
            "mergedAt": null
        }"#
        .to_string()
    }

    fn success_output(stdout: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout,
        }
    }

    fn failure_output(stderr: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(1),
            stderr,
            stdout: String::new(),
        }
    }
}
