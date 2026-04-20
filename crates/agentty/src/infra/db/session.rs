//! Session-scoped persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use super::review::SessionReviewRequestRow;
use crate::domain::agent::ReasoningLevel;
use crate::domain::session::SessionStats;
use crate::infra::agent;
use crate::infra::db::DbError;

/// Transactional turn-metadata payload persisted after one completed agent
/// turn.
pub(crate) struct SessionTurnMetadata<'a> {
    /// Session-scoped instruction bootstrap marker for app-server providers.
    pub(crate) instruction_conversation_id: Option<&'a str>,
    /// Model identifier used for per-model usage aggregation.
    pub(crate) model: &'a str,
    /// Persisted provider-native conversation identifier for future resumes.
    pub(crate) provider_conversation_id: Option<&'a str>,
    /// Serialized clarification-question payload stored on the session row.
    pub(crate) questions_json: &'a str,
    /// Serialized structured summary payload stored on the session row.
    pub(crate) summary: &'a str,
    /// Token-usage delta attributed to the completed turn.
    pub(crate) token_usage_delta: &'a SessionStats,
}

/// Row returned when loading a session from the `session` table.
///
/// Includes optional normalized forge review-request linkage metadata loaded
/// through the `session_review_request` table when the session has been
/// published for remote review.
pub struct SessionRow {
    pub added_lines: i64,
    pub base_branch: String,
    pub created_at: i64,
    pub deleted_lines: i64,
    pub id: String,
    pub in_progress_started_at: Option<i64>,
    pub in_progress_total_seconds: i64,
    pub input_tokens: i64,
    pub is_draft: bool,
    pub model: String,
    pub output: String,
    pub output_tokens: i64,
    pub project_id: Option<i64>,
    pub prompt: String,
    pub reasoning_level_override: Option<String>,
    pub published_upstream_ref: Option<String>,
    pub questions: Option<String>,
    pub review_request: Option<SessionReviewRequestRow>,
    pub size: String,
    pub status: String,
    pub summary: Option<String>,
    pub title: Option<String>,
    pub updated_at: i64,
}

