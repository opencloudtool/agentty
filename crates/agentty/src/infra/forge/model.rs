//! Shared forge review-request types.

use std::future::Future;
use std::pin::Pin;

/// Boxed async result used by review-request trait methods.
pub type ForgeFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Supported forge families that Agentty can target for review requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeKind {
    /// GitHub pull requests managed through the `gh` CLI.
    GitHub,
    /// GitLab merge requests managed through the `glab` CLI.
    GitLab,
}

impl ForgeKind {
    /// Returns the user-facing forge name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
        }
    }

    /// Returns the CLI executable name used for this forge.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::GitHub => "gh",
            Self::GitLab => "glab",
        }
    }

    /// Returns the login command users should run to authorize forge access.
    pub fn auth_login_command(self) -> &'static str {
        match self {
            Self::GitHub => "gh auth login",
            Self::GitLab => "glab auth login",
        }
    }
}

/// Normalized repository remote metadata for one supported forge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeRemote {
    /// Forge family inferred from the repository remote.
    pub forge_kind: ForgeKind,
    /// Remote hostname, optionally including a port.
    pub host: String,
    /// Repository namespace or owner path.
    pub namespace: String,
    /// Repository name without a trailing `.git` suffix.
    pub project: String,
    /// Original remote URL returned by git.
    pub repo_url: String,
    /// Browser-openable repository URL derived from the remote.
    pub web_url: String,
}

impl ForgeRemote {
    /// Returns the `<namespace>/<project>` path used by forge CLIs and URLs.
    pub fn project_path(&self) -> String {
        format!("{}/{}", self.namespace, self.project)
    }
}

/// Input required to create a review request on one forge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateReviewRequestInput {
    /// Optional body or description submitted with the review request.
    pub body: Option<String>,
    /// Source branch that should be reviewed.
    pub source_branch: String,
    /// Target branch that receives the review request.
    pub target_branch: String,
    /// Title shown in the forge review-request UI.
    pub title: String,
}

/// Normalized lifecycle state for one forge review request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewRequestState {
    /// The review request is still open.
    Open,
    /// The review request was merged.
    Merged,
    /// The review request was closed without merge.
    Closed,
}

/// Normalized review-request summary used by the app layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRequestSummary {
    /// Provider display id such as GitHub `#123` or GitLab `!42`.
    pub display_id: String,
    /// Forge family that owns the review request.
    pub forge_kind: ForgeKind,
    /// Source branch associated with the review request.
    pub source_branch: String,
    /// Normalized lifecycle state.
    pub state: ReviewRequestState,
    /// Compact provider-specific status text shown in the UI.
    pub status_summary: Option<String>,
    /// Target branch associated with the review request.
    pub target_branch: String,
    /// Review request title.
    pub title: String,
    /// Browser-openable review request URL.
    pub web_url: String,
}

/// Review-request failures normalized for actionable UI messaging.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewRequestError {
    /// The required forge CLI is not available on the user's machine.
    CliNotInstalled { forge_kind: ForgeKind },
    /// The forge CLI is installed but not authorized for the target host.
    AuthenticationRequired { forge_kind: ForgeKind, host: String },
    /// The forge host from the repository remote could not be resolved.
    HostResolutionFailed { forge_kind: ForgeKind, host: String },
    /// The repository remote does not map to a supported forge.
    UnsupportedRemote { repo_url: String },
    /// A forge CLI command ran but failed.
    OperationFailed {
        forge_kind: ForgeKind,
        message: String,
    },
}

impl ReviewRequestError {
    /// Returns actionable user-facing copy for the failure.
    pub fn detail_message(&self) -> String {
        match self {
            Self::CliNotInstalled { forge_kind } => format!(
                "{} review requests require the `{}` CLI.\nInstall `{}` and run `{}`, then retry.",
                forge_kind.display_name(),
                forge_kind.cli_name(),
                forge_kind.cli_name(),
                forge_kind.auth_login_command(),
            ),
            Self::AuthenticationRequired { forge_kind, host } => format!(
                "{} review requests require local CLI authentication for `{host}`.\nRun `{}` and \
                 retry.",
                forge_kind.display_name(),
                forge_kind.auth_login_command(),
            ),
            Self::HostResolutionFailed { forge_kind, host } => format!(
                "{} review requests could not reach `{host}`.\nCheck the repository remote host \
                 and your network or DNS setup, then retry.",
                forge_kind.display_name(),
            ),
            Self::UnsupportedRemote { repo_url } => format!(
                "Review requests are only supported for GitHub and GitLab remotes.\nThis \
                 repository remote is not supported: `{repo_url}`."
            ),
            Self::OperationFailed {
                forge_kind,
                message,
            } => format!(
                "{} review-request operation failed: {message}",
                forge_kind.display_name()
            ),
        }
    }
}
