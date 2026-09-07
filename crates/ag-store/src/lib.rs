//! Reusable repository contracts and `SQLite` persistence for Agentty sessions.

mod activity;
mod connection;
mod error;
mod operation;
mod orchestration;
mod project;
mod repository;
mod review;
mod session;
mod session_message;
mod session_preparation;
mod session_snapshot;
mod setting;
mod status;
mod timestamp;
mod usage;

pub use activity::ActivityRepository;
pub(crate) use activity::SqliteActivityRepository;
pub use connection::Database;
pub use error::DbError;
pub(crate) use error::DbResultExt;
pub(crate) use operation::SqliteOperationRepository;
pub use operation::{OperationRepository, SessionOperationRow};
#[cfg(any(test, feature = "test-utils"))]
pub use orchestration::MockOrchestrationRepository;
pub(crate) use orchestration::SqliteOrchestrationRepository;
pub use orchestration::{
    OrchestrationRepository, PersistedOrchestrationTask, SessionOrchestrationMetadataRow,
    SessionOrchestrationRow, SessionOrchestrationTaskRow,
};
pub(crate) use project::SqliteProjectRepository;
pub use project::{ProjectListRow, ProjectRepository, ProjectRow};
pub use repository::AppRepositories;
pub(crate) use review::SqliteReviewRepository;
pub use review::{
    NewSessionReviewCommentResolution, ReviewRepository, SessionReviewCommentResolutionRow,
    SessionReviewRequestRow,
};
pub(crate) use session::SqliteSessionRepository;
pub use session::{
    ForkSessionSnapshot, PersistedSessionCreation, SessionAgentModelRow, SessionDetailRow,
    SessionFocusedReviewRow, SessionListRow, SessionMessageRow, SessionRepository, SessionRow,
    SessionTurnMetadata,
};
pub use session_preparation::{
    SessionPreparationRepository, SessionPreparationRow, SessionPreparationState,
};
pub use setting::SettingRepository;
pub(crate) use setting::SqliteSettingRepository;
pub use timestamp::TimestampSource;
pub(crate) use usage::SqliteUsageRepository;
pub use usage::{SessionUsageRow, UsageRepository};
