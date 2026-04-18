//! GitLab review-request adapter routed through the `glab` CLI.

use std::sync::Arc;

use serde::Deserialize;
use url::Url;

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandOutput, ForgeCommandRunner, ForgeKind,
    ForgeRemote, ReviewRequestError, ReviewRequestState, ReviewRequestSummary,
    command_output_detail, is_gitlab_host, looks_like_authentication_failure,
    looks_like_host_resolution_failure, map_spawn_error, normalize_provider_label,
    parse_remote_url, status_summary_parts, strip_port,
};

/// GitLab merge-request adapter that normalizes `glab` command output.
pub(crate) struct GitLabReviewRequestAdapter {
    command_runner: Arc<dyn ForgeCommandRunner>,
}

impl GitLabReviewRequestAdapter {
    /// Builds one GitLab adapter from a forge command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self { command_runner }
    }

    /// Returns normalized GitLab remote metadata when `repo_url` is supported.
    pub(crate) fn detect_remote(repo_url: &str) -> Option<ForgeRemote> {
        let parsed_remote = parse_remote_url(repo_url)?;
        if !is_gitlab_host(strip_port(&parsed_remote.host)) {
            return None;
        }

        Some(parsed_remote.into_forge_remote(ForgeKind::GitLab))
    }

    /// Finds one existing merge request for `source_branch`.
    pub(crate) async fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> Result<Option<ReviewRequestSummary>, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.find_by_source_branch_after_auth(remote, source_branch)
            .await
    }

    /// Creates one new draft merge request from `input`.
    pub(crate) async fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.create_review_request_after_auth(remote, input).await
    }

    /// Refreshes one existing merge request by display id.
    pub(crate) async fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.refresh_review_request_after_auth(remote, display_id)
            .await
    }

    /// Finds one existing merge request after authentication has been
    /// verified.
    async fn find_by_source_branch_after_auth(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> Result<Option<ReviewRequestSummary>, ReviewRequestError> {
        let output = self
            .run_review_command(
                &remote,
                lookup_command(&remote, &source_branch),
                "find merge request",
            )
            .await?;
        let display_id = parse_lookup_display_id(&output.stdout).map_err(|message| {
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitLab,
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

    /// Creates one new draft merge request after authentication has been
    /// verified.
    async fn create_review_request_after_auth(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        let output = self
            .run_review_command(
                &remote,
                create_command(&remote, &input),
                "create merge request",
            )
            .await?;
        let display_id = parse_create_display_id(&output.stdout).map_err(|message| {
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitLab,
                message,
            }
        })?;

        self.refresh_review_request_after_auth(remote, display_id)
            .await
    }

    /// Refreshes one merge request after authentication has been verified.
    async fn refresh_review_request_after_auth(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        let merge_request_iid = parse_display_id(&display_id)?;
        let output = self
            .run_review_command(
                &remote,
                view_command(&remote, &merge_request_iid),
                "refresh merge request",
            )
            .await?;

        parse_view_response(&output.stdout).map_err(|message| ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitLab,
            message,
        })
    }

    /// Verifies that `glab` is installed and authenticated for `remote.host`.
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
                forge_kind: ForgeKind::GitLab,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::AuthenticationRequired {
            detail: Some(command_output_detail(&output)),
            forge_kind: ForgeKind::GitLab,
            host: remote.host.clone(),
        })
    }

    /// Runs one authenticated `glab` command and normalizes common failures.
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
                forge_kind: ForgeKind::GitLab,
                host: remote.host.clone(),
            });
        }

        if looks_like_authentication_failure(&detail, ForgeKind::GitLab) {
            return Err(ReviewRequestError::AuthenticationRequired {
                detail: Some(detail),
                forge_kind: ForgeKind::GitLab,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitLab,
            message: format!("{operation}: {detail}"),
        })
    }
}

/// Builds the `glab auth status` command for one GitLab host.
fn auth_status_command(remote: &ForgeRemote) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "auth".to_string(),
            "status".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
        ],
    )
}

/// Builds the `glab mr list` command for `source_branch`.
fn lookup_command(remote: &ForgeRemote, source_branch: &str) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "mr".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            remote.web_url.clone(),
            "--all".to_string(),
            "--source-branch".to_string(),
            source_branch.to_string(),
            "--order".to_string(),
            "created_at".to_string(),
            "--sort".to_string(),
            "desc".to_string(),
            "--per-page".to_string(),
            "1".to_string(),
            "--output".to_string(),
            "json".to_string(),
        ],
    )
}

/// Builds the `glab mr create` command for `input`.
///
/// GitLab merge requests default to draft so session-published review requests
/// do not appear ready for merge before the user chooses to mark them ready.
fn create_command(remote: &ForgeRemote, input: &CreateReviewRequestInput) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "mr".to_string(),
            "create".to_string(),
            "--repo".to_string(),
            remote.web_url.clone(),
            "--draft".to_string(),
            "--source-branch".to_string(),
            input.source_branch.clone(),
            "--target-branch".to_string(),
            input.target_branch.clone(),
            "--title".to_string(),
            input.title.clone(),
            "--description".to_string(),
            input.body.clone().unwrap_or_default(),
            "--yes".to_string(),
        ],
    )
}