/// Session-focused persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait SessionRepository: Send + Sync {
    /// Appends text to the saved output for a session row.
    async fn append_session_output(&self, id: &str, chunk: &str) -> Result<(), DbError>;

    /// Sets `project_id` for sessions that do not yet reference a project.
    async fn backfill_session_project(&self, project_id: i64) -> Result<(), DbError>;

    /// Deletes a session row by identifier.
    async fn delete_session(&self, id: &str) -> Result<(), DbError>;

    /// Returns the persisted base branch for a session, when present.
    async fn get_session_base_branch(&self, id: &str) -> Result<Option<String>, DbError>;

    /// Returns the persisted app-server instruction bootstrap marker for a
    /// session, when present.
    async fn get_session_instruction_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Returns the provider conversation identifier for a session, when
    /// present.
    async fn get_session_provider_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Inserts a newly created draft-session row.
    async fn insert_draft_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError>;

    /// Inserts a newly created session row.
    async fn insert_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError>;

    #[cfg(test)]
    /// Loads all sessions ordered by most recent update.
    async fn load_sessions(&self) -> Result<Vec<SessionRow>, DbError>;

    /// Loads all sessions ordered by most recent update for one project.
    async fn load_sessions_for_project(&self, project_id: i64) -> Result<Vec<SessionRow>, DbError>;

    /// Loads lightweight session metadata used for cheap change detection.
    async fn load_sessions_metadata(&self) -> Result<(i64, i64), DbError>;

    /// Loads the project identifier associated with one session.
    async fn load_session_project_id(&self, session_id: &str) -> Result<Option<i64>, DbError>;

    /// Returns the persisted upstream reference for a published session
    /// branch, when present.
    async fn load_session_published_upstream_ref(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Loads the persisted session-specific reasoning override, when present.
    async fn load_session_reasoning_level_override(
        &self,
        session_id: &str,
    ) -> Result<Option<ReasoningLevel>, DbError>;

    /// Loads the persisted summary text associated with one session.
    async fn load_session_summary(&self, session_id: &str) -> Result<Option<String>, DbError>;

    /// Returns `(created_at, updated_at)` timestamps for a session.
    async fn load_session_timestamps(
        &self,
        session_id: &str,
    ) -> Result<Option<(i64, i64)>, DbError>;

    /// Persists all canonical turn metadata for one completed agent turn in a
    /// single transaction.
    async fn persist_session_turn_metadata<'a>(
        &self,
        session_id: &'a str,
        turn_metadata: &'a SessionTurnMetadata<'a>,
    ) -> Result<(), DbError>;

    #[cfg(test)]
    /// Replaces the full output for a session row.
    async fn replace_session_output(&self, id: &str, output: &str) -> Result<(), DbError>;

    /// Updates persisted diff-derived size and line-count fields for a
    /// session row.
    async fn update_session_diff_stats(
        &self,
        added_lines: u64,
        deleted_lines: u64,
        id: &str,
        size: &str,
    ) -> Result<(), DbError>;

    /// Updates the persisted app-server instruction bootstrap marker for a
    /// session.
    async fn update_session_instruction_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the persisted model for a session.
    async fn update_session_model(&self, id: &str, model: &str) -> Result<(), DbError>;

    /// Updates the saved prompt for a session row.
    async fn update_session_prompt(&self, id: &str, prompt: &str) -> Result<(), DbError>;

    /// Updates the persisted provider conversation identifier for a session.
    async fn update_session_provider_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the model clarification questions for a session row.
    async fn update_session_questions(&self, id: &str, questions: &str) -> Result<(), DbError>;

    /// Updates the persisted session-specific reasoning override.
    async fn update_session_reasoning_level(
        &self,
        id: &str,
        reasoning_level: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the persisted upstream reference for a published session
    /// branch.
    async fn update_session_published_upstream_ref(
        &self,
        id: &str,
        published_upstream_ref: Option<String>,
    ) -> Result<(), DbError>;

    /// Accumulates token statistics for a session.
    async fn update_session_stats(&self, id: &str, stats: &SessionStats) -> Result<(), DbError>;

    /// Updates the status for a session row and opens or closes the persisted
    /// cumulative active-work interval when crossing the `InProgress`
    /// boundary.
    async fn update_session_status_with_timing_at(
        &self,
        id: &str,
        status: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError>;

    /// Updates the persisted session summary text for a session row.
    async fn update_session_summary(&self, id: &str, summary: &str) -> Result<(), DbError>;

    /// Updates the display title for a session row.
    async fn update_session_title(&self, id: &str, title: &str) -> Result<(), DbError>;

    /// Updates the display title for a session row only when the persisted
    /// prompt still matches the prompt snapshot used to generate that title.
    async fn update_session_title_for_prompt(
        &self,
        id: &str,
        expected_prompt: &str,
        title: &str,
    ) -> Result<bool, DbError>;

    /// Overrides the `created_at` timestamp for one session row.
    #[cfg(test)]
    async fn update_session_created_at(&self, id: &str, created_at: i64) -> Result<(), DbError>;

    #[cfg(test)]
    /// Overrides the `updated_at` timestamp for one session row.
    async fn update_session_updated_at(&self, id: &str, updated_at: i64) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`SessionRepository`].
#[derive(Clone)]
pub(crate) struct SqliteSessionRepository(SqlitePool);

impl SqliteSessionRepository {
    /// Creates a session repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

/// Row returned when loading a required string scalar value.
struct RequiredStringValueRow {
    value: String,
}

/// Row returned when loading session count and latest-update metadata.
struct SessionMetadataRow {
    max_updated_at: i64,
    session_count: i64,
}

/// Row returned when loading an optional `i64` scalar value.
struct OptionalI64ValueRow {
    value: Option<i64>,
}

/// Row returned when loading the persisted instruction bootstrap marker for
/// one session.
#[derive(sqlx::FromRow)]
struct SessionInstructionStateRow {
    app_server_instruction_provider_conversation_id: Option<String>,
}

impl SessionInstructionStateRow {
    /// Converts the optional stored provider conversation id into one
    /// normalized bootstrap conversation id when present and non-empty.
    fn into_instruction_conversation_id(self) -> Option<String> {
        agent::normalize_instruction_conversation_id(
            self.app_server_instruction_provider_conversation_id
                .as_deref(),
        )
    }
}

/// Row returned when loading both persisted timestamps for one session.
struct SessionTimestampsRow {
    created_at: i64,
    updated_at: i64,
}

/// Row returned when loading one `session` plus aliased
/// `session_review_request` join columns.
pub(crate) struct SessionJoinRow {
    added_lines: i64,
    base_branch: String,
    created_at: i64,
    deleted_lines: i64,
    id: String,
    in_progress_started_at: Option<i64>,
    in_progress_total_seconds: i64,
    input_tokens: i64,
    is_draft: bool,
    model: String,
    output: String,
    output_tokens: i64,
    project_id: Option<i64>,
    prompt: String,
    reasoning_level_override: Option<String>,
    published_upstream_ref: Option<String>,
    questions: Option<String>,
    review_request_display_id: Option<String>,
    review_request_forge_kind: Option<String>,
    pub(crate) review_request_last_refreshed_at: Option<i64>,
    review_request_source_branch: Option<String>,
    review_request_state: Option<String>,
    review_request_status_summary: Option<String>,
    review_request_target_branch: Option<String>,
    review_request_title: Option<String>,
    review_request_web_url: Option<String>,
    size: String,
    status: String,
    summary: Option<String>,
    title: Option<String>,
    updated_at: i64,
}

impl SessionJoinRow {
    /// Converts the macro-mapped join row into the public `SessionRow` model.
    pub(crate) fn into_session_row(self) -> SessionRow {
        let Self {
            added_lines,
            base_branch,
            created_at,
            deleted_lines,
            id,
            in_progress_started_at,
            in_progress_total_seconds,
            input_tokens,
            is_draft,
            model,
            output,
            output_tokens,
            project_id,
            prompt,
            reasoning_level_override,
            published_upstream_ref,
            questions,
            review_request_display_id,
            review_request_forge_kind,
            review_request_last_refreshed_at,
            review_request_source_branch,
            review_request_state,
            review_request_status_summary,
            review_request_target_branch,
            review_request_title,
            review_request_web_url,
            size,
            status,
            summary,
            title,
            updated_at,
        } = self;

        let review_request = SessionReviewRequestJoinRow {
            display_id: review_request_display_id,
            forge_kind: review_request_forge_kind,
            last_refreshed_at: review_request_last_refreshed_at,
            source_branch: review_request_source_branch,
            state: review_request_state,
            status_summary: review_request_status_summary,
            target_branch: review_request_target_branch,
            title: review_request_title,
            web_url: review_request_web_url,
        }
        .into_review_request_row();

        SessionRow {
            added_lines,
            base_branch,
            created_at,
            deleted_lines,
            id,
            in_progress_started_at,
            in_progress_total_seconds,
            input_tokens,
            is_draft,
            model,
            output,
            output_tokens,
            project_id,
            prompt,
            reasoning_level_override,
            published_upstream_ref,
            questions,
            review_request,
            size,
            status,
            summary,
            title,
            updated_at,
        }
    }

    #[cfg(test)]
    /// Builds a deterministic joined-session row fixture for repository tests.
    pub(crate) fn fixture_for_test() -> Self {
        Self {
            added_lines: 14,
            base_branch: "main".to_string(),
            created_at: 100,
            deleted_lines: 6,
            id: "session-a".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            input_tokens: 11,
            is_draft: false,
            model: "gpt-5.4".to_string(),
            output: "Saved output".to_string(),
            output_tokens: 29,
            project_id: Some(7),
            prompt: "Implement feature".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: Some("origin/session-a".to_string()),
            questions: Some("Question text".to_string()),
            review_request_display_id: Some("#42".to_string()),
            review_request_forge_kind: Some("GitHub".to_string()),
            review_request_last_refreshed_at: Some(456),
            review_request_source_branch: Some("feature/forge".to_string()),
            review_request_state: Some("Open".to_string()),
            review_request_status_summary: Some("2 approvals, checks passing".to_string()),
            review_request_target_branch: Some("main".to_string()),
            review_request_title: Some("Add forge review support".to_string()),
            review_request_web_url: Some(
                "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ),
            size: "M".to_string(),
            status: "Review".to_string(),
            summary: Some("Summary text".to_string()),
            title: Some("Review session".to_string()),
            updated_at: 200,
        }
    }
}

/// Aliased nullable `session_review_request` columns loaded through a joined
/// session query.
struct SessionReviewRequestJoinRow {
    display_id: Option<String>,
    forge_kind: Option<String>,
    last_refreshed_at: Option<i64>,
    source_branch: Option<String>,
    state: Option<String>,
    status_summary: Option<String>,
    target_branch: Option<String>,
    title: Option<String>,
    web_url: Option<String>,
}

impl SessionReviewRequestJoinRow {
    /// Converts the joined nullable columns into a review-request row only
    /// when every required field is present.
    fn into_review_request_row(self) -> Option<SessionReviewRequestRow> {
        let Self {
            display_id,
            forge_kind,
            last_refreshed_at,
            source_branch,
            state,
            status_summary,
            target_branch,
            title,
            web_url,
        } = self;

        Some(SessionReviewRequestRow {
            display_id: display_id?,
            forge_kind: forge_kind?,
            last_refreshed_at: last_refreshed_at?,
            source_branch: source_branch?,
            state: state?,
            status_summary,
            target_branch: target_branch?,
            title: title?,
            web_url: web_url?,
        })
    }
}

#[async_trait]
impl SessionRepository for SqliteSessionRepository {
    async fn append_session_output(&self, id: &str, chunk: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET output = output || ?
WHERE id = ?
",
        )
        .bind(chunk)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn backfill_session_project(&self, project_id: i64) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET project_id = ?
WHERE project_id IS NULL
",
        )
        .bind(project_id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn delete_session(&self, id: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
DELETE FROM session
WHERE id = ?
",
        )
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn get_session_base_branch(&self, id: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as!(
            RequiredStringValueRow,
            r#"
SELECT base_branch AS "value!: _"
FROM session
WHERE id = ?
"#,
            id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.map(|row| row.value))
    }

    async fn get_session_instruction_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as::<_, SessionInstructionStateRow>(
            r"
SELECT app_server_instruction_provider_conversation_id
FROM session
WHERE id = ?
",
        )
        .bind(id)
        .fetch_optional(&self.0)
        .await?;

        Ok(row.and_then(SessionInstructionStateRow::into_instruction_conversation_id))
    }

    async fn get_session_provider_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT provider_conversation_id FROM session WHERE id = ?",
            id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value)
    }

    async fn insert_draft_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        insert_session_with_draft_mode(&self.0, id, model, base_branch, status, true, project_id)
            .await
    }

    async fn insert_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        insert_session_with_draft_mode(&self.0, id, model, base_branch, status, false, project_id)
            .await
    }

    #[cfg(test)]
    async fn load_sessions(&self) -> Result<Vec<SessionRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionJoinRow,
            r#"
