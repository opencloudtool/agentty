//! GitLab review-request adapter routed through the `glab` CLI.

use std::sync::Arc;

use serde::Deserialize;

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandError, ForgeCommandOutput,
    ForgeCommandRunner, ForgeKind, ForgeRemote, ReviewRequestError, ReviewRequestState,
    ReviewRequestSummary, command_output_detail, parse_remote_url,
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
        if !parsed_remote.host_is_gitlab() {
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

    /// Finds one existing merge request after authentication has been verified.
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
        let source_branch = input.source_branch.clone();
        self.run_review_command(
            &remote,
            create_command(&remote, &input),
            "create merge request",
        )
        .await?;

        self.find_by_source_branch_after_auth(remote, source_branch)
            .await?
            .ok_or_else(|| ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitLab,
                message: "GitLab merge request was created but could not be reloaded".to_string(),
            })
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
        if output.success() || Self::auth_status_indicates_authenticated(&output) {
            return Ok(());
        }

        if looks_like_host_resolution_failure(&command_output_detail(&output)) {
            return Err(ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitLab,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::AuthenticationRequired {
            forge_kind: ForgeKind::GitLab,
            host: remote.host.clone(),
        })
    }

    /// Returns whether one `glab auth status` output still proves valid auth.
    ///
    /// Some `glab` versions report a non-zero exit status for incomplete local
    /// git-protocol setup even when the API token is present and usable. Treat
    /// those responses as authenticated so review-request creation can proceed.
    fn auth_status_indicates_authenticated(output: &ForgeCommandOutput) -> bool {
        let detail = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();

        detail.contains("logged in to") && detail.contains("token found:")
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

        if looks_like_authentication_failure(&detail) {
            return Err(ReviewRequestError::AuthenticationRequired {
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
    glab_command(remote, vec!["auth".to_string(), "status".to_string()])
}

/// Builds the `glab api` lookup command for `source_branch`.
fn lookup_command(remote: &ForgeRemote, source_branch: &str) -> ForgeCommand {
    glab_command(
        remote,
        vec![
            "api".to_string(),
            "--method".to_string(),
            "GET".to_string(),
            format!(
                "projects/{}/merge_requests",
                encode_project_path(&remote.project_path())
            ),
            "--output".to_string(),
            "json".to_string(),
            "-f".to_string(),
            format!("source_branch={source_branch}"),
            "-f".to_string(),
            "state=all".to_string(),
            "-f".to_string(),
            "per_page=1".to_string(),
            "-f".to_string(),
            "order_by=created_at".to_string(),
            "-f".to_string(),
            "sort=desc".to_string(),
        ],
    )
}

/// Builds the `glab mr create` command for `input`.
///
/// Uses `glab mr create` so GitLab draft state is expressed through the CLI's
/// native `--draft` support while keeping the flow non-interactive with
/// explicit title, description, branch, and confirmation arguments.
fn create_command(remote: &ForgeRemote, input: &CreateReviewRequestInput) -> ForgeCommand {
    let arguments = vec![
        "mr".to_string(),
        "create".to_string(),
        "--draft".to_string(),
        "--repo".to_string(),
        remote.web_url.clone(),
        "--source-branch".to_string(),
        input.source_branch.clone(),
        "--target-branch".to_string(),
        input.target_branch.clone(),
        "--title".to_string(),
        input.title.clone(),
        "--description".to_string(),
        input.body.clone().unwrap_or_default(),
        "--yes".to_string(),
    ];

    glab_command(remote, arguments)
}

/// Builds the `glab mr view` command for one merge-request iid.
fn view_command(remote: &ForgeRemote, merge_request_iid: &str) -> ForgeCommand {
    glab_command(
        remote,
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

/// Adds shared non-interactive defaults to one `glab` command.
///
/// Uses `GLAB_NO_PROMPT` so `glab` does not emit the deprecated `NO_PROMPT`
/// warning to stdout, which would corrupt JSON responses consumed by the
/// adapter. `GITLAB_HOST` and `GL_HOST` pin `glab` to the target server URL so
/// self-managed instances with explicit HTTPS ports do not rely on
/// `--hostname`, which rejects `host:port` values.
fn glab_command(remote: &ForgeRemote, arguments: Vec<String>) -> ForgeCommand {
    let server_url = gitlab_server_url(remote);

    ForgeCommand::new("glab", arguments)
        .with_environment("CLICOLOR", "0")
        .with_environment("GITLAB_HOST", server_url.clone())
        .with_environment("GL_HOST", server_url)
        .with_environment("GLAB_NO_PROMPT", "1")
        .with_environment("NO_COLOR", "1")
}

/// Returns the base server URL that `glab` should target for one remote.
fn gitlab_server_url(remote: &ForgeRemote) -> String {
    format!("https://{}", remote.host)
}

/// Maps one spawn-time failure into a normalized GitLab review-request error.
fn map_spawn_error(remote: &ForgeRemote, error: ForgeCommandError) -> ReviewRequestError {
    match error {
        ForgeCommandError::ExecutableNotFound { .. } => ReviewRequestError::CliNotInstalled {
            forge_kind: ForgeKind::GitLab,
        },
        ForgeCommandError::SpawnFailed { message, .. } => {
            if looks_like_host_resolution_failure(&message) {
                return ReviewRequestError::HostResolutionFailed {
                    forge_kind: ForgeKind::GitLab,
                    host: remote.host.clone(),
                };
            }

            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitLab,
                message: format!("failed to execute `glab`: {message}"),
            }
        }
    }
}

/// Parses one optional display id from a GitLab merge-request lookup response.
fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
    let merge_requests: Vec<GitLabLookupResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab merge-request lookup response: {error}"))?;

    Ok(merge_requests
        .first()
        .map(|merge_request| format!("!{}", merge_request.iid)))
}

/// Parses one merge-request summary from a `glab mr view` JSON response.
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

/// Parses one GitLab merge-request display id into the iid argument for `glab`.
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

/// Returns whether `detail` looks like a `glab` authentication failure.
fn looks_like_authentication_failure(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("glab auth login")
        || normalized_detail.contains("has not been authenticated with glab")
        || normalized_detail.contains("not logged")
        || normalized_detail.contains("authentication failed")
        || normalized_detail.contains("authentication required")
        || normalized_detail.contains("401 unauthorized")
}

/// Returns whether `detail` looks like a host-resolution failure.
fn looks_like_host_resolution_failure(detail: &str) -> bool {
    let normalized_detail = detail.to_ascii_lowercase();

    normalized_detail.contains("no such host")
        || normalized_detail.contains("name or service not known")
        || normalized_detail.contains("temporary failure in name resolution")
        || normalized_detail.contains("could not resolve host")
        || normalized_detail.contains("lookup ")
}

/// URL-encodes one GitLab project path so subgroups can be used in API routes.
fn encode_project_path(project_path: &str) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::new();
    for byte in project_path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0F)]));
        }
    }

    encoded
}

