//! Shared forge review-request types.

use std::future::Future;
use std::pin::Pin;

/// Shared forge family enum reused by persistence and forge adapters.
pub use crate::domain::session::ForgeKind;
/// Shared review-request remote state reused by persistence and forge adapters.
pub use crate::domain::session::ReviewRequestState;
/// Shared normalized review-request summary reused by persistence and adapters.
pub use crate::domain::session::ReviewRequestSummary;

/// Boxed async result used by review-request trait methods.
pub type ForgeFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

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