SELECT session.base_branch AS "base_branch!",
       session.added_lines AS "added_lines!",
       session.created_at AS "created_at!",
       session.deleted_lines AS "deleted_lines!",
       session.id AS "id!",
       session.in_progress_started_at,
       session.in_progress_total_seconds AS "in_progress_total_seconds!",
       session.input_tokens AS "input_tokens!",
       session.is_draft AS "is_draft!: bool",
       session.model AS "model!",
       session.output AS "output!",
       session.output_tokens AS "output_tokens!",
       session.project_id,
       session.prompt AS "prompt!",
       session.reasoning_level AS "reasoning_level_override?",
       session.published_upstream_ref,
       session.questions,
       session_review_request.display_id AS "review_request_display_id?",
       session_review_request.forge_kind AS "review_request_forge_kind?",
       session_review_request.last_refreshed_at AS "review_request_last_refreshed_at?",
       session_review_request.source_branch AS "review_request_source_branch?",
       session_review_request.state AS "review_request_state?",
       session_review_request.status_summary AS "review_request_status_summary?",
       session_review_request.target_branch AS "review_request_target_branch?",
       session_review_request.title AS "review_request_title?",
       session_review_request.web_url AS "review_request_web_url?",
       session.size AS "size!",
       session.status AS "status!",
       session.summary,
       session.title,
       session.updated_at AS "updated_at!"
