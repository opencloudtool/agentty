//! Database layer for persisting session metadata using `SQLite` via `SQLx`.

use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

mod activity;
mod operation;
mod project;
mod review;
mod session;
mod setting;
mod usage;

pub(crate) use activity::{ActivityRepository, SqliteActivityRepository};
pub use operation::SessionOperationRow;
pub(crate) use operation::{OperationRepository, SqliteOperationRepository};
pub use project::{ProjectListRow, ProjectRow};
pub(crate) use project::{ProjectRepository, SqliteProjectRepository};
pub use review::SessionReviewRequestRow;
pub(crate) use review::{ReviewRepository, SqliteReviewRepository};
#[cfg(test)]
pub(crate) use session::SessionJoinRow;
pub use session::SessionRow;
pub(crate) use session::{SessionRepository, SessionTurnMetadata, SqliteSessionRepository};
pub(crate) use setting::{SettingRepository, SqliteSettingRepository};
pub use usage::SessionUsageRow;
pub(crate) use usage::{SqliteUsageRepository, UsageRepository};

use crate::domain::agent::ReasoningLevel;
use crate::domain::session::{DailyActivity, ReviewRequest, SessionStats};

/// Typed error returned by database operations.
///
/// Wraps the underlying `SQLx`, migration, and I/O failures so callers can
/// distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A SQL query or connection-pool operation failed.
    #[error("{0}")]
    Query(#[from] sqlx::Error),

    /// An embedded schema migration failed during database open.
    #[error("{0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// A filesystem operation failed (for example creating the database
    /// directory).
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Subdirectory under the agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default database filename.
pub const DB_FILE: &str = "agentty.db";

/// Maximum number of pooled `SQLite` connections for the on-disk database.
///
/// `SQLite` still serializes writes in WAL mode, so the pool stays small and
/// biased toward a handful of concurrent readers instead of a large number of
/// queued writer contenders.
pub const DB_POOL_MAX_CONNECTIONS: u32 = 4;

/// Thin wrapper around a `SQLite` connection pool.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    repositories: AppRepositories,
}

/// App-layer repository bundle used for selective mock injection.
#[derive(Clone)]
pub struct AppRepositories {
    activity: Arc<dyn ActivityRepository>,
    operation: Arc<dyn OperationRepository>,
    project: Arc<dyn ProjectRepository>,
    review: Arc<dyn ReviewRepository>,
    session: Arc<dyn SessionRepository>,
    setting: Arc<dyn SettingRepository>,
    usage: Arc<dyn UsageRepository>,
}

impl AppRepositories {
    /// Creates a repository bundle from explicit repository implementations.
    pub(crate) fn new(
        activity: Arc<dyn ActivityRepository>,
        operation: Arc<dyn OperationRepository>,
        project: Arc<dyn ProjectRepository>,
        review: Arc<dyn ReviewRepository>,
        session: Arc<dyn SessionRepository>,
        setting: Arc<dyn SettingRepository>,
        usage: Arc<dyn UsageRepository>,
    ) -> Self {
        Self {
            activity,
            operation,
            project,
            review,
            session,
            setting,
            usage,
        }
    }

    /// Creates a repository bundle backed by one shared `SQLite` pool.
    pub(crate) fn from_database(database: &Database) -> Self {
        Self::from_pool(database.pool().clone())
    }

    /// Creates a repository bundle backed by one shared `SQLite` pool.
    pub(crate) fn from_pool(pool: SqlitePool) -> Self {
        Self::new(
            Arc::new(SqliteActivityRepository::new(pool.clone())),
            Arc::new(SqliteOperationRepository::new(pool.clone())),
            Arc::new(SqliteProjectRepository::new(pool.clone())),
            Arc::new(SqliteReviewRepository::new(pool.clone())),
            Arc::new(SqliteSessionRepository::new(pool.clone())),
            Arc::new(SqliteSettingRepository::new(pool.clone())),
            Arc::new(SqliteUsageRepository::new(pool)),
        )
    }

    /// Loads aggregated session-creation activity counts keyed by local day.
    pub(crate) async fn load_session_activity(&self) -> Result<Vec<DailyActivity>, DbError> {
        self.activity.load_session_activity().await
    }

    #[cfg(test)]
    /// Loads persisted activity event timestamps used for activity stats.
    pub(crate) async fn load_session_activity_timestamps(&self) -> Result<Vec<i64>, DbError> {
        self.activity.load_session_activity_timestamps().await
    }

    #[cfg(test)]
    /// Rebuilds `session_activity` rows from current `session.created_at`.
    pub(crate) async fn backfill_session_activity_from_sessions(&self) -> Result<(), DbError> {
        self.activity
            .backfill_session_activity_from_sessions()
            .await
    }

    #[cfg(test)]
    /// Deletes all rows from `session_activity`.
    pub(crate) async fn clear_session_activity(&self) -> Result<(), DbError> {
        self.activity.clear_session_activity().await
    }

    #[cfg(test)]
    /// Persists one session-creation activity event at a specific Unix
    /// timestamp.
    pub(crate) async fn insert_session_creation_activity_at(
        &self,
        session_id: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError> {
        self.activity
            .insert_session_creation_activity_at(session_id, timestamp_seconds)
            .await
    }

    /// Persists one session-creation activity event at the current Unix
    /// timestamp.
    pub(crate) async fn insert_session_creation_activity_now(
        &self,
        session_id: &str,
    ) -> Result<(), DbError> {
        self.activity
            .insert_session_creation_activity_now(session_id)
            .await
    }

    /// Loads operations still waiting in queue or currently running.
    pub(crate) async fn load_unfinished_session_operations(
        &self,
    ) -> Result<Vec<SessionOperationRow>, DbError> {
        self.operation.load_unfinished_session_operations().await
    }

    /// Marks an operation as canceled.
    pub(crate) async fn mark_session_operation_canceled(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        self.operation
            .mark_session_operation_canceled(operation_id, reason)
            .await
    }

    /// Marks an operation as completed successfully.
    pub(crate) async fn mark_session_operation_done(
        &self,
        operation_id: &str,
    ) -> Result<(), DbError> {
        self.operation
            .mark_session_operation_done(operation_id)
            .await
    }

    /// Marks an operation as failed with an error message.
    pub(crate) async fn mark_session_operation_failed(
        &self,
        operation_id: &str,
        error: &str,
    ) -> Result<(), DbError> {
        self.operation
            .mark_session_operation_failed(operation_id, error)
            .await
    }

    /// Marks an operation as running and refreshes its heartbeat timestamp.
    pub(crate) async fn mark_session_operation_running(
        &self,
        operation_id: &str,
    ) -> Result<(), DbError> {
        self.operation
            .mark_session_operation_running(operation_id)
            .await
    }

    /// Inserts a queued operation row for a session.
    pub(crate) async fn insert_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<(), DbError> {
        self.operation
            .insert_session_operation(operation_id, session_id, kind)
            .await
    }

    /// Requests cancellation for unfinished operations of a session.
    pub(crate) async fn request_cancel_for_session_operations(
        &self,
        session_id: &str,
    ) -> Result<(), DbError> {
        self.operation
            .request_cancel_for_session_operations(session_id)
            .await
    }

    /// Returns whether cancellation is requested for a specific operation.
    pub(crate) async fn is_cancel_requested_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<bool, DbError> {
        self.operation
            .is_cancel_requested_for_operation(operation_id)
            .await
    }

    /// Returns whether an operation is still unfinished.
    pub(crate) async fn is_session_operation_unfinished(
        &self,
        operation_id: &str,
    ) -> Result<bool, DbError> {
        self.operation
            .is_session_operation_unfinished(operation_id)
            .await
    }

    /// Marks unfinished operations as failed after process restart.
    pub(crate) async fn fail_unfinished_session_operations(
        &self,
        reason: &str,
    ) -> Result<(), DbError> {
        self.operation
            .fail_unfinished_session_operations(reason)
            .await
    }

    /// Looks up a project by identifier.
    pub(crate) async fn get_project(&self, id: i64) -> Result<Option<ProjectRow>, DbError> {
        self.project.get_project(id).await
    }

    /// Loads all configured projects with aggregated session stats.
    pub(crate) async fn load_projects_with_stats(&self) -> Result<Vec<ProjectListRow>, DbError> {
        self.project.load_projects_with_stats().await
    }

    #[cfg(test)]
    /// Updates favorite state for one project.
    pub(crate) async fn set_project_favorite(
        &self,
        project_id: i64,
        is_favorite: bool,
    ) -> Result<(), DbError> {
        self.project
            .set_project_favorite(project_id, is_favorite)
            .await
    }

    /// Marks a project as recently opened at the current Unix timestamp.
    ///
    /// # Errors
    /// Returns an error if the project row cannot be updated.
    pub async fn touch_project_last_opened(&self, project_id: i64) -> Result<(), DbError> {
        self.project.touch_project_last_opened(project_id).await
    }

    /// Inserts or updates a project by path and returns its identifier.
    ///
    /// # Errors
    /// Returns an error if the project row cannot be written or read.
    pub async fn upsert_project(
        &self,
        path: &str,
        git_branch: Option<&str>,
    ) -> Result<i64, DbError> {
        self.project
            .upsert_project(path, git_branch.map(str::to_string))
            .await
    }

    /// Updates the persisted forge review-request linkage for a session.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be updated.
    pub async fn update_session_review_request(
        &self,
        id: &str,
        review_request: Option<&ReviewRequest>,
    ) -> Result<(), DbError> {
        self.review
            .update_session_review_request(id, review_request.cloned())
            .await
    }

    /// Appends text to the saved output for a session row.
    pub(crate) async fn append_session_output(&self, id: &str, chunk: &str) -> Result<(), DbError> {
        self.session.append_session_output(id, chunk).await
    }

    /// Sets `project_id` for sessions that do not yet reference a project.
    pub(crate) async fn backfill_session_project(&self, project_id: i64) -> Result<(), DbError> {
        self.session.backfill_session_project(project_id).await
    }

    /// Deletes a session row by identifier.
    pub(crate) async fn delete_session(&self, id: &str) -> Result<(), DbError> {
        self.session.delete_session(id).await
    }

    /// Returns the persisted base branch for a session, when present.
    pub(crate) async fn get_session_base_branch(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        self.session.get_session_base_branch(id).await
    }

    /// Returns the persisted app-server instruction bootstrap marker for a
    /// session, when present.
    pub(crate) async fn get_session_instruction_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        self.session
            .get_session_instruction_conversation_id(id)
            .await
    }

