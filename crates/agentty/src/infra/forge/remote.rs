//! Forge remote detection helpers shared across provider adapters.

use super::{
    ForgeKind, ForgeRemote, GitHubReviewRequestAdapter, GitLabReviewRequestAdapter,
    ReviewRequestError,
};

/// Parsed remote components extracted from one git remote URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedRemote {
    pub(crate) host: String,
    pub(crate) namespace: String,
    pub(crate) project: String,
    pub(crate) repo_url: String,
    pub(crate) web_url: String,
}

impl ParsedRemote {
    /// Converts the parsed remote into one supported forge remote.
    pub(crate) fn into_forge_remote(self, forge_kind: ForgeKind) -> ForgeRemote {
        ForgeRemote {
            forge_kind,
            host: self.host,
            namespace: self.namespace,
            project: self.project,
            repo_url: self.repo_url,
            web_url: self.web_url,
        }
    }

    /// Returns whether the hostname clearly identifies a GitLab instance.
    pub(crate) fn host_is_gitlab(&self) -> bool {
        strip_port(&self.host)
            .split('.')
            .any(|segment| segment == "gitlab")
    }
}

/// Detects one supported forge remote from `repo_url`.
pub(crate) fn detect_remote(repo_url: &str) -> Result<ForgeRemote, ReviewRequestError> {
    if let Some(remote) = GitHubReviewRequestAdapter::detect_remote(repo_url) {
        return Ok(remote);
    }

    if let Some(remote) = GitLabReviewRequestAdapter::detect_remote(repo_url) {
        return Ok(remote);
    }

    Err(ReviewRequestError::UnsupportedRemote {
        repo_url: repo_url.to_string(),
    })
}

/// Parses a git remote URL into normalized hostname and repository components.
pub(crate) fn parse_remote_url(repo_url: &str) -> Option<ParsedRemote> {
    let trimmed_url = repo_url.trim().trim_end_matches('/');
    if trimmed_url.is_empty() {
        return None;
    }

    if let Some(ssh_remote) = trimmed_url.strip_prefix("git@") {
        let (host, path) = ssh_remote.split_once(':')?;

        return parsed_remote_from_parts(trimmed_url, host, path);
    }

    let (_, scheme_rest) = trimmed_url.split_once("://")?;
    let scheme_rest = scheme_rest.strip_prefix("git@").unwrap_or(scheme_rest);
    let (host, path) = scheme_rest.split_once('/')?;

    parsed_remote_from_parts(trimmed_url, host, path)
}

/// Removes any `:port` suffix from `host`.
pub(crate) fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

/// Builds one parsed remote from extracted host and path components.
fn parsed_remote_from_parts(repo_url: &str, host: &str, path: &str) -> Option<ParsedRemote> {
    let host = host.trim().trim_matches('/').to_ascii_lowercase();
    let path = path.trim().trim_matches('/').trim_end_matches(".git");
    if host.is_empty() || path.is_empty() {
        return None;
    }

    let (namespace, project) = path.rsplit_once('/')?;
    if namespace.is_empty() || project.is_empty() {
        return None;
    }

    Some(ParsedRemote {
        host: host.clone(),
        namespace: namespace.to_string(),
        project: project.to_string(),
        repo_url: repo_url.to_string(),
        web_url: format!("https://{host}/{path}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_remote_returns_github_remote_for_https_origin() {
        // Arrange
        let repo_url = "https://github.com/agentty-xyz/agentty.git";

        // Act
        let remote = detect_remote(repo_url).expect("github remote should be supported");

        // Assert
        assert_eq!(
            remote,
            ForgeRemote {
                forge_kind: ForgeKind::GitHub,
                host: "github.com".to_string(),
                namespace: "agentty-xyz".to_string(),
                project: "agentty".to_string(),
                repo_url: repo_url.to_string(),
                web_url: "https://github.com/agentty-xyz/agentty".to_string(),
            }
        );
    }

    #[test]
    fn detect_remote_returns_github_remote_for_ssh_origin() {
        // Arrange
        let repo_url = "git@github.com:agentty-xyz/agentty.git";

        // Act
        let remote = detect_remote(repo_url).expect("github ssh remote should be supported");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitHub);
        assert_eq!(remote.web_url, "https://github.com/agentty-xyz/agentty");
        assert_eq!(remote.project_path(), "agentty-xyz/agentty");
    }

    #[test]
    fn detect_remote_returns_gitlab_remote_for_https_origin() {
        // Arrange
        let repo_url = "https://gitlab.com/group/subgroup/project.git";

        // Act
        let remote = detect_remote(repo_url).expect("gitlab remote should be supported");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitLab);
        assert_eq!(remote.host, "gitlab.com");
        assert_eq!(remote.namespace, "group/subgroup");
        assert_eq!(remote.project, "project");
    }

    #[test]
    fn detect_remote_returns_gitlab_remote_for_self_hosted_ssh_origin() {
        // Arrange
        let repo_url = "git@gitlab.example.com:team/project.git";

        // Act
        let remote =
            detect_remote(repo_url).expect("self-hosted gitlab remote should be supported");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitLab);
        assert_eq!(remote.host, "gitlab.example.com");
        assert_eq!(remote.web_url, "https://gitlab.example.com/team/project");
    }

    #[test]
    fn detect_remote_returns_unsupported_remote_error_for_non_forge_origin() {
        // Arrange
        let repo_url = "https://example.com/team/project.git";

        // Act
        let error = detect_remote(repo_url).expect_err("non-forge remote should be rejected");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::UnsupportedRemote {
                repo_url: repo_url.to_string(),
            }
        );
        assert!(error.detail_message().contains("GitHub and GitLab"));
        assert!(error.detail_message().contains("example.com"));
    }
}