FROM session
LEFT JOIN session_review_request
ON session_review_request.session_id = session.id
ORDER BY session.updated_at DESC, session.id
"#
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows
            .into_iter()
            .map(SessionJoinRow::into_session_row)
            .collect())
    }

    async fn load_sessions_for_project(&self, project_id: i64) -> Result<Vec<SessionRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionJoinRow,
            r#"
SELECT session.base_branch AS "base_branch!",
       session.added_lines AS "added_lines!",
       session.created_at AS "created_at!",
       session.deleted_lines AS "deleted_lines!",
       session.id AS "id!",
       session.in_progress_started_at,
       session.in_progress_total_seconds AS "in_progress_total_seconds!",
       session.input_tokens AS "input_tokens!",
       session.is_draft AS "is_draft!: bool",
       session.model AS "model!",
       session.output AS "output!",
       session.output_tokens AS "output_tokens!",
       session.project_id,
       session.prompt AS "prompt!",
       session.reasoning_level AS "reasoning_level_override?",
       session.published_upstream_ref,
       session.questions,
       session_review_request.display_id AS "review_request_display_id?",
       session_review_request.forge_kind AS "review_request_forge_kind?",
       session_review_request.last_refreshed_at AS "review_request_last_refreshed_at?",
       session_review_request.source_branch AS "review_request_source_branch?",
       session_review_request.state AS "review_request_state?",
       session_review_request.status_summary AS "review_request_status_summary?",
       session_review_request.target_branch AS "review_request_target_branch?",
       session_review_request.title AS "review_request_title?",
       session_review_request.web_url AS "review_request_web_url?",
       session.size AS "size!",
       session.status AS "status!",
       session.summary,
       session.title,
       session.updated_at AS "updated_at!"
