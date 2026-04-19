//! Infrastructure adapters for database, git, and agent backends.

pub mod agent;
/// Shared app-server types and runtime trait boundaries.
pub mod app_server;
/// Provider router for app-server clients.
pub mod app_server_router;
/// Shared stdio JSON-RPC transport for app-server protocols.
pub mod app_server_transport;
/// Provider-agnostic agent channel abstraction for session turn execution.
pub mod channel;
pub mod db;
/// Gitignore-aware file indexing and fuzzy path filtering.
pub mod file_index;
/// Filesystem trait boundary used by app orchestration.
pub mod fs;
pub mod git;
/// Process-management utilities for agent subprocess lifecycle.
pub(crate) mod process;
/// Startup project-discovery boundary for home-directory repository scans.
pub mod project_discovery;
/// Tmux process boundary used by app orchestration.
pub mod tmux;
pub mod version;