/// Formats one optional provider-specific status summary.
fn status_summary_parts(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }

    Some(parts.join(", "))
}

/// Formats one provider enum-like label into sentence case words.
fn normalize_provider_label(label: &str) -> String {
    let lowercase = label.replace('_', " ").to_ascii_lowercase();
    let mut characters = lowercase.chars();
    let Some(first_character) = characters.next() else {
        return String::new();
    };

    let mut normalized = first_character.to_uppercase().collect::<String>();
    normalized.push_str(characters.as_str());

    normalized
}

/// Minimal GitLab API lookup payload used to find an existing merge request.
#[derive(Deserialize)]
struct GitLabLookupResponse {
    iid: u64,
}

/// GitLab pipeline summary nested under one merge request.
#[derive(Deserialize)]
struct GitLabHeadPipeline {
    status: Option<String>,
}

/// GitLab approval summary nested under one merge request.
#[derive(Deserialize)]
struct GitLabApproval {
    user: GitLabUser,
}

/// GitLab user payload nested under approval summaries.
#[derive(Deserialize)]
struct GitLabUser {
    username: String,
}

/// GitLab merge-request JSON payload returned by `glab mr view --output json`.
#[derive(Deserialize)]
struct GitLabViewResponse {
    /// Approval entries reported by `glab`, or an empty list when the field is
    /// omitted.
    #[serde(default)]
    approved_by: Vec<GitLabApproval>,
    detailed_merge_status: Option<String>,
    /// Draft state reported by `glab`, defaulting to `false` when older
    /// payloads omit it.
    #[serde(default)]
    draft: bool,
    head_pipeline: Option<GitLabHeadPipeline>,
    iid: u64,
    merge_status: Option<String>,
    source_branch: String,
    state: String,
    target_branch: String,
    title: String,
    web_url: String,
}