FROM session
LEFT JOIN session_review_request
ON session_review_request.session_id = session.id
WHERE session.project_id = ?
ORDER BY session.updated_at DESC, session.id
"#,
            project_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows
            .into_iter()
            .map(SessionJoinRow::into_session_row)
            .collect())
    }

    async fn load_sessions_metadata(&self) -> Result<(i64, i64), DbError> {
        let row = sqlx::query_as!(
            SessionMetadataRow,
            r#"
SELECT (SELECT COUNT(*) FROM session) AS "session_count!: _",
       COALESCE(
           (
               SELECT updated_at
               FROM session
               ORDER BY updated_at DESC, id
               LIMIT 1
           ),
           0
       ) AS "max_updated_at!: _"
"#
        )
        .fetch_one(&self.0)
        .await?;

        Ok((row.session_count, row.max_updated_at))
    }

    async fn load_session_project_id(&self, session_id: &str) -> Result<Option<i64>, DbError> {
        let row = sqlx::query_as!(
            OptionalI64ValueRow,
            r#"
SELECT project_id AS "value: _"
FROM session
WHERE id = ?
"#,
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.and_then(|row| row.value))
    }

    async fn load_session_published_upstream_ref(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT published_upstream_ref FROM session WHERE id = ?",
            id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value)
    }

    async fn load_session_reasoning_level_override(
        &self,
        session_id: &str,
    ) -> Result<Option<ReasoningLevel>, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT reasoning_level FROM session WHERE id = ?",
            session_id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value.and_then(|value| value.parse::<ReasoningLevel>().ok()))
    }

    async fn load_session_summary(&self, session_id: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query_scalar::<_, Option<String>>(
            r"
SELECT summary
FROM session
WHERE id = ?
",
        )
        .bind(session_id)
        .fetch_optional(&self.0)
        .await?;

        Ok(row.flatten())
    }

    async fn load_session_timestamps(
        &self,
        session_id: &str,
    ) -> Result<Option<(i64, i64)>, DbError> {
        let row = sqlx::query_as!(
            SessionTimestampsRow,
            r#"
SELECT created_at, updated_at
FROM session
WHERE id = ?
            "#,
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.map(|row| (row.created_at, row.updated_at)))
    }

    async fn persist_session_turn_metadata<'a>(
        &self,
        session_id: &'a str,
        turn_metadata: &'a SessionTurnMetadata<'a>,
    ) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;

        let session_update = sqlx::query(
            r"
UPDATE session
SET questions = ?,
    summary = ?,
    provider_conversation_id = ?,
    app_server_instruction_provider_conversation_id = ?
WHERE id = ?
",
        )
        .bind(turn_metadata.questions_json)
        .bind(turn_metadata.summary)
        .bind(turn_metadata.provider_conversation_id)
        .bind(turn_metadata.instruction_conversation_id)
        .bind(session_id)
        .execute(&mut *transaction)
        .await?;
        if session_update.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound.into());
        }

        if turn_metadata.token_usage_delta.input_tokens != 0
            || turn_metadata.token_usage_delta.output_tokens != 0
        {
            sqlx::query(
                r"
UPDATE session
SET input_tokens = input_tokens + ?,
    output_tokens = output_tokens + ?
WHERE id = ?
",
            )
            .bind(turn_metadata.token_usage_delta.input_tokens.cast_signed())
            .bind(turn_metadata.token_usage_delta.output_tokens.cast_signed())
            .bind(session_id)
            .execute(&mut *transaction)
            .await?;

            sqlx::query(
                r"
INSERT INTO session_usage (session_id, model, input_tokens, output_tokens, invocation_count)
VALUES (?, ?, ?, ?, 1)
ON CONFLICT(session_id, model) DO UPDATE SET
    input_tokens = input_tokens + excluded.input_tokens,
    output_tokens = output_tokens + excluded.output_tokens,
    invocation_count = invocation_count + 1
",
            )
            .bind(session_id)
            .bind(turn_metadata.model)
            .bind(turn_metadata.token_usage_delta.input_tokens.cast_signed())
            .bind(turn_metadata.token_usage_delta.output_tokens.cast_signed())
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    #[cfg(test)]
    async fn replace_session_output(&self, id: &str, output: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET output = ?
WHERE id = ?
",
        )
        .bind(output)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_diff_stats(
        &self,
        added_lines: u64,
        deleted_lines: u64,
        id: &str,
        size: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET added_lines = ?,
    deleted_lines = ?,
    size = ?
WHERE id = ?
  AND (
      added_lines <> ?
      OR deleted_lines <> ?
      OR size <> ?
  )
",
        )
        .bind(added_lines.cast_signed())
        .bind(deleted_lines.cast_signed())
        .bind(size)
        .bind(id)
        .bind(added_lines.cast_signed())
        .bind(deleted_lines.cast_signed())
        .bind(size)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_instruction_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET app_server_instruction_provider_conversation_id = ?
WHERE id = ?
",
        )
        .bind(provider_conversation_id.as_deref())
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_model(&self, id: &str, model: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET model = ?
WHERE id = ?
",
        )
        .bind(model)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_prompt(&self, id: &str, prompt: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET prompt = ?
WHERE id = ?
",
        )
        .bind(prompt)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_provider_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET provider_conversation_id = ?