/// Builds the `glab mr view` command for one merge-request IID.
fn view_command(remote: &ForgeRemote, merge_request_iid: &str) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "mr".to_string(),
            "view".to_string(),
            merge_request_iid.to_string(),
            "--repo".to_string(),
            remote.web_url.clone(),
            "--output".to_string(),
            "json".to_string(),
        ],
    )
}

/// Builds one base `glab` command with deterministic color settings and the
/// optional session worktree for repository-aware host detection.
fn gitlab_command(
    remote: &ForgeRemote,
    executable: &'static str,
    arguments: Vec<String>,
) -> ForgeCommand {
    ForgeCommand::new(executable, arguments)
        .with_environment("CLICOLOR", "0")
        .with_environment("NO_COLOR", "1")
        .with_environment("GITLAB_HOST", remote.host.clone())
        .with_optional_working_directory(remote.command_working_directory.clone())
}

/// Parses one optional display id from a GitLab merge-request lookup response.
fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
    let merge_requests: Vec<GitLabLookupResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab merge-request lookup response: {error}"))?;

    Ok(merge_requests
        .first()
        .map(|merge_request| format!("!{}", merge_request.iid)))
}

/// Parses one merge-request display id from `glab mr create` stdout.
fn parse_create_display_id(stdout: &str) -> Result<String, String> {
    let created_url = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "missing GitLab merge-request URL in create response".to_string())?;
    let created_url = Url::parse(created_url)
        .map_err(|error| format!("invalid GitLab merge-request create response URL: {error}"))?;
    let path_segments = created_url
        .path_segments()
        .ok_or_else(|| "invalid GitLab merge-request create response URL path".to_string())?
        .collect::<Vec<_>>();
    let merge_request_index = path_segments
        .iter()
        .rposition(|segment| *segment == "merge_requests")
        .ok_or_else(|| "missing merge request path segment in create response URL".to_string())?;
    let merge_request_iid = path_segments
        .get(merge_request_index + 1)
        .ok_or_else(|| "missing merge request iid in create response URL".to_string())?;
    let display_id = format!("!{merge_request_iid}");
    parse_display_id(&display_id).map_err(|error| match error {
        ReviewRequestError::OperationFailed { message, .. } => message,
        _ => "invalid GitLab merge-request display id".to_string(),
    })?;

    Ok(display_id)
}

/// Parses one merge-request summary from a `glab mr view --output json`
/// response.
fn parse_view_response(stdout: &str) -> Result<ReviewRequestSummary, String> {
    let merge_request: GitLabViewResponse = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab merge-request view response: {error}"))?;
    let state = merge_request.review_request_state();
    let status_summary = merge_request.status_summary();

    Ok(ReviewRequestSummary {
        display_id: format!("!{}", merge_request.iid),
        forge_kind: ForgeKind::GitLab,
        source_branch: merge_request.source_branch,
        state,
        status_summary,
        target_branch: merge_request.target_branch,
        title: merge_request.title,
        web_url: merge_request.web_url,
    })
}

/// Parses one GitLab merge-request display id into the numeric argument for
/// `glab`.
fn parse_display_id(display_id: &str) -> Result<String, ReviewRequestError> {
    let trimmed = display_id.trim().trim_start_matches('!');
    if trimmed.is_empty() || !trimmed.chars().all(|character| character.is_ascii_digit()) {
        return Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitLab,
            message: format!("invalid GitLab merge-request display id: `{display_id}`"),
        });
    }

    Ok(trimmed.to_string())
}

/// Formats one GitLab merge-status label for the UI.
fn merge_status_summary(
    merge_status: Option<&str>,
    detailed_merge_status: Option<&str>,
) -> Option<String> {
    let status = detailed_merge_status.or(merge_status)?;

    match status {
        "can_be_merged" | "mergeable" => Some("Mergeable".to_string()),
        "cannot_be_merged" => Some("Conflicts".to_string()),
        "cannot_be_merged_recheck" | "checking" | "unchecked" => Some("Checking".to_string()),
        "ci_still_running" | "commits_status" => Some("Checks pending".to_string()),
        "ci_must_pass" => Some("Checks required".to_string()),
        "discussions_not_resolved" => Some("Discussions unresolved".to_string()),
        "draft_status" | "not_open" => None,
        other => Some(normalize_provider_label(other)),
    }
}

/// Minimal GitLab list payload used to find an existing merge request.
#[derive(Deserialize)]
struct GitLabLookupResponse {
    iid: u64,
}