    /// Returns the provider conversation identifier for a session, when
    /// present.
    pub(crate) async fn get_session_provider_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        self.session.get_session_provider_conversation_id(id).await
    }

    /// Inserts a newly created draft-session row.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be inserted.
    pub async fn insert_draft_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        self.session
            .insert_draft_session(id, model, base_branch, status, project_id)
            .await
    }

    /// Inserts a newly created session row.
    ///
    /// # Errors
    /// Returns an error if the session row cannot be inserted.
    pub async fn insert_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        self.session
            .insert_session(id, model, base_branch, status, project_id)
            .await
    }

    #[cfg(test)]
    /// Loads all sessions ordered by most recent update.
    pub(crate) async fn load_sessions(&self) -> Result<Vec<SessionRow>, DbError> {
        self.session.load_sessions().await
    }

    /// Loads all sessions ordered by most recent update for one project.
    pub(crate) async fn load_sessions_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionRow>, DbError> {
        self.session.load_sessions_for_project(project_id).await
    }

    /// Loads lightweight session metadata used for cheap change detection.
    pub(crate) async fn load_sessions_metadata(&self) -> Result<(i64, i64), DbError> {
        self.session.load_sessions_metadata().await
    }

    /// Loads the project identifier associated with one session.
    pub(crate) async fn load_session_project_id(
        &self,
        session_id: &str,
    ) -> Result<Option<i64>, DbError> {
        self.session.load_session_project_id(session_id).await
    }

    /// Returns the persisted upstream reference for a published session
    /// branch, when present.
    pub(crate) async fn load_session_published_upstream_ref(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        self.session.load_session_published_upstream_ref(id).await
    }

    /// Loads the persisted session-specific reasoning override, when present.
    pub(crate) async fn load_session_reasoning_level_override(
        &self,
        session_id: &str,
    ) -> Result<Option<ReasoningLevel>, DbError> {
        self.session
            .load_session_reasoning_level_override(session_id)
            .await
    }

    /// Loads the persisted summary text associated with one session.
    pub(crate) async fn load_session_summary(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError> {
        self.session.load_session_summary(session_id).await
    }

    /// Returns `(created_at, updated_at)` timestamps for a session.
    pub(crate) async fn load_session_timestamps(
        &self,
        session_id: &str,
    ) -> Result<Option<(i64, i64)>, DbError> {
        self.session.load_session_timestamps(session_id).await
    }

    /// Persists all canonical turn metadata for one completed agent turn in a
    /// single transaction.
    pub(crate) async fn persist_session_turn_metadata(
        &self,
        session_id: &str,
        turn_metadata: &SessionTurnMetadata<'_>,
    ) -> Result<(), DbError> {
        self.session
            .persist_session_turn_metadata(session_id, turn_metadata)
            .await
    }

    #[cfg(test)]
    /// Replaces the full output for a session row.
    pub(crate) async fn replace_session_output(
        &self,
        id: &str,
        output: &str,
    ) -> Result<(), DbError> {
        self.session.replace_session_output(id, output).await
    }

    /// Updates persisted diff-derived size and line-count fields for a
    /// session row.
    ///
    /// # Errors
    /// Returns an error if the diff-stats update fails.
    pub async fn update_session_diff_stats(
        &self,
        added_lines: u64,
        deleted_lines: u64,
        id: &str,
        size: &str,
    ) -> Result<(), DbError> {
        self.session
            .update_session_diff_stats(added_lines, deleted_lines, id, size)
            .await
    }

    /// Updates the persisted app-server instruction bootstrap marker for a
    /// session.
    pub(crate) async fn update_session_instruction_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<&str>,
    ) -> Result<(), DbError> {
        self.session
            .update_session_instruction_conversation_id(
                id,
                provider_conversation_id.map(str::to_string),
            )
            .await
    }

    /// Updates the persisted model for a session.
    pub(crate) async fn update_session_model(&self, id: &str, model: &str) -> Result<(), DbError> {
        self.session.update_session_model(id, model).await
    }

    /// Updates the saved prompt for a session row.
    pub(crate) async fn update_session_prompt(
        &self,
        id: &str,
        prompt: &str,
    ) -> Result<(), DbError> {
        self.session.update_session_prompt(id, prompt).await
    }

    /// Updates the persisted provider conversation identifier for a session.
    pub(crate) async fn update_session_provider_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<&str>,
    ) -> Result<(), DbError> {
        self.session
            .update_session_provider_conversation_id(
                id,
                provider_conversation_id.map(str::to_string),
            )
            .await
    }

    /// Updates the model clarification questions for a session row.
    pub(crate) async fn update_session_questions(
        &self,
        id: &str,
        questions: &str,
    ) -> Result<(), DbError> {
        self.session.update_session_questions(id, questions).await
    }

    /// Updates the persisted session-specific reasoning override.
    pub(crate) async fn update_session_reasoning_level(
        &self,
        id: &str,
        reasoning_level: Option<&str>,
    ) -> Result<(), DbError> {
        self.session
            .update_session_reasoning_level(id, reasoning_level.map(str::to_string))
            .await
    }

    /// Updates the persisted upstream reference for a published session
    /// branch.
    pub(crate) async fn update_session_published_upstream_ref(
        &self,
        id: &str,
        published_upstream_ref: Option<&str>,
    ) -> Result<(), DbError> {
        self.session
            .update_session_published_upstream_ref(id, published_upstream_ref.map(str::to_string))
            .await
    }

    /// Accumulates token statistics for a session.
    pub(crate) async fn update_session_stats(
        &self,
        id: &str,
        stats: &SessionStats,
    ) -> Result<(), DbError> {
        self.session.update_session_stats(id, stats).await
    }

    /// Updates the status for a session row and opens or closes the persisted
    /// cumulative active-work interval when crossing the `InProgress`
    /// boundary.
    pub(crate) async fn update_session_status_with_timing_at(
        &self,
        id: &str,
        status: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError> {
        self.session
            .update_session_status_with_timing_at(id, status, timestamp_seconds)
            .await
    }

    /// Updates the persisted session summary text for a session row.
    pub(crate) async fn update_session_summary(
        &self,
        id: &str,
        summary: &str,
    ) -> Result<(), DbError> {
        self.session.update_session_summary(id, summary).await
    }

    /// Updates the display title for a session row.
    ///
    /// # Errors
    /// Returns an error if the title update fails.
    pub async fn update_session_title(&self, id: &str, title: &str) -> Result<(), DbError> {
        self.session.update_session_title(id, title).await
    }

    /// Updates the display title for a session row only when the persisted
    /// prompt still matches the prompt snapshot used to generate that title.
    pub(crate) async fn update_session_title_for_prompt(
        &self,
        id: &str,
        expected_prompt: &str,
        title: &str,
    ) -> Result<bool, DbError> {
        self.session
            .update_session_title_for_prompt(id, expected_prompt, title)
            .await
    }

    #[cfg(test)]
    /// Overrides the `created_at` timestamp for one session row.
    pub(crate) async fn update_session_created_at(
        &self,
        id: &str,
        created_at: i64,
    ) -> Result<(), DbError> {
        self.session.update_session_created_at(id, created_at).await
    }

    #[cfg(test)]
    /// Overrides the `updated_at` timestamp for one session row.
    pub(crate) async fn update_session_updated_at(
        &self,
        id: &str,
        updated_at: i64,
    ) -> Result<(), DbError> {
        self.session.update_session_updated_at(id, updated_at).await
    }

    /// Looks up one project-scoped setting value by project and name.
    pub(crate) async fn get_project_setting(
        &self,
        project_id: i64,
        name: crate::domain::setting::SettingName,
    ) -> Result<Option<String>, DbError> {
        self.setting.get_project_setting(project_id, name).await
    }

    /// Loads the active project identifier from application settings.
    pub(crate) async fn load_active_project_id(&self) -> Result<Option<i64>, DbError> {
        self.setting.load_active_project_id().await
    }

    /// Loads one project-scoped reasoning-effort setting.
    pub(crate) async fn load_project_reasoning_level(
        &self,
        project_id: i64,
    ) -> Result<ReasoningLevel, DbError> {
        self.setting.load_project_reasoning_level(project_id).await
    }

    #[cfg(test)]
    /// Looks up a setting value by name.
    pub(crate) async fn get_setting(
        &self,
        name: crate::domain::setting::SettingName,
    ) -> Result<Option<String>, DbError> {
        self.setting.get_setting(name).await
    }

    #[cfg(test)]
    /// Loads the persisted reasoning-effort setting.
    pub(crate) async fn load_reasoning_level(&self) -> Result<ReasoningLevel, DbError> {
        self.setting.load_reasoning_level().await
    }

    /// Persists the active project identifier in application settings.
    pub(crate) async fn set_active_project_id(&self, project_id: i64) -> Result<(), DbError> {
        self.setting.set_active_project_id(project_id).await
    }

    /// Persists one project-scoped reasoning-effort setting.
    pub(crate) async fn set_project_reasoning_level(
        &self,
        project_id: i64,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError> {
        self.setting
            .set_project_reasoning_level(project_id, reasoning_level)
            .await
    }

    #[cfg(test)]
    /// Persists the global reasoning-effort setting.
    pub(crate) async fn set_reasoning_level(
        &self,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError> {
        self.setting.set_reasoning_level(reasoning_level).await
    }

    /// Inserts or updates one project-scoped setting by project and name.
    pub(crate) async fn upsert_project_setting(
        &self,
        project_id: i64,
        name: crate::domain::setting::SettingName,
        value: &str,
    ) -> Result<(), DbError> {
        self.setting
            .upsert_project_setting(project_id, name, value)
            .await
    }

    #[cfg(test)]
    /// Inserts or updates a setting by name.
    pub(crate) async fn upsert_setting(
        &self,
        name: crate::domain::setting::SettingName,
        value: &str,
    ) -> Result<(), DbError> {
        self.setting.upsert_setting(name, value).await
    }

    /// Loads per-model token usage rows for a session, ordered by model name.
    pub(crate) async fn load_session_usage(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionUsageRow>, DbError> {
        self.usage.load_session_usage(session_id).await
    }

    /// Accumulates per-model token usage for a session.
    pub(crate) async fn upsert_session_usage(
        &self,
        session_id: &str,
        model: &str,
        stats: &SessionStats,
    ) -> Result<(), DbError> {
        self.usage
            .upsert_session_usage(session_id, model, stats)
            .await
    }
}

impl Database {
    /// Opens the `SQLite` database and runs embedded migrations.
    ///
    /// Uses up to `DB_POOL_MAX_CONNECTIONS` pooled connections so UI reads can
    /// stay responsive without oversizing the `SQLite` pool beyond what WAL
    /// can use effectively.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        let repositories = AppRepositories::from_pool(pool.clone());

        Ok(Self { pool, repositories })
    }

    /// Opens an in-memory `SQLite` database and runs migrations.
    ///
    /// This is primarily used by tests and any ephemeral workflows that need
    /// an isolated database instance while keeping the same durability and
    /// foreign-key settings as the on-disk database.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        let repositories = AppRepositories::from_pool(pool.clone());

        Ok(Self { pool, repositories })
    }

    /// Returns the shared `SQLite` connection pool for lower-level query
    /// access.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

impl Deref for Database {
    type Target = AppRepositories;

    fn deref(&self) -> &Self::Target {
        &self.repositories
    }
}

impl From<Database> for AppRepositories {
    fn from(database: Database) -> Self {
        database.repositories
    }
}

/// Returns the current Unix timestamp in whole seconds.
pub(crate) fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::session::{ForgeKind, ReviewRequestState, ReviewRequestSummary};
    use crate::domain::setting::SettingName;
    /// Environment flag used to run the DST regression helper in an isolated
    /// subprocess with a fixed timezone.
    const DST_TEST_SUBPROCESS_ENV: &str = "AGENTTY_DST_TEST_SUBPROCESS";

    /// Builds one deterministic persisted review-request fixture for DB tests.
    fn review_request_fixture() -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: 456,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("2 approvals, checks passing".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        }
    }

    /// Asserts that one loaded session row carries the expected review-request
    /// linkage.
    fn assert_review_request_row(row: &SessionRow) {
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.display_id.as_str()),
            Some("#42")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.forge_kind.as_str()),
            Some("GitHub")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.last_refreshed_at),
            Some(456)
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.source_branch.as_str()),
            Some("feature/forge")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.state.as_str()),
            Some("Open")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .and_then(|review_request| review_request.status_summary.as_deref()),
            Some("2 approvals, checks passing")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.target_branch.as_str()),
            Some("main")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.title.as_str()),
            Some("Add forge review support")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.web_url.as_str()),
            Some("https://github.com/agentty-xyz/agentty/pull/42")
        );
    }

    /// Inserts one session row with deterministic defaults for tests.
    async fn insert_session_fixture(
        database: &Database,
        session_id: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) {
        database
            .insert_session(session_id, "gpt-5.4", base_branch, status, project_id)
            .await
            .expect("failed to insert session fixture");
    }

    /// Loads one session row by identifier through `load_sessions()`.
    async fn load_session_row(database: &Database, session_id: &str) -> SessionRow {
        database
            .load_sessions()
            .await
            .expect("failed to load all sessions")
            .into_iter()
            .find(|row| row.id == session_id)
            .expect("missing session row")
    }

    /// Loads one persisted session-operation row regardless of lifecycle
    /// status.
    async fn load_session_operation_row(
        database: &Database,
        operation_id: &str,
    ) -> SessionOperationRow {
        sqlx::query_as!(
            SessionOperationRow,
            r#"
SELECT id AS "id!", session_id AS "session_id!", kind AS "kind!", status AS "status!",
       queued_at, started_at, finished_at,
       heartbeat_at, last_error, cancel_requested AS "cancel_requested: _"
FROM session_operation
WHERE id = ?
"#,
            operation_id
        )
        .fetch_one(database.pool())
        .await
        .expect("failed to load session operation row")
    }

    /// Typed helper row used to verify nullable session references.
    struct SessionUsageSessionIdRow {
        session_id: Option<String>,
    }

    /// Builds one deterministic joined-session row fixture for conversion
    /// tests.
    fn session_join_row_fixture() -> SessionJoinRow {
        SessionJoinRow::fixture_for_test()
    }

    /// Verifies `open()` creates missing parent directories before opening the
    /// on-disk database.
    #[tokio::test]
    async fn test_open_creates_missing_parent_directory() {
        // Arrange
        let temp_dir = tempdir().expect("temp dir should be created");
        let db_path = temp_dir.path().join("nested").join("db").join(DB_FILE);

        // Act
        let database = Database::open(&db_path)
            .await
            .expect("database should open with missing parent directories");

        // Assert
        assert!(db_path.parent().is_some_and(std::path::Path::is_dir));
        assert!(!database.pool().is_closed());
    }

    /// Verifies `load_sessions()` maps persisted joined session fields.
    #[tokio::test]
    async fn test_load_sessions_maps_joined_session_fields() {
        // Arrange
        let (database, project_id) = database_with_joined_session_fields().await;

        // Act
        let session_row = load_session_row(&database, "session-a").await;

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.base_branch, "main");
        assert_eq!(session_row.created_at, 100);
        assert_eq!(session_row.updated_at, 200);
        assert_eq!(session_row.model, "claude-opus-4.1");
        assert_eq!(session_row.status, "Review");
        assert_eq!(session_row.in_progress_started_at, None);
        assert_eq!(session_row.in_progress_total_seconds, 120);
        assert_eq!(session_row.project_id, Some(project_id));
        assert_eq!(session_row.prompt, "Implement the feature");
        assert_eq!(session_row.output, "First line\nSecond line");
        assert_eq!(session_row.added_lines, 14);
        assert_eq!(session_row.deleted_lines, 6);
        assert_eq!(session_row.input_tokens, 11);
        assert_eq!(session_row.output_tokens, 29);
        assert_eq!(session_row.size, "L");
        assert_eq!(
            session_row.summary.as_deref(),
            Some("Implemented the requested feature")
        );
        assert_eq!(session_row.questions.as_deref(), Some("[\"Need logs?\"]"));
        assert_eq!(session_row.title.as_deref(), Some("Feature work"));
        assert_eq!(
            session_row.published_upstream_ref.as_deref(),
            Some("origin/wt/session-a")
        );
        assert_review_request_row(&session_row);
    }

    /// Builds an in-memory database with one session covering joined fields
    /// returned by `load_sessions()`.
    async fn database_with_joined_session_fields() -> (Database, i64) {
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        let review_request = review_request_fixture();

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        persist_joined_session_metadata(&database, &review_request).await;
        persist_joined_session_output(&database).await;

        (database, project_id)
    }

    /// Persists metadata fields asserted by the joined-session mapping test.
    async fn persist_joined_session_metadata(database: &Database, review_request: &ReviewRequest) {
        database
            .update_session_created_at("session-a", 100)
            .await
            .expect("failed to update session created_at");
        database
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session updated_at");
        database
            .update_session_diff_stats(14, 6, "session-a", "L")
            .await
            .expect("failed to update session diff stats");
        database
            .update_session_questions("session-a", "[\"Need logs?\"]")
            .await
            .expect("failed to update session questions");
        database
            .update_session_prompt("session-a", "Implement the feature")
            .await
            .expect("failed to update session prompt");
        database
            .update_session_title("session-a", "Feature work")
            .await
            .expect("failed to update session title");
        database
            .update_session_summary("session-a", "Implemented the requested feature")
            .await
            .expect("failed to update session summary");
        database
            .update_session_stats(
                "session-a",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to update session stats");
        database
            .update_session_model("session-a", "claude-opus-4.1")
            .await
            .expect("failed to update session model");
        database
            .update_session_published_upstream_ref("session-a", Some("origin/wt/session-a"))
            .await
            .expect("failed to update published upstream ref");
        database
            .update_session_review_request("session-a", Some(review_request))
            .await
            .expect("failed to update review request");
    }

    /// Persists timing and output fields asserted by the joined-session
    /// mapping test.
    async fn persist_joined_session_output(database: &Database) {
        database
            .update_session_status_with_timing_at("session-a", "InProgress", 50)
            .await
            .expect("failed to open in-progress timing window");
        database
            .update_session_status_with_timing_at("session-a", "Review", 170)
            .await
            .expect("failed to close in-progress timing window");
        database
            .replace_session_output("session-a", "First line")
            .await
            .expect("failed to replace session output");
        database
            .append_session_output("session-a", "\nSecond line")
            .await
            .expect("failed to append session output");
        database
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session updated_at");
    }

    /// Verifies generated titles only overwrite the session title when the
    /// staged prompt has not changed since generation started.
    #[tokio::test]
    async fn test_update_session_title_for_prompt_requires_matching_prompt() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "session-a", "main", "New", project_id).await;
        database
            .update_session_prompt("session-a", "First draft")
            .await
            .expect("failed to persist first staged prompt");
        database
            .update_session_title("session-a", "First draft")
            .await
            .expect("failed to persist fallback title");

        // Act
        let stale_update_applied = database
            .update_session_title_for_prompt(
                "session-a",
                "Second draft",
                "Refine draft workflow title",
            )
            .await
            .expect("failed to reject stale title update");
        let matching_update_applied = database
            .update_session_title_for_prompt(
                "session-a",
                "First draft",
                "Refine draft workflow title",
            )
            .await
            .expect("failed to apply matching title update");

        // Assert
        let session_row = load_session_row(&database, "session-a").await;
        assert!(!stale_update_applied);
        assert!(matching_update_applied);
        assert_eq!(
            session_row.title.as_deref(),
            Some("Refine draft workflow title")
        );
    }

    /// Verifies timing-aware status transitions accumulate repeated
    /// `InProgress` intervals.
    #[tokio::test]
    async fn test_update_session_status_with_timing_at_accumulates_repeated_intervals() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "session-a", "main", "New", project_id).await;

        // Act
        database
            .update_session_status_with_timing_at("session-a", "InProgress", 10)
            .await
            .expect("failed to enter in-progress the first time");
        database
            .update_session_status_with_timing_at("session-a", "Review", 70)
            .await
            .expect("failed to leave in-progress the first time");
        database
            .update_session_status_with_timing_at("session-a", "InProgress", 100)
            .await
            .expect("failed to enter in-progress the second time");
        database
            .update_session_status_with_timing_at("session-a", "Question", 190)
            .await
            .expect("failed to leave in-progress the second time");
        let session_row = load_session_row(&database, "session-a").await;

        // Assert
        assert_eq!(session_row.status, "Question");
        assert_eq!(session_row.in_progress_started_at, None);
        assert_eq!(session_row.in_progress_total_seconds, 150);
    }

    /// Verifies `load_sessions_for_project()` filters rows by project id.
    #[tokio::test]
    async fn test_load_sessions_for_project_filters_to_project_rows() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project("/tmp/project-b", Some("develop"))
            .await
            .expect("failed to insert second project");

        insert_session_fixture(&database, "session-a", "main", "Review", first_project_id).await;
        insert_session_fixture(&database, "session-b", "main", "Done", first_project_id).await;
        insert_session_fixture(&database, "session-c", "develop", "Done", second_project_id).await;
        database
            .update_session_updated_at("session-a", 300)
            .await
            .expect("failed to update session-a updated_at");
        database
            .update_session_updated_at("session-b", 200)
            .await
            .expect("failed to update session-b updated_at");
        database
            .update_session_updated_at("session-c", 100)
            .await
            .expect("failed to update session-c updated_at");

        // Act
        let session_rows = database
            .load_sessions_for_project(first_project_id)
            .await
            .expect("failed to load project sessions");

        // Assert
        assert_eq!(session_rows.len(), 2);
        assert_eq!(session_rows[0].id, "session-a");
        assert_eq!(session_rows[1].id, "session-b");
        assert!(
            session_rows
                .iter()
                .all(|row| row.project_id == Some(first_project_id))
        );
    }

    /// Verifies `load_sessions_metadata()` returns session count and max
    /// `updated_at`.
    #[tokio::test]
    async fn test_load_sessions_metadata_returns_count_and_latest_timestamp() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        insert_session_fixture(&database, "session-b", "main", "Done", project_id).await;
        database
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session-a updated_at");
        database
            .update_session_updated_at("session-b", 300)
            .await
            .expect("failed to update session-b updated_at");

        // Act
        let session_metadata = database
            .load_sessions_metadata()
            .await
            .expect("failed to load session metadata");

        // Assert
        assert_eq!(session_metadata, (2, 300));
    }

    /// Verifies `load_session_timestamps()` returns the persisted timestamps.
    #[tokio::test]
    async fn test_load_session_timestamps_returns_created_and_updated_values() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .update_session_created_at("session-a", 111)
            .await
            .expect("failed to update session created_at");
        database
            .update_session_updated_at("session-a", 222)
            .await
            .expect("failed to update session updated_at");

        // Act
        let session_timestamps = database
            .load_session_timestamps("session-a")
            .await
            .expect("failed to load session timestamps");

        // Assert
        assert_eq!(session_timestamps, Some((111, 222)));
    }

    /// Verifies `get_session_base_branch()` returns the persisted branch name.
    #[tokio::test]
    async fn test_get_session_base_branch_returns_persisted_value() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "release", "Done", project_id).await;

        // Act
        let base_branch = database
            .get_session_base_branch("session-a")
            .await
            .expect("failed to load session base branch");

        // Assert
        assert_eq!(base_branch.as_deref(), Some("release"));
    }

    /// Verifies `delete_session()` removes the session row and nulls
    /// `session_usage.session_id`.
    #[tokio::test]
    async fn test_delete_session_removes_row_and_nulls_usage_foreign_key() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to insert usage row");

        // Act
        database
            .delete_session("session-a")
            .await
            .expect("failed to delete session");
        let deleted_session = database
            .load_session_timestamps("session-a")
            .await
            .expect("failed to load deleted session timestamps");
        let retained_usage_row = sqlx::query_as!(
            SessionUsageSessionIdRow,
            r#"
SELECT session_id AS "session_id: _"
FROM session_usage
WHERE model = ?
"#,
            "claude-opus-4.1"
        )
        .fetch_one(database.pool())
        .await
        .expect("failed to load retained usage row");

        // Assert
        assert_eq!(deleted_session, None);
        assert_eq!(retained_usage_row.session_id, None,);
    }

    /// Verifies `load_unfinished_session_operations()` returns only queued and
    /// running rows.
    #[tokio::test]
    async fn test_load_unfinished_session_operations_returns_only_queued_and_running_rows() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-queued", "session-a", "merge")
            .await
            .expect("failed to insert queued operation");
        database
            .insert_session_operation("operation-running", "session-a", "sync")
            .await
            .expect("failed to insert running operation");
        database
            .insert_session_operation("operation-done", "session-a", "review")
            .await
            .expect("failed to insert done operation");
        database
            .mark_session_operation_running("operation-running")
            .await
            .expect("failed to mark running operation");
        database
            .mark_session_operation_running("operation-done")
            .await
            .expect("failed to mark done operation running");
        database
            .mark_session_operation_done("operation-done")
            .await
            .expect("failed to mark done operation");

        // Act
        let unfinished_rows = database
            .load_unfinished_session_operations()
            .await
            .expect("failed to load unfinished operations");

        // Assert
        assert_eq!(unfinished_rows.len(), 2);
        assert_eq!(unfinished_rows[0].id, "operation-queued");
        assert_eq!(unfinished_rows[0].status, "queued");
        assert_eq!(unfinished_rows[1].id, "operation-running");
        assert_eq!(unfinished_rows[1].status, "running");
    }

    /// Verifies `request_cancel_for_session_operations()` marks only
    /// unfinished rows.
    #[tokio::test]
    async fn test_request_cancel_for_session_operations_marks_only_unfinished_rows() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-queued", "session-a", "merge")
            .await
            .expect("failed to insert queued operation");
        database
            .insert_session_operation("operation-done", "session-a", "review")
            .await
            .expect("failed to insert done operation");
        database
            .mark_session_operation_running("operation-done")
            .await
            .expect("failed to mark done operation running");
        database
            .mark_session_operation_done("operation-done")
            .await
            .expect("failed to mark done operation");

        // Act
        database
            .request_cancel_for_session_operations("session-a")
            .await
            .expect("failed to request cancel");
        let queued_row = load_session_operation_row(&database, "operation-queued").await;
        let done_row = load_session_operation_row(&database, "operation-done").await;

        // Assert
        assert!(queued_row.cancel_requested);
        assert!(!done_row.cancel_requested);
    }

    /// Verifies `is_session_operation_unfinished()` returns `false` for a
    /// completed operation.
    #[tokio::test]
    async fn test_is_session_operation_unfinished_returns_false_for_done_operation() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");
        database
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");
        database
            .mark_session_operation_done("operation-a")
            .await
            .expect("failed to mark operation done");

        // Act
        let is_unfinished = database
            .is_session_operation_unfinished("operation-a")
            .await
            .expect("failed to check unfinished operation state");

        // Assert
        assert!(!is_unfinished);
    }

    /// Verifies `is_cancel_requested_for_operation()` returns `true` for a
    /// cancelled operation and `false` for an unaffected one.
    #[tokio::test]
    async fn test_is_cancel_requested_for_operation_scoped_to_single_operation() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-cancelled", "session-a", "reply")
            .await
            .expect("failed to insert cancelled operation");
        database
            .insert_session_operation("operation-new", "session-a", "reply")
            .await
            .expect("failed to insert new operation");

        // Cancel only the first operation via session-level bulk update.
        database
            .request_cancel_for_session_operations("session-a")
            .await
            .expect("failed to request cancel");

        // Simulate a new operation created after the cancel request by
        // resetting its flag directly (mirrors real flow where new
        // operations are inserted with cancel_requested = 0 by default).
        sqlx::query("UPDATE session_operation SET cancel_requested = 0 WHERE id = 'operation-new'")
            .execute(&database.pool)
            .await
            .expect("failed to reset new operation flag");

        // Act
        let cancelled_flag = database
            .is_cancel_requested_for_operation("operation-cancelled")
            .await
            .expect("failed to check cancelled operation");
        let new_flag = database
            .is_cancel_requested_for_operation("operation-new")
            .await
            .expect("failed to check new operation");

        // Assert — only the cancelled operation is flagged; the new one
        // proceeds normally.
        assert!(cancelled_flag);
        assert!(!new_flag);
    }

    /// Verifies `mark_session_operation_running()` sets the running state and
    /// timestamps.
    #[tokio::test]
    async fn test_mark_session_operation_running_sets_started_at_and_heartbeat() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");

        // Act
        database
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");
        let running_row = load_session_operation_row(&database, "operation-a").await;

        // Assert
        assert_eq!(running_row.status, "running");
        assert!(running_row.started_at.is_some());
        assert!(running_row.heartbeat_at.is_some());
        assert_eq!(running_row.last_error, None);
    }

    /// Verifies `mark_session_operation_done()` sets the terminal completion
    /// fields.
    #[tokio::test]
    async fn test_mark_session_operation_done_sets_finished_state() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        database
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");
        database
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");

        // Act
        database
            .mark_session_operation_done("operation-a")
            .await
            .expect("failed to mark operation done");
        let done_row = load_session_operation_row(&database, "operation-a").await;

        // Assert
        assert_eq!(done_row.status, "done");
        assert!(done_row.finished_at.is_some());
        assert!(done_row.heartbeat_at.is_some());
        assert_eq!(done_row.last_error, None);
    }

    /// Verifies `SessionJoinRow::into_session_row()` drops partially
    /// populated review-request columns instead of surfacing an invalid row
    /// model.
    #[test]
    fn test_session_join_row_ignores_partial_review_request_columns() {
        // Arrange
        let mut session_join_row = session_join_row_fixture();
        session_join_row.review_request_last_refreshed_at = None;

        // Act
        let session_row = session_join_row.into_session_row();

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.project_id, Some(7));
        assert_eq!(session_row.status, "Review");
        assert_eq!(session_row.added_lines, 14);
        assert_eq!(session_row.deleted_lines, 6);
        assert_eq!(session_row.review_request, None);
    }

    /// Verifies `SessionJoinRow::into_session_row()` maps a fully populated
    /// review-request into the public session row model.
    #[test]
    fn test_session_join_row_maps_review_request_columns() {
        // Arrange
        let session_join_row = session_join_row_fixture();

        // Act
        let session_row = session_join_row.into_session_row();

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.added_lines, 14);
        assert_eq!(session_row.deleted_lines, 6);
        assert_eq!(session_row.project_id, Some(7));
        assert_eq!(
            session_row.published_upstream_ref.as_deref(),
            Some("origin/session-a")
        );
        assert_eq!(session_row.questions.as_deref(), Some("Question text"));
        assert_eq!(session_row.summary.as_deref(), Some("Summary text"));
        assert_eq!(session_row.title.as_deref(), Some("Review session"));
        assert_review_request_row(&session_row);
    }

    /// Verifies `upsert_session_usage()` accumulates per-model token totals and
    /// invocation counts.
    #[tokio::test]
    async fn test_upsert_session_usage_accumulates_counts_per_model() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to insert first usage row");
        database
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 3,
                    output_tokens: 5,
                },
            )
            .await
            .expect("failed to update existing usage row");
        database
            .upsert_session_usage("session-a", "ignored-model", &SessionStats::default())
            .await
            .expect("failed to ignore zero-usage update");

        // Act
        let usage_rows = database
            .load_session_usage("session-a")
            .await
            .expect("failed to load session usage");

        // Assert
        assert_eq!(usage_rows.len(), 1);
        assert_eq!(usage_rows[0].model, "claude-opus-4.1");
        assert_eq!(usage_rows[0].input_tokens, 14);
        assert_eq!(usage_rows[0].invocation_count, 2);
        assert_eq!(usage_rows[0].output_tokens, 34);
        assert_eq!(usage_rows[0].session_id.as_deref(), Some("session-a"));
    }

    #[tokio::test]
    async fn test_setting_round_trip_supports_default_smart_fast_and_review_models() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        database
            .upsert_setting(
                SettingName::DefaultSmartModel,
                AgentModel::Gemini31ProPreview.as_str(),
            )
            .await
            .expect("failed to persist default smart model");
        database
            .upsert_setting(SettingName::DefaultFastModel, AgentModel::Gpt54.as_str())
            .await
            .expect("failed to persist default fast model");
        database
            .upsert_setting(
                SettingName::DefaultReviewModel,
                AgentModel::ClaudeOpus47.as_str(),
            )
            .await
            .expect("failed to persist default review model");

        // Act
        let default_smart_model = database
            .get_setting(SettingName::DefaultSmartModel)
            .await
            .expect("failed to load default smart model");
        let default_fast_model = database
            .get_setting(SettingName::DefaultFastModel)
            .await
            .expect("failed to load default fast model");
        let default_review_model = database
            .get_setting(SettingName::DefaultReviewModel)
            .await
            .expect("failed to load default review model");

        // Assert
        assert_eq!(
            default_smart_model,
            Some(AgentModel::Gemini31ProPreview.as_str().to_string())
        );
        assert_eq!(
            default_fast_model,
            Some(AgentModel::Gpt54.as_str().to_string())
        );
        assert_eq!(
            default_review_model,
            Some(AgentModel::ClaudeOpus47.as_str().to_string())
        );
    }

    #[tokio::test]
    async fn test_project_setting_round_trip_is_isolated_per_project() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .upsert_project("/tmp/project-a", Some("main"))
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project("/tmp/project-b", Some("main"))
            .await
            .expect("failed to insert second project");

        database
            .upsert_project_setting(first_project_id, SettingName::OpenCommand, "npm run dev")
            .await
            .expect("failed to persist first project setting");
        database
            .upsert_project_setting(second_project_id, SettingName::OpenCommand, "cargo test")
            .await
            .expect("failed to persist second project setting");

        // Act
        let first_project_setting = database
            .get_project_setting(first_project_id, SettingName::OpenCommand)
            .await
            .expect("failed to load first project setting");
        let second_project_setting = database
            .get_project_setting(second_project_id, SettingName::OpenCommand)
            .await
            .expect("failed to load second project setting");

        // Assert
        assert_eq!(first_project_setting, Some("npm run dev".to_string()));
        assert_eq!(second_project_setting, Some("cargo test".to_string()));
    }

    #[tokio::test]
    async fn test_project_reasoning_level_round_trip_uses_typed_setting_helpers() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        // Act
        database
            .set_project_reasoning_level(project_id, ReasoningLevel::Low)
            .await
            .expect("failed to persist project reasoning level");
        let reasoning_level = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load project reasoning level");

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::Low);
    }

    #[tokio::test]
    async fn test_load_project_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");

        // Act
        let missing_setting_level = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load default project reasoning level");
        database
            .upsert_project_setting(project_id, SettingName::ReasoningLevel, "unsupported")
            .await
            .expect("failed to insert unsupported project reasoning level");
        let invalid_setting_level = database
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load fallback project reasoning level");

        // Assert
        assert_eq!(missing_setting_level, ReasoningLevel::High);
        assert_eq!(invalid_setting_level, ReasoningLevel::High);
    }

    #[tokio::test]
    async fn test_reasoning_level_round_trip_uses_typed_setting_helpers() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        database
            .set_reasoning_level(ReasoningLevel::Low)
            .await
            .expect("failed to persist reasoning level");
        let reasoning_level = database
            .load_reasoning_level()
            .await
            .expect("failed to load reasoning level");

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::Low);
    }

    #[tokio::test]
    async fn test_load_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let missing_setting_level = database
            .load_reasoning_level()
            .await
            .expect("failed to load default reasoning level");
        database
            .upsert_setting(SettingName::ReasoningLevel, "unsupported")
            .await
            .expect("failed to insert unsupported reasoning level");
        let invalid_setting_level = database
            .load_reasoning_level()
            .await
            .expect("failed to load fallback reasoning level");

        // Assert
        assert_eq!(missing_setting_level, ReasoningLevel::High);
        assert_eq!(invalid_setting_level, ReasoningLevel::High);
    }

    #[tokio::test]
    async fn test_session_provider_conversation_id_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .update_session_provider_conversation_id("session-a", Some("thread-123"))
            .await
            .expect("failed to set provider conversation id");
        let stored_id = database
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load provider conversation id");
        database
            .update_session_provider_conversation_id("session-a", None)
            .await
            .expect("failed to clear provider conversation id");
        let cleared_id = database
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load cleared provider conversation id");

        // Assert
        assert_eq!(stored_id, Some("thread-123".to_string()));
        assert_eq!(cleared_id, None);
    }

    #[tokio::test]
    async fn test_session_instruction_conversation_id_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session");
        let instruction_conversation_id = Some("thread-123");

        // Act
        database
            .update_session_instruction_conversation_id("session-a", instruction_conversation_id)
            .await
            .expect("failed to set instruction conversation id");
        let stored_conversation_id = database
            .get_session_instruction_conversation_id("session-a")
            .await
            .expect("failed to load instruction conversation id");
        database
            .update_session_instruction_conversation_id("session-a", None)
            .await
            .expect("failed to clear instruction conversation id");
        let cleared_conversation_id = database
            .get_session_instruction_conversation_id("session-a")
            .await
            .expect("failed to load cleared instruction conversation id");

        // Assert
        assert_eq!(stored_conversation_id, Some("thread-123".to_string()));
        assert_eq!(cleared_conversation_id, None);
    }

    #[tokio::test]
    async fn test_session_published_upstream_ref_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .update_session_published_upstream_ref("session-a", Some("origin/wt/session-a"))
            .await
            .expect("failed to persist session published upstream ref");
        let persisted_row = database
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing persisted session row");
        database
            .update_session_published_upstream_ref("session-a", None)
            .await
            .expect("failed to clear session published upstream ref");
        let cleared_row = database
            .load_sessions()
            .await
            .expect("failed to load sessions after clearing")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing cleared session row");

        // Assert
        assert_eq!(
            persisted_row.published_upstream_ref.as_deref(),
            Some("origin/wt/session-a")
        );
        assert_eq!(cleared_row.published_upstream_ref, None);
    }

    #[tokio::test]
    async fn test_load_session_published_upstream_ref_returns_stored_value() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-load", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        database
            .update_session_published_upstream_ref("session-load", Some("origin/wt/session-load"))
            .await
            .expect("failed to set published upstream ref");

        // Act
        let loaded_ref = database
            .load_session_published_upstream_ref("session-load")
            .await
            .expect("failed to load published upstream ref");

        // Assert
        assert_eq!(loaded_ref.as_deref(), Some("origin/wt/session-load"));
    }

    #[tokio::test]
    async fn test_load_session_published_upstream_ref_returns_none_when_unset() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-unset", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let loaded_ref = database
            .load_session_published_upstream_ref("session-unset")
            .await
            .expect("failed to load published upstream ref");

        // Assert
        assert_eq!(loaded_ref, None);
    }

    #[tokio::test]
    async fn test_load_session_published_upstream_ref_returns_none_for_missing_session() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let loaded_ref = database
            .load_session_published_upstream_ref("nonexistent")
            .await
            .expect("failed to load published upstream ref");

        // Assert
        assert_eq!(loaded_ref, None);
    }

    #[tokio::test]
    async fn test_session_review_request_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let review_request = review_request_fixture();

        // Act
        database
            .update_session_review_request("session-a", Some(&review_request))
            .await
            .expect("failed to persist session review request");
        let persisted_row = database
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing persisted session row");
        database
            .update_session_review_request("session-a", None)
            .await
            .expect("failed to clear session review request");
        let cleared_row = database
            .load_sessions()
            .await
            .expect("failed to load sessions after clearing")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing cleared session row");

        // Assert
        assert_review_request_row(&persisted_row);
        assert_eq!(cleared_row.review_request, None);
    }

    #[tokio::test]
    async fn test_insert_session_creation_activity_at_persists_timestamp() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .insert_session_creation_activity_at("session-a", 123)
            .await
            .expect("failed to persist activity event");
        let activity_timestamps = database
            .load_session_activity_timestamps()
            .await
            .expect("failed to load activity timestamps");

        // Assert
        assert_eq!(activity_timestamps, vec![123]);
    }

    #[tokio::test]
    async fn test_insert_session_creation_activity_at_ignores_duplicates_per_session() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .insert_session_creation_activity_at("session-a", 100)
            .await
            .expect("failed to persist first activity event");
        database
            .insert_session_creation_activity_at("session-a", 200)
            .await
            .expect("failed to persist duplicate activity event");
        let activity_timestamps = database
            .load_session_activity_timestamps()
            .await
            .expect("failed to load activity timestamps");

        // Assert
        assert_eq!(activity_timestamps, vec![100]);
    }

    #[tokio::test]
    async fn test_load_session_activity_timestamps_keeps_deleted_session_history() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .insert_session_creation_activity_at("session-a", 100)
            .await
            .expect("failed to persist first activity event");
        database
            .insert_session("session-b", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .insert_session_creation_activity_at("session-b", 200)
            .await
            .expect("failed to persist second activity event");
        database
            .delete_session("session-a")
            .await
            .expect("failed to delete first session");

        // Act
        let activity_timestamps = database
            .load_session_activity_timestamps()
            .await
            .expect("failed to load activity timestamps");

        // Assert
        assert_eq!(activity_timestamps, vec![100, 200]);
    }

    /// Verifies `load_session_activity()` groups immutable activity rows by
    /// local day.
    #[tokio::test]
    async fn test_load_session_activity_groups_counts_by_local_day() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .insert_session("session-b", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .insert_session("session-c", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert third session");

        let first_day_timestamp = 10 * 86_400 + 10;
        let second_timestamp_same_day = 10 * 86_400 + 600;
        let second_day_timestamp = 11 * 86_400 + 50;

        database
            .clear_session_activity()
            .await
            .expect("failed to clear session activity");
        database
            .insert_session_creation_activity_at("session-a", first_day_timestamp)
            .await
            .expect("failed to persist first activity event");
        database
            .insert_session_creation_activity_at("session-b", second_timestamp_same_day)
            .await
            .expect("failed to persist second activity event");
        database
            .insert_session_creation_activity_at("session-c", second_day_timestamp)
            .await
            .expect("failed to persist third activity event");

        let expected_activity = vec![
            DailyActivity {
                day_key: local_day_key(first_day_timestamp),
                session_count: 2,
            },
            DailyActivity {
                day_key: local_day_key(second_day_timestamp),
                session_count: 1,
            },
        ];

        // Act
        let activity = database
            .load_session_activity()
            .await
            .expect("failed to load aggregated session activity");

        // Assert
        assert_eq!(activity, expected_activity);
    }

    #[tokio::test]
    async fn test_load_projects_with_stats_returns_session_counts_and_last_update() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session-a");
        database
            .insert_session("session-b", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session-b");

        // Act
        let projects = database
            .load_projects_with_stats()
            .await
            .expect("failed to load projects with stats");

        // Assert
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].session_count, 2);
        assert!(projects[0].last_session_updated_at.is_some());
    }

    /// Converts one Unix timestamp into the local day key used by heatmap
    /// activity rows.
    fn local_day_key(timestamp_seconds: i64) -> i64 {
        let utc_timestamp = time::OffsetDateTime::from_unix_timestamp(timestamp_seconds)
            .expect("timestamp should be valid for test fixture");
        let local_offset = time::UtcOffset::local_offset_at(utc_timestamp)
            .expect("local offset should resolve for test fixture");

        timestamp_seconds
            .saturating_add(i64::from(local_offset.whole_seconds()))
            .div_euclid(86_400)
    }

    /// Verifies the SQL activity aggregation matches Rust local-day grouping
    /// across a known daylight-saving transition in an isolated timezone-fixed
    /// subprocess.
    #[test]
    fn test_load_session_activity_matches_rust_grouping_across_dst_transition() {
        // Arrange
        if !cfg!(unix) {
            return;
        }

        let current_test_binary = env::current_exe().expect("failed to resolve current test bin");

        // Act
        let output = Command::new(current_test_binary)
            .env(DST_TEST_SUBPROCESS_ENV, "1")
            .env("TZ", "America/Los_Angeles")
            .arg(
                "test_load_session_activity_matches_rust_grouping_across_dst_transition_subprocess",
            )
            .arg("--exact")
            .arg("--test-threads=1")
            .output()
            .expect("failed to run DST subprocess test");

        // Assert
        assert!(
            output.status.success(),
            "DST subprocess test failed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Verifies the SQL activity aggregation keeps timestamps on both sides of
    /// the 2024 spring-forward transition in the same local day when Rust's
    /// per-event local-offset calculation says they should.
    #[tokio::test]
    async fn test_load_session_activity_matches_rust_grouping_across_dst_transition_subprocess() {
        // Arrange
        if !cfg!(unix) || env::var_os(DST_TEST_SUBPROCESS_ENV).is_none() {
            return;
        }

        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .insert_session("session-b", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .insert_session("session-c", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert third session");
        database
            .clear_session_activity()
            .await
            .expect("failed to clear activity history");

        // `2024-03-10T01:30:00-08:00`, still before the DST jump.
        let before_dst_jump = 1_710_063_000_i64;
        // `2024-03-10T03:30:00-07:00`, after the skipped hour.
        let after_dst_jump = 1_710_066_600_i64;
        // `2024-03-11T00:30:00-07:00`, the next local day.
        let next_local_day = 1_710_142_200_i64;

        database
            .insert_session_creation_activity_at("session-a", before_dst_jump)
            .await
            .expect("failed to persist pre-DST activity");
        database
            .insert_session_creation_activity_at("session-b", after_dst_jump)
            .await
            .expect("failed to persist post-DST activity");
        database
            .insert_session_creation_activity_at("session-c", next_local_day)
            .await
            .expect("failed to persist next-day activity");

        let first_day_key = local_day_key(before_dst_jump);
        let second_day_key = local_day_key(after_dst_jump);
        let third_day_key = local_day_key(next_local_day);

        // Act
        let activity = database
            .load_session_activity()
            .await
            .expect("failed to load grouped session activity");

        // Assert
        assert_eq!(first_day_key, second_day_key);
        assert_ne!(second_day_key, third_day_key);
        assert_eq!(
            activity,
            vec![
                DailyActivity {
                    day_key: first_day_key,
                    session_count: 2,
                },
                DailyActivity {
                    day_key: third_day_key,
                    session_count: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_set_and_load_active_project_id_round_trip() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");

        // Act
        database
            .set_active_project_id(project_id)
            .await
            .expect("failed to persist active project id");
        let active_project_id = database
            .load_active_project_id()
            .await
            .expect("failed to load active project id");

        // Assert
        assert_eq!(active_project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_load_session_project_id_returns_associated_project() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let loaded_project_id = database
            .load_session_project_id("session-a")
            .await
            .expect("failed to load session project id");

        // Assert
        assert_eq!(loaded_project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_load_session_summary_returns_persisted_summary() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Done", project_id)
            .await
            .expect("failed to insert session");
        database
            .update_session_summary("session-a", "persisted summary")
            .await
            .expect("failed to update session summary");

        // Act
        let loaded_summary = database
            .load_session_summary("session-a")
            .await
            .expect("failed to load session summary");

        // Assert
        assert_eq!(loaded_summary.as_deref(), Some("persisted summary"));
    }

    #[tokio::test]
    /// Verifies transactional turn-metadata persistence rolls back partial
    /// writes when any statement in the transaction fails.
    async fn test_persist_session_turn_metadata_rolls_back_on_failure() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to insert project");
        database
            .insert_session("session-a", "gpt-5.4", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        database
            .update_session_summary("session-a", "persisted summary")
            .await
            .expect("failed to seed summary");
        sqlx::query("DROP TABLE session_usage")
            .execute(database.pool())
            .await
            .expect("failed to drop session-usage table");

        // Act
        let result = database
            .persist_session_turn_metadata(
                "session-a",
                &SessionTurnMetadata {
                    instruction_conversation_id: Some("instruction-thread"),
                    model: AgentModel::Gpt54.as_str(),
                    provider_conversation_id: Some("thread-123"),
                    questions_json: r#"[{"text":"Need tests?"}]"#,
                    summary: r#"{"turn":"Updated the worker.","session":"Session state changed."}"#,
                    token_usage_delta: &SessionStats {
                        added_lines: 0,
                        deleted_lines: 0,
                        input_tokens: 3,
                        output_tokens: 5,
                    },
                },
            )
            .await;
        let session = database
            .load_sessions()
            .await
            .expect("failed to reload sessions")
            .into_iter()
            .find(|session| session.id == "session-a")
            .expect("expected seeded session");
        let provider_conversation_id = database
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load provider conversation id");

        // Assert
        assert!(matches!(result, Err(DbError::Query(_))));
        assert_eq!(session.summary.as_deref(), Some("persisted summary"));
        assert_eq!(session.questions.as_deref(), None);
        assert_eq!(session.input_tokens, 0);
        assert_eq!(session.output_tokens, 0);
        assert_eq!(provider_conversation_id.as_deref(), None);
    }

    #[tokio::test]
    async fn test_set_project_favorite_updates_project_state() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");

        // Act
        database
            .set_project_favorite(project_id, true)
            .await
            .expect("failed to set project favorite");
        let project = database
            .get_project(project_id)
            .await
            .expect("failed to load project")
            .expect("expected existing project");

        // Assert
        assert!(project.is_favorite);
    }

    #[tokio::test]
    async fn query_on_dropped_table_returns_db_error_query() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        sqlx::query("DROP TABLE session")
            .execute(database.pool())
            .await
            .expect("failed to drop table");

        // Act
        let result = database.load_sessions_metadata().await;

        // Assert
        assert!(
            matches!(result, Err(DbError::Query(_))),
            "expected DbError::Query variant"
        );
    }

    #[tokio::test]
    async fn db_error_display_includes_underlying_message() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        sqlx::query("DROP TABLE session")
            .execute(database.pool())
            .await
            .expect("failed to drop table");

        // Act
        let result = database.load_sessions_metadata().await;

        // Assert
        let error = result.expect_err("expected query on dropped table to fail");
        let display_text = error.to_string();
        assert!(
            !display_text.is_empty(),
            "DbError Display should produce a non-empty message"
        );
    }

    #[tokio::test]
    async fn open_with_unwritable_parent_returns_db_error_io() {
        // Arrange — place the database path under a regular file so
        // `create_dir_all` fails with an I/O error.
        let temp = tempdir().expect("failed to create temp directory");
        let blocking_file = temp.path().join("not_a_dir");
        std::fs::write(&blocking_file, b"").expect("failed to create blocking file");
        let db_path = blocking_file.join("nested").join("db.sqlite");

        // Act
        let result = Database::open(&db_path).await;

        // Assert
        assert!(
            matches!(result, Err(DbError::Io(_))),
            "expected DbError::Io variant"
        );
    }

    #[tokio::test]
    async fn open_configures_small_wal_pool_and_normal_synchronous_mode() {
        // Arrange
        let temp = tempdir().expect("failed to create temp directory");
        let db_path = temp.path().join("agentty.db");

        // Act
        let database = Database::open(&db_path)
            .await
            .expect("failed to open database");
        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load journal mode pragma");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load synchronous pragma");

        // Assert
        assert_eq!(
            database.pool().options().get_max_connections(),
            DB_POOL_MAX_CONNECTIONS
        );
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
    }

    #[tokio::test]
    async fn open_in_memory_uses_single_connection_and_normal_synchronous_mode() {
        // Arrange, Act
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory database");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load synchronous pragma");

        // Assert
        assert_eq!(database.pool().options().get_max_connections(), 1);
        assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
    }

    // NOTE: `DbError::Migration` is not directly tested because
    // `Database::open` and `Database::open_in_memory` run migrations
    // atomically after connecting — there is no injection point to
    // pre-corrupt the schema before migrations execute. The `#[from]`
    // derive mapping from `sqlx::migrate::MigrateError` is validated
    // at compile time by `thiserror`.
}