WHERE id = ?
",
        )
        .bind(provider_conversation_id.as_deref())
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_questions(&self, id: &str, questions: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET questions = ?
WHERE id = ?
",
        )
        .bind(questions)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_reasoning_level(
        &self,
        id: &str,
        reasoning_level: Option<String>,
    ) -> Result<(), DbError> {
        sqlx::query!(
            r#"
UPDATE session
SET reasoning_level = ?
WHERE id = ?
            "#,
            reasoning_level,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_published_upstream_ref(
        &self,
        id: &str,
        published_upstream_ref: Option<String>,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET published_upstream_ref = ?
WHERE id = ?
",
        )
        .bind(published_upstream_ref.as_deref())
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_stats(&self, id: &str, stats: &SessionStats) -> Result<(), DbError> {
        if stats.input_tokens == 0 && stats.output_tokens == 0 {
            return Ok(());
        }

        sqlx::query(
            r"
UPDATE session
SET input_tokens = input_tokens + ?,
    output_tokens = output_tokens + ?
WHERE id = ?
",
        )
        .bind(stats.input_tokens.cast_signed())
        .bind(stats.output_tokens.cast_signed())
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_status_with_timing_at(
        &self,
        id: &str,
        status: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET status = ?,
    in_progress_total_seconds = CASE
        WHEN ? = 'InProgress' OR in_progress_started_at IS NULL THEN in_progress_total_seconds
        ELSE in_progress_total_seconds + MAX(0, ? - in_progress_started_at)
    END,
    in_progress_started_at = CASE
        WHEN ? = 'InProgress' THEN COALESCE(in_progress_started_at, ?)
        ELSE NULL
    END
WHERE id = ?
",
        )
        .bind(status)
        .bind(status)
        .bind(timestamp_seconds)
        .bind(status)
        .bind(timestamp_seconds)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_summary(&self, id: &str, summary: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET summary = ?
WHERE id = ?
",
        )
        .bind(summary)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_title(&self, id: &str, title: &str) -> Result<(), DbError> {
        sqlx::query!(
            r#"
UPDATE session
SET title = ?
WHERE id = ?
"#,
            title,
            id,
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_title_for_prompt(
        &self,
        id: &str,
        expected_prompt: &str,
        title: &str,
    ) -> Result<bool, DbError> {
        let result = sqlx::query!(
            r#"
UPDATE session
SET title = ?
WHERE id = ?
  AND prompt = ?
"#,
            title,
            id,
            expected_prompt,
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    #[cfg(test)]
    async fn update_session_created_at(&self, id: &str, created_at: i64) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET created_at = ?
WHERE id = ?
",
        )
        .bind(created_at)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    #[cfg(test)]
    async fn update_session_updated_at(&self, id: &str, updated_at: i64) -> Result<(), DbError> {
        sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(updated_at)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

/// Inserts one newly created session row with explicit draft-mode
/// persistence.
async fn insert_session_with_draft_mode(
    pool: &SqlitePool,
    id: &str,
    model: &str,
    base_branch: &str,
    status: &str,
    is_draft: bool,
    project_id: i64,
) -> Result<(), DbError> {
    sqlx::query(
        r"
INSERT INTO session (id, model, base_branch, status, is_draft, project_id, prompt, output)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
",
    )
    .bind(id)
    .bind(model)
    .bind(base_branch)
    .bind(status)
    .bind(is_draft)
    .bind(project_id)
    .bind("")
    .bind("")
    .execute(pool)
    .await?;

    Ok(())
}