/// GitLab merge-request JSON payload returned by `glab mr view --output json`.
#[derive(Deserialize)]
struct GitLabViewResponse {
    #[serde(default)]
    draft: bool,
    #[serde(rename = "detailed_merge_status")]
    detailed_merge_status: Option<String>,
    iid: u64,
    #[serde(rename = "merge_status")]
    merge_status: Option<String>,
    #[serde(rename = "merged_at")]
    merged_at: Option<String>,
    #[serde(rename = "source_branch")]
    source_branch: String,
    state: String,
    #[serde(rename = "target_branch")]
    target_branch: String,
    title: String,
    #[serde(rename = "web_url")]
    web_url: String,
}

impl GitLabViewResponse {
    /// Maps GitLab state fields into the normalized review-request state.
    fn review_request_state(&self) -> ReviewRequestState {
        if self.merged_at.is_some() || self.state.eq_ignore_ascii_case("merged") {
            return ReviewRequestState::Merged;
        }

        if matches!(self.state.as_str(), "closed" | "locked") {
            return ReviewRequestState::Closed;
        }

        ReviewRequestState::Open
    }

    /// Formats the provider-specific status summary for the UI.
    fn status_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.draft {
            parts.push("Draft".to_string());
        }

        if let Some(merge_summary) = merge_status_summary(
            self.merge_status.as_deref(),
            self.detailed_merge_status.as_deref(),
        ) {
            parts.push(merge_summary);
        }

        status_summary_parts(&parts)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use mockall::Sequence;

    use super::*;
    use crate::command::MockForgeCommandRunner;

    #[tokio::test]
    async fn find_by_source_branch_builds_lookup_and_refresh_commands() {
        // Arrange
        let remote = gitlab_remote();
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
            .returning(|_| Box::pin(async { Ok(success_output(r#"[{"iid":42}]"#.to_string())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .find_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("GitLab lookup should succeed");

        // Assert
        assert_eq!(
            review_request,
            Some(ReviewRequestSummary {
                display_id: "!42".to_string(),
                forge_kind: ForgeKind::GitLab,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Draft, Mergeable".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn find_by_source_branch_returns_none_for_empty_lookup_response() {
        // Arrange
        let remote = gitlab_remote();
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
            .returning(|_| Box::pin(async { Ok(success_output("[]".to_string())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .find_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("GitLab lookup should succeed");

        // Assert
        assert_eq!(review_request, None);
    }

    #[tokio::test]
    async fn create_review_request_builds_create_command_and_returns_summary() {
        // Arrange
        let remote = gitlab_remote();
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
                        "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42\n".to_string(),
                    ))
                })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .create_review_request(remote, input)
            .await
            .expect("GitLab create should succeed");

        // Assert
        assert_eq!(review_request.display_id, "!42");
        assert_eq!(review_request.forge_kind, ForgeKind::GitLab);
        assert_eq!(
            review_request.web_url,
            "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
        );
    }

    #[test]
    fn detect_remote_supports_gitlab_hosts() {
        // Arrange
        let repo_url = "https://gitlab.com/agentty-xyz/agentty.git";

        // Act
        let remote =
            GitLabReviewRequestAdapter::detect_remote(repo_url).expect("gitlab remote expected");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitLab);
        assert_eq!(remote.host, "gitlab.com");
        assert_eq!(remote.project_path(), "agentty-xyz/agentty");
    }

    #[test]
    fn parse_display_id_rejects_invalid_merge_request_reference() {
        // Arrange
        let display_id = "!not-a-number";

        // Act
        let error = parse_display_id(display_id).expect_err("invalid display id should fail");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitLab,
                message: "invalid GitLab merge-request display id: `!not-a-number`".to_string(),
            }
        );
    }

    #[test]
    fn parse_create_display_id_reads_merge_request_iid_from_created_url() {
        // Arrange
        let stdout = "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42\n";

        // Act
        let display_id = parse_create_display_id(stdout).expect("create output should parse");

        // Assert
        assert_eq!(display_id, "!42");
    }

    #[test]
    fn create_command_uses_remote_working_directory_for_glab_git_context() {
        // Arrange
        let remote =
            gitlab_remote().with_command_working_directory(PathBuf::from("/tmp/session-worktree"));
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let command = create_command(&remote, &input);

        // Assert
        assert_eq!(
            command.working_directory,
            Some(PathBuf::from("/tmp/session-worktree"))
        );
        assert!(
            command
                .environment
                .contains(&("GITLAB_HOST".to_string(), "gitlab.com".to_string()))
        );
    }

    /// Builds one normalized GitLab remote for command-construction tests.
    fn gitlab_remote() -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Builds one successful command output with `stdout`.
    fn success_output(stdout: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout,
        }
    }

    /// Returns one representative GitLab merge-request JSON response.
    fn gitlab_view_json() -> String {
        r#"{
            "draft": true,
            "detailed_merge_status": "can_be_merged",
            "iid": 42,
            "merge_status": "can_be_merged",
            "merged_at": null,
            "source_branch": "feature/forge",
            "state": "opened",
            "target_branch": "main",
            "title": "Add forge review support",
            "web_url": "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
        }"#
        .to_string()
    }
}
