//! Completed-turn persistence for resumable harness chats.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{SqliteConnection, SqlitePool, Transaction};
use thiserror::Error;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::model::{ModelMessage, ModelMetadata};
use crate::tool::{ReadArguments, ToolCall, WriteArguments};
use crate::{OutputSchema, OutputSchemaError, TurnError};

pub(crate) const TURN_LEASE_SECONDS: i64 = 300;
pub(crate) const TURN_LEASE_RENEWAL_INTERVAL_SECONDS: u64 = 100;

const DB_POOL_MAX_CONNECTIONS: u32 = 4;
const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const TURN_SIZE_PAGE_SIZE: i64 = 64;

struct ModelIdentityRow {
    model: Option<String>,
    provider: Option<String>,
}

struct SessionMessageRow {
    kind: String,
    payload: String,
    turn_position: i64,
}

struct SessionRow {
    max_history_bytes: i64,
    model: Option<String>,
    output_schema: String,
    provider: Option<String>,
    provider_session_id: Option<String>,
    system_prompt: Option<String>,
}

struct TurnSizeRow {
    retained_bytes: i64,
    turn_position: i64,
}

/// Stored identity needed to reconstruct a built-in model client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInfo {
    model: Option<String>,
    provider: Option<String>,
}

impl SessionInfo {
    /// Loads a session's stored model identity without constructing a harness.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the database cannot be opened or the
    /// session does not exist.
    pub async fn load(database_path: impl AsRef<Path>, id: &str) -> Result<Self, SessionError> {
        let database = Database::open(database_path.as_ref()).await?;
        let (provider, model) = database.load_model_identity(id).await?;

        Ok(Self { model, provider })
    }

    /// Returns the stored model identifier, when the session model exposed it.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the stored provider identifier, when the session model exposed
    /// it.
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }
}

/// Configuration stored when creating a persistent chat session.
#[derive(Clone, Debug)]
pub(crate) struct NewSession {
    id: String,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl NewSession {
    /// Creates a persistent-session configuration.
    pub(crate) fn new(id: impl Into<String>, schema: OutputSchema) -> Self {
        Self {
            id: id.into(),
            schema,
            system_prompt: None,
        }
    }

    /// Adds a system prompt that is restored with the session.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());

        self
    }

    pub(crate) fn with_optional_system_prompt(mut self, system_prompt: Option<String>) -> Self {
        self.system_prompt = system_prompt;

        self
    }

    /// Returns the stable application-provided session identifier.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Returns the structured-output schema retained by the session.
    pub(crate) fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the optional session system prompt.
    pub(crate) fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

/// Supplies Unix timestamps for persistent session writes.
pub(crate) trait TimestampSource: Send + Sync {
    /// Returns the current Unix timestamp in whole seconds.
    fn now_timestamp_seconds(&self) -> i64;
}

impl<TimestampFn> TimestampSource for TimestampFn
where
    TimestampFn: Fn() -> i64 + Send + Sync,
{
    fn now_timestamp_seconds(&self) -> i64 {
        self()
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum DatabaseIdentity {
    File(PathBuf),
    Temporary(u64),
}

impl DatabaseIdentity {
    async fn for_path(path: &Path) -> Result<Self, std::io::Error> {
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Ok(Self::temporary());
        }

        tokio::fs::canonicalize(path).await.map(Self::File)
    }

    fn temporary() -> Self {
        static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(0);

        Self::Temporary(NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed))
    }
}

/// SQLite database used by persistent harness sessions.
#[derive(Clone)]
pub(crate) struct Database {
    abandoned_turns: Arc<AbandonedTurnRegistry>,
    identity: DatabaseIdentity,
    pool: SqlitePool,
    #[cfg(test)]
    reservation_commit_control: Option<Arc<ReservationCommitControl>>,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl Database {
    /// Opens a SQLite database and runs embedded migrations.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be created, the
    /// database cannot be opened, or a migration fails.
    pub(crate) async fn open(path: &Path) -> Result<Self, SessionError> {
        Self::open_with_timestamp_source(path, system_timestamp_source()).await
    }

    /// Opens a SQLite database with an injected persistence timestamp source.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be created, the
    /// database cannot be opened, or a migration fails.
    pub(crate) async fn open_with_timestamp_source(
        path: &Path,
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Result<Self, SessionError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = connect_options(path);
        let pool = SqlitePoolOptions::new()
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .connect_with(options)
            .await
            .session_context("open persistent session database")?;

        sqlx::migrate!("./migrations").run(&pool).await?;
        let identity = DatabaseIdentity::for_path(path).await?;

        Ok(Self {
            abandoned_turns: shared_abandoned_turn_registry(),
            identity,
            pool,
            #[cfg(test)]
            reservation_commit_control: None,
            timestamp_source,
        })
    }

    /// Opens an isolated in-memory SQLite database and runs migrations.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or a migration
    /// fails.
    #[cfg(test)]
    pub(crate) async fn open_in_memory() -> Result<Self, SessionError> {
        Self::open_in_memory_with_timestamp_source(system_timestamp_source()).await
    }

    /// Opens an in-memory database with an injected timestamp source.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or a migration
    /// fails.
    #[cfg(test)]
    pub(crate) async fn open_in_memory_with_timestamp_source(
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Result<Self, SessionError> {
        let options = connect_options(Path::new(":memory:"));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .session_context("open in-memory persistent session database")?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self {
            abandoned_turns: shared_abandoned_turn_registry(),
            identity: DatabaseIdentity::temporary(),
            pool,
            reservation_commit_control: None,
            timestamp_source,
        })
    }

    pub(crate) async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        max_history_bytes: usize,
    ) -> Result<(), SessionError> {
        if config.id.trim().is_empty() {
            return Err(SessionError::InvalidData {
                reason: "session identifier must not be empty".to_string(),
            });
        }
        let max_history_bytes =
            i64::try_from(max_history_bytes).map_err(|_| SessionError::InvalidData {
                reason: "history byte limit exceeds SQLite integer range".to_string(),
            })?;
        let output_schema = config.schema.value().to_string();
        let system_prompt = config.system_prompt.as_deref();
        let (provider, model) = metadata.as_ref().map_or((None, None), |metadata| {
            (Some(metadata.provider()), Some(metadata.model()))
        });
        let now = self.timestamp_source.now_timestamp_seconds();
        let result = sqlx::query(
            r"
INSERT INTO session (
    id, provider, model, output_schema, system_prompt, max_history_bytes, created_at, updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO NOTHING
",
        )
        .bind(&config.id)
        .bind(provider)
        .bind(model)
        .bind(output_schema)
        .bind(system_prompt)
        .bind(max_history_bytes)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .session_context("create persistent session")?;

        if result.rows_affected() == 0 {
            return Err(SessionError::AlreadyExists {
                id: config.id.clone(),
            });
        }

        Ok(())
    }

    pub(crate) async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        self.recover_stale_turns(id).await?;
        let row = sqlx::query_as!(
            SessionRow,
            r#"
SELECT provider,
       model,
       output_schema,
       system_prompt,
       max_history_bytes AS "max_history_bytes!: i64",
       provider_session_id
FROM session
WHERE id = ?
"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .session_context("load persistent session")?
        .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        let max_history_bytes = decode_max_history_bytes(id, row.max_history_bytes)?;
        let output_schema = serde_json::from_str::<Value>(&row.output_schema).map_err(|error| {
            SessionError::InvalidData {
                reason: format!("session `{id}` has invalid output-schema JSON: {error}"),
            }
        })?;
        let schema = OutputSchema::new(output_schema)?;
        let turns = self.load_turns(id, max_history_bytes).await?;

        Ok(LoadedSession {
            max_history_bytes,
            model: row.model,
            provider: row.provider,
            provider_session_id: row.provider_session_id,
            schema,
            system_prompt: row.system_prompt,
            turns,
        })
    }

    async fn load_model_identity(
        &self,
        id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionError> {
        let row = sqlx::query_as!(
            ModelIdentityRow,
            "SELECT provider, model FROM session WHERE id = ?",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .session_context("load persistent session model identity")?
        .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;

        Ok((row.provider, row.model))
    }

    pub(crate) async fn begin_turn(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<AcquiredTurn, SessionError> {
        let message = EncodedMessage::from_message(&ModelMessage::User(prompt.to_string()))?;

        loop {
            let acquisition = self.load_turn_acquisition(session_id).await?;
            if let Some(guard) = self
                .reserve_turn(session_id, &message, &acquisition)
                .await?
            {
                return Ok(AcquiredTurn {
                    guard,
                    provider_session_id: acquisition.provider_session_id,
                    turn_position: acquisition.turn_position,
                    turns: acquisition.turns,
                });
            }
        }
    }

    async fn load_turn_acquisition(
        &self,
        session_id: &str,
    ) -> Result<TurnAcquisition, SessionError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("load persistent session turn acquisition")?;
        let session = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT max_history_bytes, provider_session_id FROM session WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .session_context("load persistent session turn acquisition")?
        .ok_or_else(|| SessionError::NotFound {
            id: session_id.to_string(),
        })?;
        let (max_history_bytes, provider_session_id) = session;
        let max_history_bytes = decode_max_history_bytes(session_id, max_history_bytes)?;
        let turn_position = next_turn_position(&mut transaction, session_id).await?;
        let latest_completed_turn = latest_completed_turn(&mut transaction, session_id).await?;
        let turns = load_turns_from(&mut transaction, session_id, max_history_bytes).await?;
        transaction
            .commit()
            .await
            .session_context("load persistent session turn acquisition")?;

        Ok(TurnAcquisition {
            latest_completed_turn,
            max_history_bytes,
            provider_session_id,
            turn_position,
            turns,
        })
    }

    async fn reserve_turn(
        &self,
        session_id: &str,
        message: &EncodedMessage,
        acquisition: &TurnAcquisition,
    ) -> Result<Option<TurnGuard>, SessionError> {
        let recovery_now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("reserve persistent session turn")?;
        recover_stale_turns(&mut transaction, session_id, recovery_now).await?;
        let abandoned_turns = self.abandoned_turns.for_session(&self.identity, session_id);
        for owner in &abandoned_turns {
            interrupt_owned_turn(&mut transaction, owner, recovery_now)
                .await
                .session_context("recover abandoned persistent session turn")?;
        }
        let session = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT max_history_bytes, provider_session_id FROM session WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .session_context("reserve persistent session turn")?
        .ok_or_else(|| SessionError::NotFound {
            id: session_id.to_string(),
        })?;
        let (max_history_bytes, provider_session_id) = session;
        let max_history_bytes = decode_max_history_bytes(session_id, max_history_bytes)?;
        let turn_position = next_turn_position(&mut transaction, session_id).await?;
        let latest_completed_turn = latest_completed_turn(&mut transaction, session_id).await?;
        if max_history_bytes != acquisition.max_history_bytes
            || provider_session_id != acquisition.provider_session_id
            || turn_position != acquisition.turn_position
            || latest_completed_turn != acquisition.latest_completed_turn
        {
            return Ok(None);
        }
        let reservation_now = self.timestamp_source.now_timestamp_seconds();
        let lease_expires_at = reservation_now.saturating_add(TURN_LEASE_SECONDS);
        let result = sqlx::query_scalar::<_, Vec<u8>>(
            r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at,
    owner_token
)
VALUES (?, ?, 'running', NULL, ?, ?, ?, randomblob(16))
RETURNING owner_token
",
        )
        .bind(session_id)
        .bind(acquisition.turn_position)
        .bind(lease_expires_at)
        .bind(reservation_now)
        .bind(reservation_now)
        .fetch_one(&mut *transaction)
        .await;
        let owner_token = result.map_err(|error| {
            if is_unique_violation(&error) {
                SessionError::Busy {
                    id: session_id.to_string(),
                }
            } else {
                SessionError::QueryContext {
                    operation: "begin persistent session turn",
                    source: error,
                }
            }
        })?;
        let owner = TurnOwner {
            database: self.identity.clone(),
            interruption_error_type: "interrupted",
            session_id: session_id.to_string(),
            token: owner_token,
            turn_position: acquisition.turn_position,
        };
        let mut guard = TurnGuard::new(self, owner);
        insert_message(
            &mut transaction,
            session_id,
            acquisition.turn_position,
            0,
            message,
            reservation_now,
            "reserve persistent session turn",
        )
        .await?;
        transaction
            .commit()
            .await
            .session_context("reserve persistent session turn")?;
        #[cfg(test)]
        if self
            .reservation_commit_control
            .as_ref()
            .is_some_and(|control| control.pause_after_commit())
        {
            std::future::pending::<()>().await;
        }
        self.abandoned_turns.remove(&abandoned_turns);
        guard.activate();

        Ok(Some(guard))
    }

    pub(crate) async fn complete_turn(
        &self,
        session_id: &str,
        turn_position: i64,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError> {
        let encoded_messages = messages
            .iter()
            .map(EncodedMessage::from_message)
            .collect::<Result<Vec<_>, _>>()?;
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("complete persistent session turn")?;
        for (index, message) in encoded_messages.iter().enumerate() {
            let message_position = i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1);
            insert_message(
                &mut transaction,
                session_id,
                turn_position,
                message_position,
                message,
                now,
                "complete persistent session turn",
            )
            .await?;
        }
        let result = sqlx::query(
            r"
UPDATE session_turn
SET status = 'completed', error_type = NULL, lease_expires_at = NULL, updated_at = ?
WHERE session_id = ? AND turn_position = ? AND status = 'running'
",
        )
        .bind(now)
        .bind(session_id)
        .bind(turn_position)
        .execute(&mut *transaction)
        .await
        .session_context("complete persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(SessionError::InvalidData {
                reason: format!("session `{session_id}` turn {turn_position} is not running"),
            });
        }
        sqlx::query(
            r"
UPDATE session
SET provider_session_id = ?, updated_at = ?
WHERE id = ?
",
        )
        .bind(provider_session_id)
        .bind(now)
        .bind(session_id)
        .execute(&mut *transaction)
        .await
        .session_context("complete persistent session turn")?;
        transaction
            .commit()
            .await
            .session_context("complete persistent session turn")?;

        Ok(())
    }

