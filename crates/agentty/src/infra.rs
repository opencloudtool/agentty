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
pub mod codex_app_server;
pub mod db;
/// Gitignore-aware file indexing and fuzzy path filtering.
pub mod file_index;
/// Gemini ACP-backed app-server client implementation.
pub mod gemini_acp;
pub mod git;
pub mod version;
