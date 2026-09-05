//! Infrastructure adapters for database, filesystem, and system boundaries.
/// Clipboard image capture and persistence boundary for prompt attachments.
pub(crate) mod clipboard_image;
/// Wall-clock boundary used by app, runtime, and session orchestration.
pub mod clock;
pub mod db;
/// Gitignore-aware file indexing and fuzzy path filtering.
pub mod file_index;
/// Filesystem trait boundary used by app orchestration.
pub mod fs;
/// Agentty data-directory path resolution.
pub mod home;
/// Workspace-only personality discovery boundary.
pub mod personality;
/// Process-management utilities for agent subprocess lifecycle.
pub(crate) mod process;
/// Native process creation identities for resource accounting.
pub(crate) mod process_identity;
/// Startup project-discovery boundary for home-directory repository scans.
pub mod project_discovery;
/// Host process-accounting boundary.
pub(crate) mod resource;
/// Tmux process boundary used by app orchestration.
pub mod tmux;
pub mod version;