    pub(crate) async fn fail_turn(
        &self,
        session_id: &str,
        turn_position: i64,
        error: &TurnError,
    ) -> Result<(), SessionError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let error_type = format!("{:?}", error.error_type());
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("fail persistent session turn")?;
        let result = sqlx::query(
            r"
UPDATE session_turn
SET status = 'failed', error_type = ?, lease_expires_at = NULL, updated_at = ?
WHERE session_id = ? AND turn_position = ? AND status = 'running'
",
        )
        .bind(error_type)
        .bind(now)
        .bind(session_id)
        .bind(turn_position)
        .execute(&mut *transaction)
        .await
        .session_context("fail persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(SessionError::InvalidData {
                reason: format!("session `{session_id}` turn {turn_position} is not running"),
            });
        }
        sqlx::query(
            r"
UPDATE session
SET provider_session_id = NULL, updated_at = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(session_id)
        .execute(&mut *transaction)
        .await
        .session_context("fail persistent session turn")?;
        transaction
            .commit()
            .await
            .session_context("fail persistent session turn")?;

        Ok(())
    }

    async fn recover_stale_turns(&self, session_id: &str) -> Result<(), SessionError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        sqlx::query(
            r"
UPDATE session_turn
SET status = 'interrupted', error_type = 'interrupted', lease_expires_at = NULL, updated_at = ?
WHERE session_id = ?
  AND status IN ('pending', 'running')
  AND lease_expires_at <= ?
",
        )
        .bind(now)
        .bind(session_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .session_context("recover stale persistent session turns")?;

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    #[cfg(test)]
    pub(crate) async fn append_turn(
        &self,
        session_id: &str,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError> {
        let encoded_messages = messages
            .iter()
            .map(EncodedMessage::from_message)
            .collect::<Result<Vec<_>, _>>()?;
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("append persistent session turn")?;
        let result = sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(session_id)
        .execute(&mut *transaction)
        .await
        .session_context("append persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(SessionError::NotFound {
                id: session_id.to_string(),
            });
        }
        let turn_position = next_turn_position(&mut transaction, session_id).await?;
        sqlx::query(
            r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at
)
VALUES (?, ?, 'completed', NULL, NULL, ?, ?)
",
        )
        .bind(session_id)
        .bind(turn_position)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .session_context("append persistent session turn")?;

        for (message_position, message) in encoded_messages.iter().enumerate() {
            let message_position = i64::try_from(message_position).unwrap_or(i64::MAX);
            sqlx::query(
                r"
INSERT INTO session_message (
    session_id, turn_position, message_position, kind, payload, retained_bytes, created_at
)
VALUES (?, ?, ?, ?, ?, ?, ?)
",
            )
            .bind(session_id)
            .bind(turn_position)
            .bind(message_position)
            .bind(message.kind)
            .bind(&message.payload)
            .bind(message.retained_bytes)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .session_context("append persistent session turn")?;
        }

        transaction
            .commit()
            .await
            .session_context("append persistent session turn")?;

        Ok(())
    }

    async fn load_turns(
        &self,
        session_id: &str,
        max_history_bytes: usize,
    ) -> Result<Vec<Vec<ModelMessage>>, SessionError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .session_context("load persistent session history")?;

        load_turns_from(&mut connection, session_id, max_history_bytes).await
    }
}

/// Error returned by persistent session operations.
#[derive(Debug, Error)]
pub enum SessionError {
    /// A session with the requested identifier already exists.
    #[error("persistent session `{id}` already exists")]
    AlreadyExists {
        /// Conflicting session identifier.
        id: String,
    },
    /// Another process or task is already running a turn for this session.
    #[error("persistent session `{id}` already has an active turn")]
    Busy {
        /// Busy session identifier.
        id: String,
    },
    /// A filesystem operation failed while opening the database.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Persisted data violated a session invariant.
    #[error("invalid persistent session data: {reason}")]
    InvalidData {
        /// Description of the invalid persisted value.
        reason: String,
    },
    /// The model supplied while opening a session differs from the saved model.
    #[error(
        "persistent session `{id}` uses {stored_provider}/{stored_model}, not \
         {actual_provider}/{actual_model}"
    )]
    ModelMismatch {
        /// Model supplied by the current harness.
        actual_model: String,
        /// Provider supplied by the current harness.
        actual_provider: String,
        /// Session identifier.
        id: String,
        /// Model stored with the session.
        stored_model: String,
        /// Provider stored with the session.
        stored_provider: String,
    },
    /// An embedded migration failed.
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    /// The requested session does not exist.
    #[error("persistent session `{id}` does not exist")]
    NotFound {
        /// Missing session identifier.
        id: String,
    },
    /// The active turn's lease was recovered by another owner.
    #[error("persistent session `{id}` lost ownership of turn {turn_position}")]
    OwnershipLost {
        /// Session identifier whose turn lost ownership.
        id: String,
        /// Position of the turn that lost ownership.
        turn_position: i64,
    },
    /// A named SQLite operation failed.
    #[error("persistent session operation `{operation}` failed: {source}")]
    QueryContext {
        /// Stable semantic operation name.
        operation: &'static str,
        /// Underlying `SQLx` failure.
        #[source]
        source: sqlx::Error,
    },
    /// A persisted output schema is no longer valid.
    #[error(transparent)]
    Schema(#[from] OutputSchemaError),
    /// Durable session operations require a configured SQLite database.
    #[error("durable sessions require Harness::database(path)")]
    StorageRequired,
    /// The model turn failed before it could be persisted.
    #[error(transparent)]
    Turn(#[from] TurnError),
    /// A model turn and the attempt to persist its failure both failed.
    #[error("{turn}; additionally failed to persist the turn failure: {persistence}")]
    TurnPersistence {
        /// Failure returned by the model turn.
        #[source]
        turn: TurnError,
        /// Failure returned while recording the turn failure.
        persistence: Box<SessionError>,
    },
}

pub(crate) struct AcquiredTurn {
    pub(crate) guard: TurnGuard,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) turn_position: i64,
    pub(crate) turns: Vec<Vec<ModelMessage>>,
}