impl GitLabViewResponse {
    /// Maps GitLab state strings into the normalized review-request state.
    fn review_request_state(&self) -> ReviewRequestState {
        match self.state.as_str() {
            "merged" => ReviewRequestState::Merged,
            "closed" => ReviewRequestState::Closed,
            _ => ReviewRequestState::Open,
        }
    }

    /// Formats the provider-specific status summary for the UI.
    fn status_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.draft {
            parts.push("Draft".to_string());
        }

        if !self.approved_by.is_empty() {
            let approval_count = self
                .approved_by
                .iter()
                .filter(|approval| !approval.user.username.is_empty())
                .count();
            let label = if approval_count == 1 {
                "1 approval".to_string()
            } else {
                format!("{approval_count} approvals")
            };
            parts.push(label);
        }

        if let Some(head_pipeline) = &self.head_pipeline
            && let Some(status) = head_pipeline.status.as_deref()
        {
            parts.push(format!("Pipeline {}", normalize_provider_label(status)));
        }

        if let Some(merge_status) = self
            .detailed_merge_status
            .as_deref()
            .or(self.merge_status.as_deref())
        {
            parts.push(normalize_provider_label(merge_status));
        }

        status_summary_parts(&parts)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mockall::Sequence;

    use super::*;
    use crate::infra::forge::MockForgeCommandRunner;

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
            .returning(|_| Box::pin(async { Ok(success_output(r#"[{"iid":17}]"#.to_string())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "17")
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
                display_id: "!17".to_string(),
                forge_kind: ForgeKind::GitLab,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("1 approval, Pipeline Success, Mergeable".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://gitlab.example.com/team/project/-/merge_requests/17".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn create_review_request_builds_api_create_command_for_self_hosted_gitlab() {
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
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &lookup_command(&remote, "feature/forge")
            })
            .returning(|_| Box::pin(async { Ok(success_output(r#"[{"iid":17}]"#.to_string())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "17")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .create_review_request(remote, input)
            .await
            .expect("GitLab create should succeed");

        // Assert
        assert_eq!(review_request.display_id, "!17");
        assert_eq!(
            review_request.status_summary.as_deref(),
            Some("1 approval, Pipeline Success, Mergeable")
        );
    }

    #[tokio::test]
    async fn find_by_source_branch_accepts_non_zero_auth_status_when_glab_reports_logged_in() {
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
            .returning(|_| {
                Box::pin(async {
                    Ok(ForgeCommandOutput {
                        exit_code: Some(1),
                        stderr: String::new(),
                        stdout: "gitlab.example.com\n  ✓ Logged in to gitlab.example.com as \
                                 minev.dev\n  ✓ Token found: **************************"
                            .to_string(),
                    })
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
            .returning(|_| Box::pin(async { Ok(success_output(r#"[{"iid":17}]"#.to_string())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "17")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .find_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("logged-in auth status output should be accepted");

        // Assert
        assert_eq!(
            review_request.map(|summary| summary.display_id),
            Some("!17".to_string())
        );
    }

    #[test]
    fn create_command_omits_description_field_when_body_is_missing() {
        // Arrange
        let remote = gitlab_remote();
        let input = CreateReviewRequestInput {
            body: None,
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let command = create_command(&remote, &input);

        // Assert
        assert_eq!(command.executable, "glab");
        assert!(command.arguments.starts_with(&[
            "mr".to_string(),
            "create".to_string(),
            "--draft".to_string(),
            "--repo".to_string(),
            "https://gitlab.example.com/team/project".to_string(),
        ]));
        let description_index = command
            .arguments
            .iter()
            .position(|argument| argument == "--description")
            .expect("description flag should be present for non-interactive creation");
        assert_eq!(
            command.arguments.get(description_index + 1),
            Some(&String::new())
        );
    }

    #[test]
    fn create_command_marks_merge_requests_as_draft_by_default() {
        // Arrange
        let remote = gitlab_remote();
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let command = create_command(&remote, &input);

        // Assert
        assert_eq!(command.executable, "glab");
        assert!(
            command
                .arguments
                .iter()
                .any(|argument| argument == "--draft")
        );
        assert!(command.arguments.iter().any(|argument| argument == "--yes"));
    }

    #[test]
    fn create_command_passes_description_when_body_is_present() {
        // Arrange
        let remote = gitlab_remote();
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let command = create_command(&remote, &input);

        // Assert
        let description_index = command
            .arguments
            .iter()
            .position(|argument| argument == "--description")
            .expect("description flag should be present when body is provided");
        assert_eq!(
            command.arguments.get(description_index + 1),
            Some(&"Implements the provider adapters.".to_string())
        );
    }

    #[tokio::test]
    async fn refresh_review_request_maps_host_resolution_error() {
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

                move |command| command == &view_command(&remote, "17")
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(failure_output(
                        "Get \"https://gitlab.example.com/api/v4/projects/team%2Fproject\": dial \
                         tcp: lookup gitlab.example.com: no such host"
                            .to_string(),
                    ))
                })
            });
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let error = adapter
            .refresh_review_request(remote, "!17".to_string())
            .await
            .expect_err("DNS failures should be normalized");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitLab,
                host: "gitlab.example.com".to_string(),
            }
        );
    }

    #[test]
    fn parse_view_response_accepts_missing_approved_by_field() {
        // Arrange
        let stdout = r#"{
            "iid": 17,
            "title": "Add forge review support",
            "state": "opened",
            "web_url": "https://gitlab.example.com/team/project/-/merge_requests/17",
            "source_branch": "feature/forge",
            "target_branch": "main",
            "draft": false,
            "merge_status": "mergeable",
            "detailed_merge_status": "mergeable",
            "head_pipeline": {
                "status": "success"
            }
        }"#;

        // Act
        let review_request =
            parse_view_response(stdout).expect("missing approvals should default to empty");

        // Assert
        assert_eq!(review_request.display_id, "!17");
        assert_eq!(
            review_request.status_summary.as_deref(),
            Some("Pipeline Success, Mergeable")
        );
    }

    #[test]
    fn glab_commands_use_environment_host_targeting_without_legacy_no_prompt() {
        // Arrange
        let remote = gitlab_remote();

        // Act
        let command = lookup_command(&remote, "feature/forge");

        // Assert
        assert!(command.environment.contains(&(
            "GITLAB_HOST".to_string(),
            "https://gitlab.example.com".to_string()
        )));
        assert!(
            command
                .environment
                .contains(&("GLAB_NO_PROMPT".to_string(), "1".to_string()))
        );
        assert!(command.environment.contains(&(
            "GL_HOST".to_string(),
            "https://gitlab.example.com".to_string()
        )));
        assert!(
            !command
                .environment
                .iter()
                .any(|(key, _value)| key == "NO_PROMPT")
        );
        assert!(
            !command
                .arguments
                .iter()
                .any(|argument| argument == "--hostname")
        );
    }

    #[test]
    fn glab_commands_keep_explicit_https_ports_in_environment_host_targeting() {
        // Arrange
        let mut remote = gitlab_remote();
        remote.host = "gitlab.example.com:8443".to_string();
        remote.repo_url = "https://gitlab.example.com:8443/team/project.git".to_string();
        remote.web_url = "https://gitlab.example.com:8443/team/project".to_string();

        // Act
        let command = create_command(
            &remote,
            &CreateReviewRequestInput {
                body: None,
                source_branch: "feature/forge".to_string(),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
            },
        );

        // Assert
        assert!(command.environment.contains(&(
            "GITLAB_HOST".to_string(),
            "https://gitlab.example.com:8443".to_string(),
        )));
        assert!(
            !command
                .arguments
                .iter()
                .any(|argument| argument == "--hostname")
        );
    }

    fn gitlab_remote() -> ForgeRemote {
        ForgeRemote {
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.example.com".to_string(),
            namespace: "team".to_string(),
            project: "project".to_string(),
            repo_url: "https://gitlab.example.com/team/project.git".to_string(),
            web_url: "https://gitlab.example.com/team/project".to_string(),
        }
    }

    fn gitlab_view_json() -> String {
        r#"{
            "iid": 17,
            "title": "Add forge review support",
            "state": "opened",
            "web_url": "https://gitlab.example.com/team/project/-/merge_requests/17",
            "source_branch": "feature/forge",
            "target_branch": "main",
            "draft": false,
            "merge_status": "mergeable",
            "detailed_merge_status": "mergeable",
            "approved_by": [
                {
                    "user": {
                        "username": "reviewer"
                    }
                }
            ],
            "head_pipeline": {
                "status": "success"
            }
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