pub(crate) struct LoadedSession {
    pub(crate) max_history_bytes: usize,
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) schema: OutputSchema,
    pub(crate) system_prompt: Option<String>,
    pub(crate) turns: Vec<Vec<ModelMessage>>,
}

struct TurnAcquisition {
    latest_completed_turn: Option<i64>,
    max_history_bytes: usize,
    provider_session_id: Option<String>,
    turn_position: i64,
    turns: Vec<Vec<ModelMessage>>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct TurnOwner {
    database: DatabaseIdentity,
    interruption_error_type: &'static str,
    session_id: String,
    token: Vec<u8>,
    turn_position: i64,
}

#[derive(Default)]
struct AbandonedTurnRegistry {
    owners: Mutex<HashSet<TurnOwner>>,
}

impl AbandonedTurnRegistry {
    fn for_session(&self, database: &DatabaseIdentity, session_id: &str) -> Vec<TurnOwner> {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|owner| &owner.database == database && owner.session_id == session_id)
            .cloned()
            .collect()
    }

    fn register(&self, owner: TurnOwner) {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(owner);
    }

    fn remove(&self, owners: &[TurnOwner]) {
        let mut registered = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for owner in owners {
            registered.remove(owner);
        }
    }
}

fn shared_abandoned_turn_registry() -> Arc<AbandonedTurnRegistry> {
    static REGISTRY: OnceLock<Arc<AbandonedTurnRegistry>> = OnceLock::new();

    Arc::clone(REGISTRY.get_or_init(Arc::default))
}

pub(crate) struct TurnGuard {
    armed: bool,
    owner: TurnOwner,
    ownership_failure: Option<oneshot::Receiver<SessionError>>,
    pool: SqlitePool,
    registry: Arc<AbandonedTurnRegistry>,
    renewal_stop: Option<oneshot::Sender<()>>,
    renewal_task: Option<JoinHandle<()>>,
    runtime: tokio::runtime::Handle,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl TurnGuard {
    fn new(database: &Database, owner: TurnOwner) -> Self {
        Self {
            armed: true,
            owner,
            ownership_failure: None,
            pool: database.pool.clone(),
            registry: Arc::clone(&database.abandoned_turns),
            renewal_stop: None,
            renewal_task: None,
            runtime: tokio::runtime::Handle::current(),
            timestamp_source: Arc::clone(&database.timestamp_source),
        }
    }

    fn activate(&mut self) {
        self.owner.interruption_error_type = "cancelled";
        let owner = self.owner.clone();
        let interval = Duration::from_secs(TURN_LEASE_RENEWAL_INTERVAL_SECONDS);
        let first_renewal = Instant::now() + interval;
        let pool = self.pool.clone();
        let timestamp_source = Arc::clone(&self.timestamp_source);
        let (renewal_stop, mut stop_requested) = oneshot::channel();
        let (ownership_failed, ownership_failure) = oneshot::channel();
        self.renewal_stop = Some(renewal_stop);
        self.ownership_failure = Some(ownership_failure);
        self.renewal_task = Some(self.runtime.spawn(async move {
            let mut ticker = tokio::time::interval_at(first_renewal, interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let now = timestamp_source.now_timestamp_seconds();
                        let failure = match renew_owned_turn(&pool, &owner, now).await {
                            Ok(true) => continue,
                            Ok(false) => SessionError::OwnershipLost {
                                id: owner.session_id.clone(),
                                turn_position: owner.turn_position,
                            },
                            Err(source) => SessionError::QueryContext {
                                operation: "renew persistent session turn lease",
                                source,
                            },
                        };
                        let _ = ownership_failed.send(failure);

                        break;
                    }
                    _ = &mut stop_requested => break,
                }
            }
        }));
    }

    pub(crate) async fn ownership_failure(&mut self) -> SessionError {
        let Some(failure) = self.ownership_failure.take() else {
            return SessionError::OwnershipLost {
                id: self.owner.session_id.clone(),
                turn_position: self.owner.turn_position,
            };
        };

        failure
            .await
            .unwrap_or_else(|_| SessionError::OwnershipLost {
                id: self.owner.session_id.clone(),
                turn_position: self.owner.turn_position,
            })
    }

    pub(crate) fn disarm(&mut self) {
        self.stop_renewal();
        self.armed = false;
    }

    pub(crate) fn mark_interrupted(&mut self) {
        self.owner.interruption_error_type = "interrupted";
    }

    fn stop_renewal(&mut self) {
        if let Some(stop) = self.renewal_stop.take() {
            let _ = stop.send(());
        }
        self.renewal_task.take();
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.stop_renewal();
        if !self.armed {
            return;
        }
        let owner = self.owner.clone();
        self.registry.register(owner.clone());

        let pool = self.pool.clone();
        let registry = Arc::clone(&self.registry);
        let timestamp_source = Arc::clone(&self.timestamp_source);
        std::mem::drop(self.runtime.spawn(async move {
            let result = async {
                let mut connection = pool.acquire().await?;
                interrupt_owned_turn(
                    &mut connection,
                    &owner,
                    timestamp_source.now_timestamp_seconds(),
                )
                .await
            }
            .await;
            if result.is_ok() {
                registry.remove(std::slice::from_ref(&owner));
            }
        }));
    }
}

#[cfg(test)]
#[derive(Default)]
struct ReservationCommitControl {
    commit_seen: std::sync::atomic::AtomicBool,
    pause_once: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl ReservationCommitControl {
    fn paused() -> Self {
        Self {
            commit_seen: std::sync::atomic::AtomicBool::new(false),
            pause_once: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn pause_after_commit(&self) -> bool {
        if !self
            .pause_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }
        self.commit_seen
            .store(true, std::sync::atomic::Ordering::SeqCst);

        true
    }
}

struct EncodedMessage {
    kind: &'static str,
    payload: String,
    retained_bytes: i64,
}

impl EncodedMessage {
    fn from_message(message: &ModelMessage) -> Result<Self, SessionError> {
        let (kind, payload) = match message {
            ModelMessage::Assistant(content) => ("assistant", serialize_payload(content)),
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } => (
                "assistant_reasoning",
                serialize_payload(&StoredAssistantReasoning {
                    content,
                    reasoning_content,
                }),
            ),
            ModelMessage::AssistantToolCall(call) => (
                "assistant_tool_call",
                serialize_payload(&StoredToolCall::from_call(call)?),
            ),
            ModelMessage::AssistantToolCalls(calls) => (
                "assistant_tool_calls",
                calls
                    .iter()
                    .map(StoredToolCall::from_call)
                    .collect::<Result<Vec<_>, _>>()
                    .and_then(|calls| serialize_payload(&calls)),
            ),
            ModelMessage::System(_) => {
                return Err(SessionError::InvalidData {
                    reason: "system prompts must be stored on the session".to_string(),
                });
            }
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            } => (
                "tool_result",
                serialize_payload(&StoredToolResult {
                    call_id,
                    content,
                    name,
                }),
            ),
            ModelMessage::User(content) => ("user", serialize_payload(content)),
        };
        let payload = payload?;
        let retained_bytes = i64::try_from(message.retained_bytes()).unwrap_or(i64::MAX);

        Ok(Self {
            kind,
            payload,
            retained_bytes,
        })
    }

    fn into_message(kind: &str, payload: &str) -> Result<ModelMessage, SessionError> {
        match kind {
            "assistant" => deserialize_payload(payload).map(ModelMessage::Assistant),
            "assistant_reasoning" => {
                let assistant = deserialize_payload::<StoredAssistantReasoningOwned>(payload)?;

                Ok(ModelMessage::AssistantReasoning {
                    content: assistant.content,
                    reasoning_content: assistant.reasoning_content,
                })
            }
            "assistant_tool_call" => deserialize_payload::<StoredToolCall>(payload)?
                .into_call()
                .map(ModelMessage::AssistantToolCall),
            "assistant_tool_calls" => deserialize_payload::<Vec<StoredToolCall>>(payload)?
                .into_iter()
                .map(StoredToolCall::into_call)
                .collect::<Result<Vec<_>, _>>()
                .map(ModelMessage::AssistantToolCalls),
            "tool_result" => {
                let result = deserialize_payload::<StoredToolResultOwned>(payload)?;

                Ok(ModelMessage::ToolResult {
                    call_id: result.call_id,
                    content: result.content,
                    name: result.name,
                })
            }
            "user" => deserialize_payload(payload).map(ModelMessage::User),
            _ => Err(SessionError::InvalidData {
                reason: format!("unknown persistent message kind `{kind}`"),
            }),
        }
    }
}

#[derive(Serialize)]
struct StoredAssistantReasoning<'a> {
    content: &'a str,
    reasoning_content: &'a str,
}

#[derive(Deserialize)]
struct StoredAssistantReasoningOwned {
    content: String,
    reasoning_content: String,
}

#[derive(Deserialize, Serialize)]
struct StoredToolCall {
    arguments: Value,
    id: String,
    name: String,
    reasoning_content: Option<String>,
}

impl StoredToolCall {
    fn from_call(call: &ToolCall) -> Result<Self, SessionError> {
        let arguments = call
            .arguments_json()
            .map_err(|error| invalid_json(&error))?;
        let arguments = serde_json::from_str(&arguments).map_err(|error| invalid_json(&error))?;

        Ok(Self {
            arguments,
            id: call.id().to_string(),
            name: call.name().to_string(),
            reasoning_content: call.reasoning_content().map(str::to_string),
        })
    }

    fn into_call(self) -> Result<ToolCall, SessionError> {
        match self.name.as_str() {
            "read" => serde_json::from_value::<ReadArguments>(self.arguments)
                .map(|arguments| ToolCall::read(self.id, arguments, self.reasoning_content))
                .map_err(|error| invalid_json(&error)),
            "write" => serde_json::from_value::<WriteArguments>(self.arguments)
                .map(|arguments| ToolCall::write(self.id, arguments, self.reasoning_content))
                .map_err(|error| invalid_json(&error)),
            _ => Err(SessionError::InvalidData {
                reason: format!("unknown persistent tool `{}`", self.name),
            }),
        }
    }
}

#[derive(Serialize)]
struct StoredToolResult<'a> {
    call_id: &'a str,
    content: &'a str,
    name: &'a str,
}

#[derive(Deserialize)]
struct StoredToolResultOwned {
    call_id: String,
    content: String,
    name: String,
}

trait DbResultExt<T> {
    fn session_context(self, operation: &'static str) -> Result<T, SessionError>;
}

impl<T> DbResultExt<T> for Result<T, sqlx::Error> {
    fn session_context(self, operation: &'static str) -> Result<T, SessionError> {
        self.map_err(|source| SessionError::QueryContext { operation, source })
    }
}

fn connect_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(DB_BUSY_TIMEOUT)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
}

async fn load_turns_from(
    connection: &mut SqliteConnection,
    session_id: &str,
    max_history_bytes: usize,
) -> Result<Vec<Vec<ModelMessage>>, SessionError> {
    let Some(oldest_turn) =
        oldest_turn_within_budget(connection, session_id, max_history_bytes).await?
    else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query_as!(
        SessionMessageRow,
        r#"
SELECT message.turn_position AS "turn_position!: i64",
       message.kind AS "kind!: String",
       message.payload AS "payload!: String"
FROM session_message AS message
JOIN session_turn AS turn
  ON turn.session_id = message.session_id
 AND turn.turn_position = message.turn_position
WHERE message.session_id = ?
  AND message.turn_position >= ?
  AND turn.status = 'completed'
ORDER BY message.turn_position, message.message_position
"#,
        session_id,
        oldest_turn
    )
    .fetch_all(&mut *connection)
    .await
    .session_context("load persistent session history")?;
    let mut turns = Vec::<Vec<ModelMessage>>::new();
    let mut current_position = None;

    for row in rows {
        if current_position != Some(row.turn_position) {
            turns.push(Vec::new());
            current_position = Some(row.turn_position);
        }
        let message = EncodedMessage::into_message(&row.kind, &row.payload)?;
        if let Some(turn) = turns.last_mut() {
            turn.push(message);
        }
    }

    Ok(turns)
}

async fn oldest_turn_within_budget(
    connection: &mut SqliteConnection,
    session_id: &str,
    max_history_bytes: usize,
) -> Result<Option<i64>, SessionError> {
    let mut retained_bytes = 0_usize;
    let mut oldest_turn = None;
    let mut before_turn = None;

    'pages: loop {
        let turn_sizes = load_turn_size_page(connection, session_id, before_turn).await?;
        if turn_sizes.is_empty() {
            break;
        }
        let page_is_full =
            turn_sizes.len() == usize::try_from(TURN_SIZE_PAGE_SIZE).unwrap_or(usize::MAX);

        for (turn_position, turn_bytes) in turn_sizes {
            let turn_bytes =
                usize::try_from(turn_bytes).map_err(|_| SessionError::InvalidData {
                    reason: format!("session `{session_id}` has an invalid retained byte count"),
                })?;
            let next_retained_bytes = retained_bytes.saturating_add(turn_bytes);
            if next_retained_bytes > max_history_bytes {
                break 'pages;
            }
            retained_bytes = next_retained_bytes;
            oldest_turn = Some(turn_position);
            before_turn = Some(turn_position);
        }

        if !page_is_full {
            break;
        }
    }

    Ok(oldest_turn)
}

async fn load_turn_size_page(
    connection: &mut SqliteConnection,
    session_id: &str,
    before_turn: Option<i64>,
) -> Result<Vec<(i64, i64)>, SessionError> {
    let Some(inclusive_end) =
        before_turn.map_or(Some(i64::MAX), |position| position.checked_sub(1))
    else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query_as!(
        TurnSizeRow,
        r#"
SELECT message.turn_position AS "turn_position!: i64",
       SUM(message.retained_bytes) AS "retained_bytes!: i64"
FROM session_message AS message
JOIN session_turn AS turn
  ON turn.session_id = message.session_id
 AND turn.turn_position = message.turn_position
WHERE message.session_id = ?
  AND message.turn_position <= ?
  AND turn.status = 'completed'
GROUP BY message.turn_position
ORDER BY message.turn_position DESC
LIMIT ?
"#,
        session_id,
        inclusive_end,
        TURN_SIZE_PAGE_SIZE
    )
    .fetch_all(&mut *connection)
    .await
    .session_context("load persistent session history")?;

    Ok(rows
        .into_iter()
        .map(|row| (row.turn_position, row.retained_bytes))
        .collect())
}

fn decode_max_history_bytes(id: &str, max_history_bytes: i64) -> Result<usize, SessionError> {
    usize::try_from(max_history_bytes).map_err(|_| SessionError::InvalidData {
        reason: format!("session `{id}` has an invalid history byte limit"),
    })
}

async fn next_turn_position(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
) -> Result<i64, SessionError> {
    sqlx::query_scalar::<_, i64>(
        r"
SELECT COALESCE(MAX(turn_position), -1) + 1
FROM session_turn
WHERE session_id = ?
",
    )
    .bind(session_id)
    .fetch_one(&mut **transaction)
    .await
    .session_context("append persistent session turn")
}

async fn latest_completed_turn(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
) -> Result<Option<i64>, SessionError> {
    sqlx::query_scalar::<_, Option<i64>>(
        r"
SELECT MAX(turn_position)
FROM session_turn
WHERE session_id = ? AND status = 'completed'
",
    )
    .bind(session_id)
    .fetch_one(&mut **transaction)
    .await
    .session_context("load persistent session history position")
}

async fn insert_message(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
    turn_position: i64,
    message_position: i64,
    message: &EncodedMessage,
    now: i64,
    operation: &'static str,
) -> Result<(), SessionError> {
    sqlx::query(
        r"
INSERT INTO session_message (
    session_id, turn_position, message_position, kind, payload, retained_bytes, created_at
)
VALUES (?, ?, ?, ?, ?, ?, ?)
",
    )
    .bind(session_id)
    .bind(turn_position)
    .bind(message_position)
    .bind(message.kind)
    .bind(&message.payload)
    .bind(message.retained_bytes)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .session_context(operation)?;

    Ok(())
}

async fn recover_stale_turns(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
    now: i64,
) -> Result<(), SessionError> {
    sqlx::query(
        r"
UPDATE session_turn
SET status = 'interrupted', error_type = 'interrupted', lease_expires_at = NULL, updated_at = ?
WHERE session_id = ?
  AND status IN ('pending', 'running')
  AND lease_expires_at <= ?
",
    )
    .bind(now)
    .bind(session_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .session_context("recover stale persistent session turns")?;

    Ok(())
}

async fn renew_owned_turn(
    pool: &SqlitePool,
    owner: &TurnOwner,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let lease_expires_at = now.saturating_add(TURN_LEASE_SECONDS);
    let result = sqlx::query(
        r"
UPDATE session_turn
SET lease_expires_at = ?, updated_at = ?
WHERE session_id = ?
  AND turn_position = ?
  AND owner_token = ?
  AND status = 'running'
",
    )
    .bind(lease_expires_at)
    .bind(now)
    .bind(&owner.session_id)
    .bind(owner.turn_position)
    .bind(&owner.token)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

async fn interrupt_owned_turn(
    connection: &mut SqliteConnection,
    owner: &TurnOwner,
    now: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
UPDATE session_turn
SET status = 'interrupted', error_type = ?, lease_expires_at = NULL, updated_at = ?
WHERE session_id = ?
  AND turn_position = ?
  AND owner_token = ?
  AND status IN ('pending', 'running')
",
    )
    .bind(owner.interruption_error_type)
    .bind(now)
    .bind(&owner.session_id)
    .bind(owner.turn_position)
    .bind(&owner.token)
    .execute(connection)
    .await?;

    Ok(())
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
}

fn serialize_payload<T: Serialize>(payload: &T) -> Result<String, SessionError> {
    serde_json::to_string(payload).map_err(|error| invalid_json(&error))
}

fn deserialize_payload<'a, T: Deserialize<'a>>(payload: &'a str) -> Result<T, SessionError> {
    serde_json::from_str(payload).map_err(|error| invalid_json(&error))
}

fn invalid_json(error: &serde_json::Error) -> SessionError {
    SessionError::InvalidData {
        reason: format!("invalid persistent message JSON: {error}"),
    }
}

fn system_timestamp_source() -> Arc<dyn TimestampSource> {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    use serde_json::json;
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType, Migrator};
    use tempfile::tempdir;

    use super::*;
    use crate::model::{MockModel, ModelResponse};

    struct SessionTimestampsRow {
        message_timestamp: i64,
        session_created_at: i64,
        session_updated_at: i64,
    }

    struct TurnStatusRow {
        message_count: i64,
        status: String,
    }

    fn schema() -> OutputSchema {
        OutputSchema::new(json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn model() -> MockModel {
        let mut model = MockModel::new();
        model.expect_metadata().return_const(None);

        model
    }

    fn metadata_model(provider: &'static str, model_name: &str) -> MockModel {
        let mut model = MockModel::new();
        let metadata = ModelMetadata::new(provider, model_name).expect("metadata should be valid");
        model.expect_metadata().return_const(Some(metadata));

        model
    }

    fn read_call(id: &str) -> ToolCall {
        let arguments = serde_json::from_value::<ReadArguments>(json!({
            "action": "file",
            "path": "Cargo.toml",
            "limit": 1
        }))
        .expect("read arguments should be valid");

        ToolCall::read(
            id.to_string(),
            arguments,
            Some("inspect manifest".to_string()),
        )
    }

    fn write_call(id: &str) -> ToolCall {
        let arguments = serde_json::from_value::<WriteArguments>(json!({
            "path": "src/lib.rs",
            "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
        }))
        .expect("write arguments should be valid");

        ToolCall::write(id.to_string(), arguments, None)
    }

    fn turn(prompt: &str, answer: &str) -> Vec<ModelMessage> {
        vec![
            ModelMessage::User(prompt.to_string()),
            ModelMessage::Assistant(format!(r#"{{"summary":"{answer}"}}"#)),
        ]
    }

    async fn active_turn_owner(
        database: &Database,
        session_id: &str,
        turn_position: i64,
    ) -> TurnOwner {
        let token = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT owner_token FROM session_turn WHERE session_id = ? AND turn_position = ?",
        )
        .bind(session_id)
        .bind(turn_position)
        .fetch_one(&database.pool)
        .await
        .expect("owner token should load");

        TurnOwner {
            database: database.identity.clone(),
            interruption_error_type: "interrupted",
            session_id: session_id.to_string(),
            token,
            turn_position,
        }
    }

    type HistoricalMessage = (i64, i64, &'static str, &'static str, i64, i64);

    const MIGRATION_ONE_SNAPSHOT: &str = r"CREATE TABLE session (
    id TEXT PRIMARY KEY NOT NULL,
    provider TEXT,
    model TEXT,
    output_schema TEXT NOT NULL,
    system_prompt TEXT,
    max_history_bytes INTEGER NOT NULL CHECK (max_history_bytes > 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (
        (provider IS NULL AND model IS NULL)
        OR (provider IS NOT NULL AND model IS NOT NULL)
    )
);

CREATE TABLE session_message (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    turn_position INTEGER NOT NULL,
    message_position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    retained_bytes INTEGER NOT NULL CHECK (retained_bytes >= 0),
    created_at INTEGER NOT NULL,
    UNIQUE (session_id, turn_position, message_position)
);

CREATE INDEX session_message_session_id_turn_position_idx
ON session_message (session_id, turn_position);
";

    async fn create_version_one_database(path: &Path) -> [HistoricalMessage; 4] {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(connect_options(path))
            .await
            .expect("version-one database should open");
        Migrator::with_migrations(vec![Migration::new(
            1,
            "create session".into(),
            MigrationType::Simple,
            MIGRATION_ONE_SNAPSHOT.into_sql_str(),
            false,
        )])
        .run(&pool)
        .await
        .expect("frozen migration one should apply");
        sqlx::query(
            r"
INSERT INTO session (
    id, provider, model, output_schema, system_prompt, max_history_bytes, created_at, updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
",
        )
        .bind("session-a")
        .bind("provider")
        .bind("model")
        .bind(schema().value().to_string())
        .bind("persistent instructions")
        .bind(100_000_i64)
        .bind(5_i64)
        .bind(30_i64)
        .execute(&pool)
        .await
        .expect("historical session should be inserted");
        let historical_messages = [
            (3_i64, 0_i64, "user", r#""first""#, 5_i64, 10_i64),
            (3, 1, "assistant", r#""{\"summary\":\"one\"}""#, 17, 11),
            (8, 0, "user", r#""second""#, 6, 20),
            (8, 1, "assistant", r#""{\"summary\":\"two\"}""#, 17, 21),
        ];
        for (turn_position, message_position, kind, payload, retained_bytes, created_at) in
            historical_messages
        {
            sqlx::query(
                r"
INSERT INTO session_message (
    session_id, turn_position, message_position, kind, payload, retained_bytes, created_at
)
VALUES (?, ?, ?, ?, ?, ?, ?)
",
            )
            .bind("session-a")
            .bind(turn_position)
            .bind(message_position)
            .bind(kind)
            .bind(payload)
            .bind(retained_bytes)
            .bind(created_at)
            .execute(&pool)
            .await
            .expect("historical message should be inserted");
        }
        pool.close().await;

        historical_messages
    }

    #[test]
    fn session_config_exposes_values_and_system_prompt() {
        // Arrange
        let schema = schema();

        // Act
        let config = NewSession::new("session-a", schema.clone())
            .with_system_prompt("persistent instructions");

        // Assert
        assert_eq!(config.id(), "session-a");
        assert_eq!(config.schema(), &schema);
        assert_eq!(config.system_prompt(), Some("persistent instructions"));
    }

    #[test]
    fn timestamp_source_closure_returns_injected_value() {
        // Arrange
        let timestamp_source = || 123;

        // Act
        let timestamp = timestamp_source.now_timestamp_seconds();

        // Assert
        assert_eq!(timestamp, 123);
    }

    #[tokio::test]
    async fn on_disk_database_creates_parent_and_applies_connection_policy() {
        // Arrange
        let temp_dir = tempdir().expect("temp directory should be created");
        let database_path = temp_dir.path().join("nested/harness.db");
        let timestamp_source: Arc<dyn TimestampSource> = Arc::new(|| 123);

        // Act
        let database = Database::open_with_timestamp_source(&database_path, timestamp_source)
            .await
            .expect("database should open");
        let journal_mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&database.pool)
            .await
            .expect("journal mode should load");
        let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&database.pool)
            .await
            .expect("foreign key setting should load");
        let synchronous = sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
            .fetch_one(&database.pool)
            .await
            .expect("synchronous setting should load");

        // Assert
        assert!(database_path.exists());
        assert_eq!(journal_mode, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(synchronous, 1);
    }

    #[tokio::test]
    async fn owner_token_migration_preserves_existing_turns() {
        // Arrange
        let temp_dir = tempdir().expect("temporary directory should be created");
        let database_path = temp_dir.path().join("harness.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(connect_options(&database_path))
            .await
            .expect("legacy database should open");
        sqlx::migrate!("./migrations")
            .run_to(2, &pool)
            .await
            .expect("legacy migrations should run");
        sqlx::query(
            r"
INSERT INTO session (id, output_schema, max_history_bytes, created_at, updated_at)
VALUES ('session-a', '{}', 100000, 10, 10)
",
        )
        .execute(&pool)
        .await
        .expect("legacy session should be inserted");
        sqlx::query(
            r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at
)
VALUES ('session-a', 0, 'running', NULL, 310, 10, 10)
",
        )
        .execute(&pool)
        .await
        .expect("legacy turn should be inserted");
        pool.close().await;

        // Act
        let database = Database::open(&database_path)
            .await
            .expect("upgraded database should open");
        let turn = sqlx::query_as::<_, (String, Option<Vec<u8>>)>(
            "SELECT status, owner_token FROM session_turn WHERE session_id = 'session-a'",
        )
        .fetch_one(&database.pool)
        .await
        .expect("upgraded turn should load");

        // Assert
        assert_eq!(turn, ("running".to_string(), None));
    }

    #[tokio::test]
    async fn on_disk_database_supports_sqlite_temporary_paths_without_a_parent() {
        // Arrange
        let database_path = Path::new("");

        // Act
        let result = Database::open(database_path).await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn default_database_constructors_use_system_timestamps() {
        // Arrange
        let temp_dir = tempdir().expect("temp directory should be created");
        let database_path = temp_dir.path().join("harness.db");

        // Act
        let on_disk = Database::open(&database_path)
            .await
            .expect("on-disk database should open");
        let in_memory = Database::open_in_memory()
            .await
            .expect("in-memory database should open");

        // Assert
        assert!(on_disk.timestamp_source.now_timestamp_seconds() > 0);
        assert!(in_memory.timestamp_source.now_timestamp_seconds() > 0);
    }

    #[tokio::test]
    async fn migration_two_backfills_historical_turn_lifecycle_without_data_loss() {
        // Arrange
        let temp_dir = tempdir().expect("temporary directory should be created");
        let database_path = temp_dir.path().join("harness.db");
        let historical_messages = create_version_one_database(&database_path).await;

        // Act
        let database = Database::open(&database_path)
            .await
            .expect("database should upgrade to migration two");
        let loaded = database
            .load_session("session-a")
            .await
            .expect("upgraded session should load");
        let session_row = sqlx::query_as::<_, (i64, i64, Option<String>)>(
            "SELECT created_at, updated_at, provider_session_id FROM session WHERE id = ?",
        )
        .bind("session-a")
        .fetch_one(database.pool())
        .await
        .expect("upgraded session row should load");
        let turns = sqlx::query_as::<_, (i64, String, Option<String>, Option<i64>, i64, i64)>(
            r"
SELECT turn_position, status, error_type, lease_expires_at, created_at, updated_at
FROM session_turn
WHERE session_id = ?
ORDER BY turn_position
",
        )
        .bind("session-a")
        .fetch_all(database.pool())
        .await
        .expect("backfilled turns should load");
        let messages = sqlx::query_as::<_, (i64, i64, String, String, i64, i64)>(
            r"
SELECT turn_position, message_position, kind, payload, retained_bytes, created_at
FROM session_message
WHERE session_id = ?
ORDER BY turn_position, message_position
",
        )
        .bind("session-a")
        .fetch_all(database.pool())
        .await
        .expect("historical messages should load");

        // Assert
        assert_eq!(session_row, (5, 30, None));
        assert_eq!(
            turns,
            vec![
                (3, "completed".to_string(), None, None, 10, 11),
                (8, "completed".to_string(), None, None, 20, 21),
            ]
        );
        assert_eq!(
            messages,
            historical_messages
                .into_iter()
                .map(
                    |(
                        turn_position,
                        message_position,
                        kind,
                        payload,
                        retained_bytes,
                        created_at,
                    )| {
                        (
                            turn_position,
                            message_position,
                            kind.to_string(),
                            payload.to_string(),
                            retained_bytes,
                            created_at,
                        )
                    },
                )
                .collect::<Vec<_>>()
        );
        assert_eq!(
            loaded.turns,
            vec![turn("first", "one"), turn("second", "two")]
        );
    }

    #[tokio::test]
    async fn database_round_trips_every_persistent_message_kind() {
        // Arrange
        let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 456))
            .await
            .expect("database should open");
        let config =
            NewSession::new("session-a", schema()).with_system_prompt("persistent instructions");
        let metadata = ModelMetadata::new("provider", "model").expect("metadata should be valid");
        database
            .create_session(&config, Some(metadata), 100_000)
            .await
            .expect("session should be created");
        let messages = vec![
            ModelMessage::User("inspect and edit".to_string()),
            ModelMessage::AssistantReasoning {
                content: r#"{"summary":"thinking"}"#.to_string(),
                reasoning_content: "I should inspect before editing.".to_string(),
            },
            ModelMessage::AssistantToolCall(read_call("read-one")),
            ModelMessage::ToolResult {
                call_id: "read-one".to_string(),
                content: "manifest".to_string(),
                name: "read".to_string(),
            },
            ModelMessage::AssistantToolCalls(vec![read_call("read-two"), write_call("write-one")]),
            ModelMessage::ToolResult {
                call_id: "read-two".to_string(),
                content: "source".to_string(),
                name: "read".to_string(),
            },
            ModelMessage::ToolResult {
                call_id: "write-one".to_string(),
                content: "written".to_string(),
                name: "write".to_string(),
            },
            ModelMessage::Assistant(r#"{"summary":"done"}"#.to_string()),
        ];

        // Act
        database
            .append_turn("session-a", &messages)
            .await
            .expect("turn should be appended");
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");
        let timestamps = sqlx::query_as!(
            SessionTimestampsRow,
            r#"
SELECT session.created_at AS "session_created_at!: i64",
       session.updated_at AS "session_updated_at!: i64",
       session_message.created_at AS "message_timestamp!: i64"
FROM session
INNER JOIN session_message ON session_message.session_id = session.id
WHERE session.id = ?
LIMIT 1
"#,
            "session-a"
        )
        .fetch_one(&database.pool)
        .await
        .expect("timestamps should load");

        // Assert
        assert_eq!(loaded.provider.as_deref(), Some("provider"));
        assert_eq!(loaded.model.as_deref(), Some("model"));
        assert_eq!(loaded.schema, schema());
        assert_eq!(
            loaded.system_prompt.as_deref(),
            Some("persistent instructions")
        );
        assert_eq!(loaded.turns, vec![messages]);
        assert_eq!(timestamps.message_timestamp, 456);
        assert_eq!(timestamps.session_created_at, 456);
        assert_eq!(timestamps.session_updated_at, 456);
    }

    #[tokio::test]
    async fn session_info_loads_model_identity_and_reports_missing_sessions() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let database = Database::open(&database_path)
            .await
            .expect("database should open");
        let metadata = ModelMetadata::new("provider", "model").expect("metadata should be valid");
        database
            .create_session(
                &NewSession::new("session-a", schema()),
                Some(metadata),
                100_000,
            )
            .await
            .expect("session should be created");

        // Act
        let info = SessionInfo::load(&database_path, "session-a")
            .await
            .expect("session identity should load");
        let missing = SessionInfo::load(&database_path, "missing")
            .await
            .expect_err("missing session should fail");

        // Assert
        assert_eq!(info.provider(), Some("provider"));
        assert_eq!(info.model(), Some("model"));
        assert!(matches!(missing, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn beginning_a_turn_for_a_missing_session_reports_not_found() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");

        // Act
        let error = database
            .begin_turn("missing", "prompt")
            .await
            .err()
            .expect("missing session should fail");

        // Assert
        assert!(matches!(error, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn beginning_a_turn_preserves_non_unique_database_failures() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        sqlx::query(
            r"
CREATE TRIGGER reject_session_turn
BEFORE INSERT ON session_turn
BEGIN
    SELECT RAISE(ABORT, 'turn rejected');
END
",
        )
        .execute(&database.pool)
        .await
        .expect("trigger should be created");

        // Act
        let error = database
            .begin_turn("session-a", "prompt")
            .await
            .err()
            .expect("database failure should be preserved");

        // Assert
        assert!(matches!(error, SessionError::QueryContext { .. }));
    }

    #[tokio::test]
    async fn beginning_a_turn_does_not_reserve_when_history_loading_fails() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");
        sqlx::query("UPDATE session_message SET kind = 'unknown' WHERE session_id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("history should be corrupted");

        // Act
        let error = database
            .begin_turn("session-a", "new prompt")
            .await
            .err()
            .expect("invalid history should fail acquisition");
        let active_turns = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_turn WHERE status IN ('pending', 'running')",
        )
        .fetch_one(&database.pool)
        .await
        .expect("active turn count should load");
        let message_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_message WHERE session_id = 'session-a'",
        )
        .fetch_one(&database.pool)
        .await
        .expect("message count should load");

        // Assert
        assert!(matches!(error, SessionError::InvalidData { .. }));
        assert_eq!(active_turns, 0);
        assert_eq!(message_count, 2);
    }

    #[tokio::test]
    async fn beginning_a_turn_calculates_the_lease_when_reserving() {
        // Arrange
        let now = Arc::new(AtomicI64::new(10));
        let timestamp_source: Arc<dyn TimestampSource> = {
            let now = Arc::clone(&now);

            Arc::new(move || now.fetch_add(10, Ordering::SeqCst))
        };
        let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");

        // Act
        let acquired = database
            .begin_turn("session-a", "prompt")
            .await
            .expect("turn should begin");
        let row = sqlx::query_as::<_, (String, i64, i64, i64)>(
            r"
SELECT status, lease_expires_at, created_at, updated_at
FROM session_turn
WHERE session_id = ? AND turn_position = ?
",
        )
        .bind("session-a")
        .bind(acquired.turn_position)
        .fetch_one(&database.pool)
        .await
        .expect("turn lifecycle should load");

        // Assert
        assert_eq!(row, ("running".to_string(), 330, 30, 30));
    }

    #[tokio::test]
    async fn reserving_a_turn_rejects_a_stale_acquisition_snapshot() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let acquisition = database
            .load_turn_acquisition("session-a")
            .await
            .expect("turn acquisition should load");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");
        let message = EncodedMessage::from_message(&ModelMessage::User("prompt".to_string()))
            .expect("message should encode");

        // Act
        let reserved = database
            .reserve_turn("session-a", &message, &acquisition)
            .await
            .expect("reservation should be checked");
        let active_turns = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_turn WHERE status IN ('pending', 'running')",
        )
        .fetch_one(&database.pool)
        .await
        .expect("active turn count should load");

        // Assert
        assert!(reserved.is_none());
        assert_eq!(active_turns, 0);
    }

    #[tokio::test]
    async fn reserving_a_turn_for_a_removed_session_reports_not_found() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let acquisition = database
            .load_turn_acquisition("session-a")
            .await
            .expect("turn acquisition should load");
        sqlx::query("DELETE FROM session WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("session should be removed");
        let message = EncodedMessage::from_message(&ModelMessage::User("prompt".to_string()))
            .expect("message should encode");

        // Act
        let result = database
            .reserve_turn("session-a", &message, &acquisition)
            .await;

        // Assert
        assert!(matches!(result, Err(SessionError::NotFound { .. })));
    }

    #[tokio::test]
    async fn cancelling_turn_acquisition_leaves_no_active_turn() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database = Database::open(&directory.path().join("harness.db"))
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let mut blocker = database
            .pool
            .begin()
            .await
            .expect("blocking transaction should begin");
        sqlx::query("UPDATE session SET updated_at = updated_at WHERE id = 'session-a'")
            .execute(&mut *blocker)
            .await
            .expect("blocking transaction should hold the writer lock");

        // Act
        let cancellation = tokio::time::timeout(
            Duration::from_millis(50),
            database.begin_turn("session-a", "cancelled"),
        )
        .await;
        blocker
            .rollback()
            .await
            .expect("blocking transaction should roll back");
        let acquired = database
            .begin_turn("session-a", "replacement")
            .await
            .expect("replacement turn should begin immediately");

        // Assert
        assert!(cancellation.is_err());
        assert_eq!(acquired.turn_position, 0);
    }

    #[tokio::test]
    async fn cancelling_after_commit_recovers_the_owned_turn_immediately() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let mut database = Database::open(&database_path)
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let mut replacement_database = Database::open(&database_path)
            .await
            .expect("replacement database should open");
        let commit_control = Arc::new(ReservationCommitControl::paused());
        database.reservation_commit_control = Some(Arc::clone(&commit_control));
        replacement_database.reservation_commit_control = Some(Arc::clone(&commit_control));

        // Act
        let cancellation = tokio::time::timeout(
            Duration::from_secs(1),
            database.begin_turn("session-a", "cancelled"),
        )
        .await;
        let commit_seen = commit_control.commit_seen.load(Ordering::SeqCst);
        let acquired = replacement_database
            .begin_turn("session-a", "replacement")
            .await
            .expect("replacement turn should begin immediately");
        let turns = sqlx::query_as::<_, (i64, String)>(
            r"
SELECT turn_position, status
FROM session_turn
WHERE session_id = 'session-a'
ORDER BY turn_position
",
        )
        .fetch_all(&replacement_database.pool)
        .await
        .expect("turn states should load");

        // Assert
        assert!(cancellation.is_err());
        assert!(commit_seen);
        assert_eq!(acquired.turn_position, 1);
        assert_eq!(
            turns,
            vec![(0, "interrupted".to_string()), (1, "running".to_string())]
        );
    }

    #[tokio::test]
    async fn registered_cancelled_owner_preserves_its_reason_during_recovery() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let abandoned = database
            .begin_turn("session-a", "abandoned")
            .await
            .expect("turn should begin");
        let mut owner = active_turn_owner(&database, "session-a", abandoned.turn_position).await;
        owner.interruption_error_type = "cancelled";
        database.abandoned_turns.register(owner);

        // Act
        let replacement = database
            .begin_turn("session-a", "replacement")
            .await
            .expect("replacement turn should begin");
        let turns = sqlx::query_as::<_, (i64, String, Option<String>)>(
            r"
SELECT turn_position, status, error_type
FROM session_turn
WHERE session_id = 'session-a'
ORDER BY turn_position
",
        )
        .fetch_all(&database.pool)
        .await
        .expect("turn states should load");

        // Assert
        assert_eq!(replacement.turn_position, 1);
        assert_eq!(
            turns,
            vec![
                (0, "interrupted".to_string(), Some("cancelled".to_string())),
                (1, "running".to_string(), None),
            ]
        );
    }

    #[tokio::test]
    async fn stopped_ownership_monitor_reports_ownership_loss() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let mut acquired = database
            .begin_turn("session-a", "prompt")
            .await
            .expect("turn should begin");
        acquired
            .guard
            .renewal_task
            .take()
            .expect("ownership monitor should be running")
            .abort();

        // Act
        let error = acquired.guard.ownership_failure().await;
        let repeated_error = acquired.guard.ownership_failure().await;

        // Assert
        assert!(matches!(
            error,
            SessionError::OwnershipLost {
                ref id,
                turn_position: 0,
            } if id == "session-a"
        ));
        assert!(matches!(
            repeated_error,
            SessionError::OwnershipLost {
                ref id,
                turn_position: 0,
            } if id == "session-a"
        ));
    }

    #[tokio::test]
    async fn abandoned_owners_are_scoped_to_their_database() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let first_database = Database::open(&directory.path().join("first.db"))
            .await
            .expect("first database should open");
        let second_database = Database::open(&directory.path().join("second.db"))
            .await
            .expect("second database should open");
        for database in [&first_database, &second_database] {
            database
                .create_session(&NewSession::new("session-a", schema()), None, 100_000)
                .await
                .expect("session should be created");
        }
        let first_turn = first_database
            .begin_turn("session-a", "first abandoned")
            .await
            .expect("first turn should begin");
        let second_turn = second_database
            .begin_turn("session-a", "second abandoned")
            .await
            .expect("second turn should begin");
        let first_owner =
            active_turn_owner(&first_database, "session-a", first_turn.turn_position).await;
        let second_owner =
            active_turn_owner(&second_database, "session-a", second_turn.turn_position).await;
        first_database.abandoned_turns.register(first_owner);
        second_database.abandoned_turns.register(second_owner);

        // Act
        let second_replacement = second_database
            .begin_turn("session-a", "second replacement")
            .await
            .expect("second replacement should begin");
        let first_owners = first_database
            .abandoned_turns
            .for_session(&first_database.identity, "session-a");
        let first_replacement = first_database
            .begin_turn("session-a", "first replacement")
            .await
            .expect("first replacement should begin");

        // Assert
        assert_eq!(second_replacement.turn_position, 1);
        assert_eq!(first_owners.len(), 1);
        assert_eq!(first_replacement.turn_position, 1);
    }

    #[tokio::test]
    async fn failing_or_completing_a_turn_that_is_not_running_reports_invalid_data() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let turn_position = database
            .begin_turn("session-a", "prompt")
            .await
            .expect("turn should begin")
            .turn_position;
        database
            .fail_turn(
                "session-a",
                turn_position,
                &TurnError::Model(crate::ModelError::InvalidResponse),
            )
            .await
            .expect("turn should fail");

        // Act
        let error = database
            .complete_turn("session-a", turn_position, &[], None)
            .await
            .expect_err("non-running turn should fail");
        let repeated_failure = database
            .fail_turn(
                "session-a",
                turn_position,
                &TurnError::Model(crate::ModelError::InvalidResponse),
            )
            .await
            .expect_err("repeated turn failure should fail");

        // Assert
        assert!(matches!(error, SessionError::InvalidData { .. }));
        assert!(matches!(repeated_failure, SessionError::InvalidData { .. }));
    }

    #[tokio::test]
    async fn database_recovers_expired_active_turns_as_interrupted() {
        // Arrange
        let now = Arc::new(AtomicI64::new(10));
        let timestamp_source: Arc<dyn TimestampSource> = {
            let now = Arc::clone(&now);

            Arc::new(move || now.load(Ordering::SeqCst))
        };
        let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let abandoned = database
            .begin_turn("session-a", "abandoned")
            .await
            .expect("turn should begin")
            .turn_position;
        now.store(10 + TURN_LEASE_SECONDS + 1, Ordering::SeqCst);

        // Act
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");
        let replacement = database
            .begin_turn("session-a", "replacement")
            .await
            .expect("replacement turn should begin")
            .turn_position;
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM session_turn WHERE session_id = ? AND turn_position = ?",
        )
        .bind("session-a")
        .bind(abandoned)
        .fetch_one(&database.pool)
        .await
        .expect("turn status should load");

        // Assert
        assert_eq!(loaded.turns, [] as [Vec<ModelMessage>; 0]);
        assert_eq!(status, "interrupted");
        assert_eq!(replacement, abandoned + 1);
    }

    #[tokio::test]
    async fn database_loads_only_newest_complete_turns_within_budget() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let older_turn = turn("older question", "older answer");
        let latest_turn = turn("latest question", "latest answer");
        let latest_bytes = latest_turn.iter().map(ModelMessage::retained_bytes).sum();
        database
            .create_session(&NewSession::new("session-a", schema()), None, latest_bytes)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &older_turn)
            .await
            .expect("older turn should be appended");
        database
            .append_turn("session-a", &latest_turn)
            .await
            .expect("latest turn should be appended");

        // Act
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");

        // Assert
        assert_eq!(loaded.turns, vec![latest_turn]);
    }

    #[tokio::test]
    async fn database_paginates_turn_sizes_until_the_history_budget_is_filled() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let turns = (0..70)
            .map(|position| {
                turn(
                    &format!("question {position}"),
                    &format!("answer {position}"),
                )
            })
            .collect::<Vec<_>>();
        let expected_turns = turns[5..].to_vec();
        let max_history_bytes = expected_turns
            .iter()
            .flatten()
            .map(ModelMessage::retained_bytes)
            .sum();
        database
            .create_session(
                &NewSession::new("session-a", schema()),
                None,
                max_history_bytes,
            )
            .await
            .expect("session should be created");
        for messages in &turns {
            database
                .append_turn("session-a", messages)
                .await
                .expect("turn should be appended");
        }

        // Act
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");

        // Assert
        assert_eq!(loaded.turns, expected_turns);
    }

    #[tokio::test]
    async fn history_size_page_preserves_integer_boundary_turns() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        for (turn_position, retained_bytes) in [(i64::MIN, 1_i64), (i64::MAX, 2_i64)] {
            sqlx::query(
                r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at
)
VALUES (?, ?, 'completed', NULL, NULL, 1, 1)
",
            )
            .bind("session-a")
            .bind(turn_position)
            .execute(&database.pool)
            .await
            .expect("boundary turn should be inserted");
            sqlx::query(
                r#"
INSERT INTO session_message (
    session_id, turn_position, message_position, kind, payload, retained_bytes, created_at
)
VALUES (?, ?, 0, 'user', '"boundary"', ?, 1)
"#,
            )
            .bind("session-a")
            .bind(turn_position)
            .bind(retained_bytes)
            .execute(&database.pool)
            .await
            .expect("boundary message should be inserted");
        }

        // Act
        let mut connection = database
            .pool
            .acquire()
            .await
            .expect("database connection should be acquired");
        let initial_page = load_turn_size_page(&mut connection, "session-a", None)
            .await
            .expect("initial page should load");
        let before_maximum = load_turn_size_page(&mut connection, "session-a", Some(i64::MAX))
            .await
            .expect("page before maximum should load");
        let before_minimum = load_turn_size_page(&mut connection, "session-a", Some(i64::MIN))
            .await
            .expect("page before minimum should load");

        // Assert
        assert_eq!(initial_page, [(i64::MAX, 2), (i64::MIN, 1)]);
        assert_eq!(before_maximum, [(i64::MIN, 1)]);
        assert_eq!(before_minimum, [] as [(i64, i64); 0]);
    }

    #[tokio::test]
    async fn database_excludes_a_newest_turn_larger_than_the_budget() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 1)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");

        // Act
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");

        // Assert
        assert_eq!(loaded.turns, Vec::<Vec<ModelMessage>>::new());
    }

    #[tokio::test]
    async fn append_turn_rolls_back_every_message_when_one_insert_fails() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        sqlx::query(
            r"
CREATE TRIGGER reject_assistant_message
BEFORE INSERT ON session_message
WHEN NEW.kind = 'assistant'
BEGIN
    SELECT RAISE(ABORT, 'assistant rejected');
END
",
        )
        .execute(&database.pool)
        .await
        .expect("trigger should be created");

        // Act
        let error = database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect_err("turn append should fail");
        let message_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_message WHERE session_id = ?",
        )
        .bind("session-a")
        .fetch_one(&database.pool)
        .await
        .expect("message count should load");

        // Assert
        assert!(matches!(error, SessionError::QueryContext { .. }));
        assert_eq!(message_count, 0);
    }

    #[tokio::test]
    async fn on_disk_database_serializes_concurrent_appends_before_reading_positions() {
        // Arrange
        let temp_dir = tempdir().expect("temp directory should be created");
        let database_path = temp_dir.path().join("harness.db");
        let next_timestamp = Arc::new(AtomicI64::new(1));
        let timestamp_source: Arc<dyn TimestampSource> = {
            let next_timestamp = Arc::clone(&next_timestamp);

            Arc::new(move || next_timestamp.fetch_add(1, Ordering::SeqCst))
        };
        let database = Database::open_with_timestamp_source(&database_path, timestamp_source)
            .await
            .expect("database should open");
        for session_id in ["session-a", "session-b"] {
            database
                .create_session(&NewSession::new(session_id, schema()), None, 100_000)
                .await
                .expect("session should be created");
        }
        sqlx::query(
            r"
CREATE TRIGGER require_session_update_before_message
BEFORE INSERT ON session_message
WHEN (
    SELECT updated_at
    FROM session
    WHERE id = NEW.session_id
) != NEW.created_at
BEGIN
    SELECT RAISE(ABORT, 'session update must acquire the writer lock first');
END
",
        )
        .execute(&database.pool)
        .await
        .expect("ordering trigger should be created");
        let first_turn = turn("first question", "first answer");
        let second_turn = turn("second question", "second answer");

        // Act
        let (first_result, second_result) = tokio::join!(
            database.append_turn("session-a", &first_turn),
            database.append_turn("session-b", &second_turn),
        );

        // Assert
        first_result.expect("first concurrent append should succeed");
        second_result.expect("second concurrent append should succeed");
        let message_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_message")
            .fetch_one(&database.pool)
            .await
            .expect("message count should load");
        assert_eq!(message_count, 4);
    }

    #[tokio::test]
    async fn appending_an_empty_turn_to_a_missing_session_reports_not_found() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");

        // Act
        let error = database
            .append_turn("missing", &[])
            .await
            .expect_err("missing session should fail");

        // Assert
        assert!(matches!(error, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn database_reports_creation_and_loading_errors() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let config = NewSession::new("session-a", schema());
        database
            .create_session(&config, None, 100)
            .await
            .expect("session should be created");

        // Act
        let duplicate = database
            .create_session(&config, None, 100)
            .await
            .expect_err("duplicate should fail");
        let empty = database
            .create_session(&NewSession::new(" ", schema()), None, 100)
            .await
            .expect_err("empty identifier should fail");
        let oversized_limit = database
            .create_session(&NewSession::new("session-b", schema()), None, usize::MAX)
            .await
            .expect_err("oversized limit should fail");
        let missing = database
            .load_session("missing")
            .await
            .err()
            .expect("missing session should fail");

        // Assert
        assert!(matches!(duplicate, SessionError::AlreadyExists { .. }));
        assert!(matches!(empty, SessionError::InvalidData { .. }));
        assert!(matches!(oversized_limit, SessionError::InvalidData { .. }));
        assert!(matches!(missing, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn database_rejects_invalid_persisted_schema_and_message_data() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");
        sqlx::query("UPDATE session SET output_schema = 'not-json' WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("schema should be corrupted");

        // Act
        let invalid_schema = database
            .load_session("session-a")
            .await
            .err()
            .expect("invalid schema should fail");
        sqlx::query("UPDATE session SET output_schema = '{}' WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("schema should be repaired");
        sqlx::query("UPDATE session_message SET kind = 'unknown' WHERE session_id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("message kind should be corrupted");
        let invalid_message = database
            .load_session("session-a")
            .await
            .err()
            .expect("invalid message should fail");

        // Assert
        assert!(matches!(invalid_schema, SessionError::InvalidData { .. }));
        assert!(matches!(invalid_message, SessionError::InvalidData { .. }));
    }

    #[tokio::test]
    async fn database_rejects_negative_persisted_byte_counts() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&database.pool)
            .await
            .expect("constraints should be disabled for corruption fixture");
        sqlx::query("UPDATE session SET max_history_bytes = -1 WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("history byte limit should be corrupted");

        // Act
        let invalid_limit = database
            .load_session("session-a")
            .await
            .err()
            .expect("negative history byte limit should fail");
        sqlx::query("UPDATE session SET max_history_bytes = 100000 WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("history byte limit should be repaired");
        sqlx::query(
            "UPDATE session_message SET retained_bytes = -1 WHERE session_id = 'session-a'",
        )
        .execute(&database.pool)
        .await
        .expect("retained byte count should be corrupted");
        let invalid_message_size = database
            .load_session("session-a")
            .await
            .err()
            .expect("negative retained byte count should fail");

        // Assert
        assert!(matches!(invalid_limit, SessionError::InvalidData { .. }));
        assert!(matches!(
            invalid_message_size,
            SessionError::InvalidData { .. }
        ));
    }

    #[test]
    fn message_decoder_rejects_invalid_json_arguments_and_unknown_tools() {
        // Arrange
        let invalid_json = "{";
        let invalid_read =
            r#"{"arguments":{"bogus":true},"id":"call","name":"read","reasoning_content":null}"#;
        let unknown_tool = r#"{"arguments":{},"id":"call","name":"bash","reasoning_content":null}"#;

        // Act
        let malformed = EncodedMessage::into_message("user", invalid_json);
        let invalid_arguments = EncodedMessage::into_message("assistant_tool_call", invalid_read);
        let unknown = EncodedMessage::into_message("assistant_tool_call", unknown_tool);
        let system = EncodedMessage::from_message(&ModelMessage::System("system".to_string()));

        // Assert
        assert!(matches!(malformed, Err(SessionError::InvalidData { .. })));
        assert!(matches!(
            invalid_arguments,
            Err(SessionError::InvalidData { .. })
        ));
        assert!(matches!(unknown, Err(SessionError::InvalidData { .. })));
        assert!(matches!(system, Err(SessionError::InvalidData { .. })));
    }

    #[tokio::test]
    async fn persistent_chat_restores_completed_history_and_system_prompt() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            let expected = if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ModelMessage::System("persistent instructions".to_string()),
                    ModelMessage::User("first".to_string()),
                ]
            } else {
                vec![
                    ModelMessage::System("persistent instructions".to_string()),
                    ModelMessage::User("first".to_string()),
                    ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                    ModelMessage::User("second".to_string()),
                ]
            };
            assert_eq!(request.messages(), expected);

            Ok(crate::ModelCompletion::from_response(
                ModelResponse::Output(json!({
                    "summary": if expected.len() == 2 { "one" } else { "two" }
                })),
            ))
        });
        let harness = crate::Harness::new(model).database(&database_path);
        let mut session = harness
            .session("session-a", schema())
            .system_prompt("persistent instructions")
            .create()
            .await
            .expect("session should be created");

        // Act
        let first = session
            .send("first")
            .await
            .expect("first turn should succeed");
        drop(session);
        let mut resumed = harness
            .resume("session-a")
            .await
            .expect("session should reopen");
        let second = resumed
            .send("second")
            .await
            .expect("second turn should succeed");

        // Assert
        assert_eq!(resumed.id(), "session-a");
        assert_eq!(first.output(), &json!({ "summary": "one" }));
        assert_eq!(second.output(), &json!({ "summary": "two" }));
    }

    #[tokio::test]
    async fn persistent_chat_does_not_store_failed_turns() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let mut model = model();
        model
            .expect_complete()
            .times(1)
            .returning(|_| Err(crate::ModelError::InvalidResponse));
        let harness = crate::Harness::new(model).database(&database_path);
        let mut session = harness
            .session("session-a", schema())
            .create()
            .await
            .expect("session should be created");

        // Act
        let error = session.send("failed").await.expect_err("turn should fail");
        let database = Database::open(&database_path)
            .await
            .expect("database should open");
        let row = sqlx::query_as!(
            TurnStatusRow,
            r#"
SELECT COUNT(message.id) AS "message_count!: i64",
       turn.status AS "status!: String"
FROM session_turn AS turn
LEFT JOIN session_message AS message
  ON message.session_id = turn.session_id
 AND message.turn_position = turn.turn_position
WHERE turn.session_id = ?
GROUP BY turn.status
"#,
            "session-a"
        )
        .fetch_one(&database.pool)
        .await
        .expect("message count should load");

        // Assert
        assert!(matches!(error, SessionError::Turn(_)));
        assert_eq!(row.message_count, 1);
        assert_eq!(row.status, "failed");
    }

    #[tokio::test]
    async fn opening_session_validates_saved_model_identity() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let original =
            crate::Harness::new(metadata_model("provider-a", "model-a")).database(&database_path);
        original
            .session("session-a", schema())
            .create()
            .await
            .expect("session should be created");
        let different =
            crate::Harness::new(metadata_model("provider-b", "model-b")).database(&database_path);

        // Act
        let mismatch = different
            .resume("session-a")
            .await
            .err()
            .expect("model mismatch should fail");
        let missing = different
            .resume("missing")
            .await
            .err()
            .expect("missing session should fail");

        // Assert
        assert!(matches!(mismatch, SessionError::ModelMismatch { .. }));
        assert!(matches!(missing, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn opening_session_accepts_matching_model_identity() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let original =
            crate::Harness::new(metadata_model("provider", "model")).database(&database_path);
        original
            .session("session-a", schema())
            .create()
            .await
            .expect("session should be created");
        let matching =
            crate::Harness::new(metadata_model("provider", "model")).database(&database_path);

        // Act
        let session = matching
            .resume("session-a")
            .await
            .expect("matching session should open");

        // Assert
        assert_eq!(session.id(), "session-a");
    }

    #[tokio::test]
    async fn opening_session_rejects_incomplete_saved_model_identity() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let database = Database::open(&database_path)
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        let mut connection = database
            .pool
            .acquire()
            .await
            .expect("connection should open");
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut *connection)
            .await
            .expect("constraints should be disabled for corruption fixture");
        sqlx::query("UPDATE session SET provider = 'provider' WHERE id = 'session-a'")
            .execute(&mut *connection)
            .await
            .expect("model identity should be corrupted");
        drop(connection);
        let harness = crate::Harness::new(model()).database(&database_path);

        // Act
        let error = harness
            .resume("session-a")
            .await
            .err()
            .expect("incomplete identity should fail");

        // Assert
        assert!(matches!(error, SessionError::InvalidData { .. }));
    }

    #[tokio::test]
    async fn persistent_chat_uses_saved_history_budget_when_reopened() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let harness = crate::Harness::new(model())
            .database(&database_path)
            .max_history_bytes(NonZeroUsize::new(64).expect("history limit should be nonzero"));
        let session = harness
            .session("session-a", schema())
            .create()
            .await
            .expect("session should be created");
        drop(session);

        // Act
        let _reopened = harness
            .resume("session-a")
            .await
            .expect("session should reopen");

        // Assert
        let loaded = Database::open(&database_path)
            .await
            .expect("database should open")
            .load_session("session-a")
            .await
            .expect("session should load");
        assert_eq!(loaded.max_history_bytes, 64);
    }
}
