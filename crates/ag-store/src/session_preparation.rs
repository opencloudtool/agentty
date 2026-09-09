//! Durable workspace preparation and first-prompt handoff.

use ag_session::{SessionMessageKind, stored_message_content};
use async_trait::async_trait;

use crate::session::{SqliteSessionRepository, insert_session_with_draft_mode};
use crate::session_message::SessionMessageStore;
use crate::{DbError, PersistedSessionCreation};

/// Workspace readiness, independent of the session's conversation status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum SessionPreparationState {
    /// A worker owns workspace setup.
    Preparing,
    /// Workspace setup completed successfully.
    Ready,
    /// Setup or prompt handoff needs an explicit retry.
    Failed,
    /// Cancellation prevents further setup or prompt dispatch.
    Canceled,
}

/// Recoverable inputs for workspace setup and a deferred first turn.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct SessionPreparationRow {
    /// Most recent setup failure, if any.
    pub error: Option<String>,
    /// Serialized structured prompt, retained until execution starts.
    pub prompt: Option<String>,
    /// Stable owner of the worktree.
    pub session_id: String,
    /// Branch or frozen commit from which to create the worktree.
    pub start_ref: String,
    /// Current workspace preparation state.
    pub state: SessionPreparationState,
}

/// Persistence boundary for resumable workspace setup.
#[async_trait]
pub trait SessionPreparationRepository: Send + Sync {
    /// Atomically reserves a session and its pending workspace setup.
    async fn reserve_session(&self, session: PersistedSessionCreation<'_>) -> Result<(), DbError>;
    /// Registers lazy draft or fork setup without replacing an existing
    /// attempt.
    async fn insert_session_preparation(
        &self,
        session_id: &str,
        start_ref: &str,
    ) -> Result<(), DbError>;
    /// Loads preparation state for one session.
    async fn load_session_preparation(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionPreparationRow>, DbError>;
    /// Loads preparation state for one project's session list.
    async fn load_session_preparations(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionPreparationRow>, DbError>;
    /// Saves the first prompt without overwriting an earlier accepted
    /// submission.
    async fn save_preparation_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<bool, DbError>;
    /// Transitions setup unless cancellation has already won the race.
    async fn update_session_preparation(
        &self,
        session_id: &str,
        state: SessionPreparationState,
        error: Option<&str>,
    ) -> Result<bool, DbError>;
    /// Cancels setup and returns whether its active worker owns cleanup.
    /// The claim races atomically with the worker's completion transition.
    async fn cancel_session_preparation(&self, session_id: &str) -> Result<bool, DbError>;
    /// Returns queue or execution evidence, excluding failed operations that
    /// never started and remain retryable.
    async fn preparation_prompt_operation_status(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError>;
    /// Removes a failed handoff only when execution never began, allowing
    /// its stable operation identifier to be submitted again.
    async fn reclaim_preparation_prompt_operation(&self, session_id: &str) -> Result<(), DbError>;
    /// Atomically records execution start, persists the user transcript, and
    /// acknowledges the saved payload and ends draft staging. Initial prompts
    /// reuse an existing matching transcript row; replies always append.
    /// Returns false if cancellation or completion already won.
    async fn begin_preparation_prompt_operation(
        &self,
        session_id: &str,
        transcript_text: &str,
    ) -> Result<bool, DbError>;
    /// Acknowledges recovered execution of a saved prompt.
    async fn clear_preparation_prompt(&self, session_id: &str) -> Result<(), DbError>;
    /// Makes interrupted setup retryable without automatically replaying a
    /// turn.
    async fn recover_session_preparations(&self) -> Result<(), DbError>;
}

#[async_trait]
impl SessionPreparationRepository for SqliteSessionRepository {
    async fn reserve_session(&self, session: PersistedSessionCreation<'_>) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;
        let session_id = session.id;
        let start_ref = session.base_branch;
        insert_session_with_draft_mode(&mut *transaction, self.now(), session).await?;
        sqlx::query(
            "INSERT INTO session_preparation (session_id, state, start_ref) VALUES (?, \
             'preparing', ?)",
        )
        .bind(session_id)
        .bind(start_ref)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(())
    }

    async fn insert_session_preparation(
        &self,
        session_id: &str,
        start_ref: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO session_preparation (session_id, state, start_ref) VALUES (?, \
             'preparing', ?) ON CONFLICT(session_id) DO NOTHING",
        )
        .bind(session_id)
        .bind(start_ref)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn load_session_preparation(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionPreparationRow>, DbError> {
        Ok(sqlx::query_as(
            "SELECT session_id, state, start_ref, prompt, error FROM session_preparation WHERE \
             session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.0)
        .await?)
    }

    async fn load_session_preparations(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionPreparationRow>, DbError> {
        Ok(sqlx::query_as(
            "SELECT session_id, state, start_ref, session_preparation.prompt, error FROM \
             session_preparation JOIN session ON session.id = session_id WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_all(&self.0)
        .await?)
    }

    async fn save_preparation_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE session_preparation SET prompt = ? WHERE session_id = ? AND prompt IS NULL \
             AND state != 'canceled'",
        )
        .bind(prompt)
        .bind(session_id)
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn update_session_preparation(
        &self,
        session_id: &str,
        state: SessionPreparationState,
        error: Option<&str>,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE session_preparation SET state = ?, error = ? WHERE session_id = ? AND state \
             != 'canceled'",
        )
        .bind(state)
        .bind(error)
        .bind(session_id)
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn cancel_session_preparation(&self, session_id: &str) -> Result<bool, DbError> {
        let claimed = sqlx::query(
            "UPDATE session_preparation SET state = 'canceled', error = NULL WHERE session_id = ? \
             AND state = 'preparing'",
        )
        .bind(session_id)
        .execute(&self.0)
        .await?
        .rows_affected()
            == 1;
        if !claimed {
            self.update_session_preparation(session_id, SessionPreparationState::Canceled, None)
                .await?;
        }

        Ok(claimed)
    }

    async fn preparation_prompt_operation_status(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT status FROM session_operation WHERE id = ? AND (status != 'failed' OR \
             started_at IS NOT NULL)",
        )
        .bind(format!("workspace:{session_id}"))
        .fetch_optional(&self.0)
        .await?)
    }

    async fn clear_preparation_prompt(&self, session_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE session_preparation SET prompt = NULL WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.0)
            .await?;

        Ok(())
    }

    async fn reclaim_preparation_prompt_operation(&self, session_id: &str) -> Result<(), DbError> {
        sqlx::query(
            "DELETE FROM session_operation WHERE id = ? AND status = 'failed' AND started_at IS \
             NULL",
        )
        .bind(format!("workspace:{session_id}"))
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn begin_preparation_prompt_operation(
        &self,
        session_id: &str,
        transcript_text: &str,
    ) -> Result<bool, DbError> {
        let mut transaction = self.0.begin().await?;
        let operation_id = format!("workspace:{session_id}");
        let operation_kind: Option<String> = sqlx::query_scalar(
            "UPDATE session_operation SET status = 'running', started_at = ?, heartbeat_at = ?, \
             last_error = NULL WHERE id = ? AND status = 'queued' AND cancel_requested = 0 AND \
             EXISTS (SELECT 1 FROM session_preparation WHERE session_id = ? AND state = 'ready' \
             AND prompt IS NOT NULL) RETURNING kind",
        )
        .bind(self.now())
        .bind(self.now())
        .bind(operation_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(operation_kind) = operation_kind.as_deref() {
            let content = stored_message_content(SessionMessageKind::UserPrompt, transcript_text);
            let recorded = operation_kind == "start_prompt"
                && sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM session_message WHERE session_id = ? AND kind = \
                     'user_prompt' AND content = ?)",
                )
                .bind(session_id)
                .bind(&content)
                .fetch_one(&mut *transaction)
                .await?;
            if !recorded {
                SessionMessageStore::append_normalized_in_transaction(
                    &mut transaction,
                    session_id,
                    SessionMessageKind::UserPrompt,
                    &content,
                    self.now(),
                )
                .await?;
            }
            sqlx::query("UPDATE session SET is_draft = 0 WHERE id = ?")
                .bind(session_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("UPDATE session_preparation SET prompt = NULL WHERE session_id = ?")
                .bind(session_id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;

        Ok(operation_kind.is_some())
    }

    async fn recover_session_preparations(&self) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE session_preparation SET state = 'failed', error = 'Workspace setup was \
             interrupted. Your saved prompt is retained.' WHERE state = 'preparing' OR (state = \
             'ready' AND prompt IS NOT NULL)",
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ag_agent::{PermissionMode, ReasoningLevel, ResponseStyle, SpeedMode};

    use super::*;
    use crate::{AppRepositories, ForkSessionSnapshot};

    #[tokio::test]
    async fn draft_staging_ends_only_when_saved_prompt_acceptance_commits() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("database");
        prepare_saved_operation(&db).await;
        sqlx::query("UPDATE session SET is_draft = 1 WHERE id = 'first'")
            .execute(&pool)
            .await
            .expect("draft");
        sqlx::query(
            "CREATE TRIGGER reject_transfer BEFORE UPDATE OF prompt ON session_preparation WHEN \
             NEW.prompt IS NULL BEGIN SELECT RAISE(ABORT, 'transfer rejected'); END",
        )
        .execute(&pool)
        .await
        .expect("trigger");

        // Act
        let rejected = db
            .sessions()
            .begin_preparation_prompt_operation("first", "saved payload")
            .await;
        let draft_after_failure = db
            .sessions()
            .load_session("first")
            .await
            .expect("load")
            .expect("session");
        sqlx::query("DROP TRIGGER reject_transfer")
            .execute(&pool)
            .await
            .expect("restore transfer");
        let accepted = db
            .sessions()
            .begin_preparation_prompt_operation("first", "saved payload")
            .await
            .expect("accept");
        let draft_after_acceptance = db
            .sessions()
            .load_session("first")
            .await
            .expect("load")
            .expect("session");

        // Assert
        assert!(rejected.is_err());
        assert!(draft_after_failure.is_draft);
        assert!(accepted);
        assert!(!draft_after_acceptance.is_draft);
    }

    #[tokio::test]
    async fn execution_marker_and_prompt_transfer_are_atomic_and_only_unstarted_failures_retry() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("database");
        prepare_saved_operation(&db).await;
        let sessions = db.sessions();
        sqlx::query(
            "CREATE TRIGGER reject_transfer BEFORE UPDATE OF prompt ON session_preparation WHEN \
             NEW.prompt IS NULL BEGIN SELECT RAISE(ABORT, 'transfer rejected'); END",
        )
        .execute(&pool)
        .await
        .expect("trigger");

        // Act
        let rejected = sessions
            .begin_preparation_prompt_operation("first", "saved payload")
            .await;

        // Assert
        assert!(rejected.is_err());
        let operations = db
            .operations()
            .load_unfinished_session_operations()
            .await
            .expect("operations");
        assert_eq!(operations[0].status, "queued");
        assert!(operations[0].started_at.is_none());
        assert_eq!(
            sessions
                .load_session_preparation("first")
                .await
                .expect("load")
                .expect("row")
                .prompt
                .as_deref(),
            Some("saved payload")
        );

        // Act: only an unstarted failed attempt may release its stable id.
        db.operations()
            .mark_session_operation_failed("workspace:first", "interrupted")
            .await
            .expect("failure");
        sessions
            .reclaim_preparation_prompt_operation("first")
            .await
            .expect("reclaim");
        assert!(
            sessions
                .preparation_prompt_operation_status("first")
                .await
                .expect("status")
                .is_none()
        );
        sqlx::query("DROP TRIGGER reject_transfer")
            .execute(&pool)
            .await
            .expect("restore transfer");
        db.operations()
            .insert_session_operation("workspace:first", "first", "start_prompt")
            .await
            .expect("retry");
        assert!(
            sessions
                .begin_preparation_prompt_operation("first", "saved payload")
                .await
                .expect("begin")
        );
        db.operations()
            .mark_session_operation_failed("workspace:first", "provider failed")
            .await
            .expect("failure after start");
        sessions
            .reclaim_preparation_prompt_operation("first")
            .await
            .expect("do not reclaim execution");

        // Assert
        assert_eq!(
            sessions
                .preparation_prompt_operation_status("first")
                .await
                .expect("status")
                .as_deref(),
            Some("failed")
        );
        assert!(
            !sessions
                .begin_preparation_prompt_operation("first", "saved payload")
                .await
                .expect("already begun")
        );
        assert!(
            sessions
                .load_session_preparation("first")
                .await
                .expect("load")
                .expect("row")
                .prompt
                .is_none()
        );
    }

    #[tokio::test]
    async fn restart_after_marker_commit_retains_the_prompt_without_worker_publication() {
        // Arrange
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("sessions.db");
        let db = crate::Database::open(&path).await.expect("database");
        prepare_saved_operation(&db).await;

        // Act: stop exactly after the start transaction, before live
        // publication.
        assert!(
            db.sessions()
                .begin_preparation_prompt_operation("first", "saved payload [Image #1]")
                .await
                .expect("begin")
        );
        db.pool().close().await;
        drop(db);
        let reopened = crate::Database::open(&path).await.expect("reopen");
        reopened
            .sessions()
            .recover_session_preparations()
            .await
            .expect("recover preparation");
        reopened
            .operations()
            .fail_unfinished_session_operations("restart")
            .await
            .expect("recover operations");
        reopened
            .sessions()
            .reclaim_preparation_prompt_operation("first")
            .await
            .expect("keep started operation");
        let messages = reopened
            .sessions()
            .load_session_messages("first")
            .await
            .expect("transcript");

        // Assert
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "saved payload [Image #1]");
        assert_eq!(messages[0].kind, "user_prompt");
        assert_eq!(messages[0].position, 0);
        assert!(
            reopened
                .sessions()
                .load_session_preparation("first")
                .await
                .expect("load")
                .expect("row")
                .prompt
                .is_none()
        );
        assert_eq!(
            reopened
                .sessions()
                .preparation_prompt_operation_status("first")
                .await
                .expect("operation")
                .as_deref(),
            Some("failed")
        );
    }

    #[tokio::test]
    async fn initial_publication_reuses_history_but_fork_replies_append() {
        for (kind, expected_count) in [("start_prompt", 1), ("reply", 2)] {
            // Arrange
            let (db, pool) = AppRepositories::in_memory_with_pool()
                .await
                .expect("database");
            prepare_saved_operation(&db).await;
            sqlx::query("UPDATE session_operation SET kind = ? WHERE id = 'workspace:first'")
                .bind(kind)
                .execute(&pool)
                .await
                .expect("kind");
            db.sessions()
                .append_session_message(
                    "first",
                    SessionMessageKind::UserPrompt,
                    "  repeated prompt",
                )
                .await
                .expect("legacy transcript");

            // Act
            assert!(
                db.sessions()
                    .begin_preparation_prompt_operation("first", "\n  repeated prompt \n")
                    .await
                    .expect("begin")
            );
            let messages = db
                .sessions()
                .load_session_messages("first")
                .await
                .expect("messages");

            // Assert
            assert_eq!(messages.len(), expected_count);
            for (position, message) in messages.iter().enumerate() {
                assert_eq!(message.position, i64::try_from(position).expect("position"));
                assert_eq!(message.content, "  repeated prompt");
            }
        }
    }

    /// Reserves a ready workspace with its first command durably queued.
    async fn prepare_saved_operation(db: &AppRepositories) {
        let project_id = db
            .projects()
            .upsert_project("/project", Some("main".to_string()))
            .await
            .expect("project");
        let sessions = db.sessions();
        sessions
            .reserve_session(reservation("first", project_id))
            .await
            .expect("session");
        sessions
            .save_preparation_prompt("first", "saved payload")
            .await
            .expect("prompt");
        sessions
            .update_session_preparation("first", SessionPreparationState::Ready, None)
            .await
            .expect("ready");
        db.operations()
            .insert_session_operation("workspace:first", "first", "start_prompt")
            .await
            .expect("operation");
    }

    fn reservation(id: &str, project_id: i64) -> PersistedSessionCreation<'_> {
        PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id,
            is_draft: false,
            model: "gpt-5.6-sol",
            orchestration_task_id: None,
            parent_session_id: None,
            permission_mode: PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ResponseStyle::default(),
            role: None,
            speed_mode: SpeedMode::Normal,
            status: "Draft",
        }
    }

    #[tokio::test]
    async fn reservation_retains_first_prompt_across_failure_recovery() {
        // Arrange
        let (repositories, _) = AppRepositories::in_memory_with_pool().await.expect("db");
        let project_id = repositories
            .projects()
            .upsert_project("project", None)
            .await
            .expect("project");
        let sessions = repositories.sessions();
        sessions
            .reserve_session(reservation("session", project_id))
            .await
            .expect("reserve");

        // Act
        assert!(
            sessions
                .save_preparation_prompt("session", "first prompt")
                .await
                .expect("save")
        );
        assert!(
            !sessions
                .save_preparation_prompt("session", "replacement")
                .await
                .expect("duplicate")
        );
        sessions
            .insert_session_preparation("session", "other-ref")
            .await
            .expect("idempotent insert");
        sessions
            .recover_session_preparations()
            .await
            .expect("recover");
        let recovered = sessions
            .load_session_preparation("session")
            .await
            .expect("load")
            .expect("row");
        let project_rows = sessions
            .load_session_preparations(project_id)
            .await
            .expect("project rows");
        sessions
            .update_session_preparation("session", SessionPreparationState::Preparing, None)
            .await
            .expect("retry");
        sessions
            .update_session_preparation("session", SessionPreparationState::Ready, None)
            .await
            .expect("ready");
        sessions
            .recover_session_preparations()
            .await
            .expect("recover handoff");
        let interrupted_handoff = sessions
            .load_session_preparation("session")
            .await
            .expect("load")
            .expect("row");
        sessions
            .clear_preparation_prompt("session")
            .await
            .expect("acknowledge");

        // Assert
        assert_eq!(recovered.state, SessionPreparationState::Failed);
        assert_eq!(recovered.start_ref, "main");
        assert_eq!(recovered.prompt.as_deref(), Some("first prompt"));
        assert!(recovered.error.expect("reason").contains("interrupted"));
        assert_eq!(project_rows.len(), 1);
        assert_eq!(interrupted_handoff.state, SessionPreparationState::Failed);
        assert!(
            sessions
                .load_session_preparation("missing")
                .await
                .expect("missing")
                .is_none()
        );
        assert!(
            sessions
                .load_session_preparations(project_id + 1)
                .await
                .expect("other project")
                .is_empty()
        );
        assert_eq!(
            sessions
                .load_session("session")
                .await
                .expect("session")
                .expect("row")
                .status,
            "Draft"
        );
    }

    #[tokio::test]
    async fn cancellation_rejects_late_completion_and_submission() {
        // Arrange
        let (repositories, _) = AppRepositories::in_memory_with_pool().await.expect("db");
        let project_id = repositories
            .projects()
            .upsert_project("project", None)
            .await
            .expect("project");
        let sessions = repositories.sessions();
        sessions
            .reserve_session(reservation("session", project_id))
            .await
            .expect("reserve");

        // Act
        let worker_owns_cleanup = sessions
            .cancel_session_preparation("session")
            .await
            .expect("cancel");
        let late_completion = sessions
            .update_session_preparation("session", SessionPreparationState::Ready, None)
            .await
            .expect("late completion");
        let canceled_submission = sessions
            .save_preparation_prompt("session", "late prompt")
            .await
            .expect("canceled submission");

        // Assert
        assert!(worker_owns_cleanup);
        assert!(!late_completion);
        assert!(!canceled_submission);
    }

    #[tokio::test]
    async fn cancellation_owns_cleanup_when_preparation_finished_first() {
        for state in [
            SessionPreparationState::Ready,
            SessionPreparationState::Failed,
        ] {
            // Arrange
            let repositories = AppRepositories::in_memory().await.expect("db");
            prepare_saved_operation(&repositories).await;
            repositories
                .sessions()
                .update_session_preparation("first", state, Some("setup result"))
                .await
                .expect("complete");

            // Act
            let worker_owns_cleanup = repositories
                .sessions()
                .cancel_session_preparation("first")
                .await
                .expect("cancel");
            let preparation = repositories
                .sessions()
                .load_session_preparation("first")
                .await
                .expect("load")
                .expect("row");

            // Assert
            assert!(!worker_owns_cleanup);
            assert_eq!(preparation.state, SessionPreparationState::Canceled);
            assert!(preparation.error.is_none());
            assert!(
                !repositories
                    .sessions()
                    .cancel_session_preparation("missing")
                    .await
                    .expect("legacy")
            );
        }
    }

    #[tokio::test]
    async fn reservation_rolls_back_when_preparation_cannot_be_persisted() {
        // Arrange
        let (repositories, pool) = AppRepositories::in_memory_with_pool().await.expect("db");
        let project_id = repositories
            .projects()
            .upsert_project("project", None)
            .await
            .expect("project");
        sqlx::query(
            "CREATE TRIGGER reject_preparation BEFORE INSERT ON session_preparation BEGIN SELECT \
             RAISE(ABORT, 'rejected'); END",
        )
        .execute(&pool)
        .await
        .expect("trigger");

        // Act
        let result = repositories
            .sessions()
            .reserve_session(reservation("rejected", project_id))
            .await;
        let row = repositories
            .sessions()
            .load_session("rejected")
            .await
            .expect("load");

        // Assert
        assert!(result.is_err());
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn lazy_and_fork_preparation_preserve_identity_and_handoff_evidence() {
        // Arrange
        let (repositories, _) = AppRepositories::in_memory_with_pool().await.expect("db");
        let project_id = repositories
            .projects()
            .upsert_project("project", None)
            .await
            .expect("project");
        let sessions = repositories.sessions();
        sessions
            .insert_session_with_agent(reservation("source", project_id))
            .await
            .expect("source");

        // Act
        sessions
            .insert_session_preparation("source", "parent-tip")
            .await
            .expect("lazy preparation");
        sessions
            .reserve_fork_session_snapshot(
                ForkSessionSnapshot {
                    new_session_id: "fork",
                    source_session_id: "source",
                    status: "Review",
                },
                "frozen-commit",
            )
            .await
            .expect("fork");
        let before = sessions
            .preparation_prompt_operation_status("fork")
            .await
            .expect("unsubmitted");
        repositories
            .operations()
            .insert_session_operation("workspace:fork", "fork", "reply")
            .await
            .expect("handoff");
        let queued = sessions
            .preparation_prompt_operation_status("fork")
            .await
            .expect("queued");
        repositories
            .operations()
            .mark_session_operation_done("workspace:fork")
            .await
            .expect("done");
        let completed = sessions
            .preparation_prompt_operation_status("fork")
            .await
            .expect("completed");
        let fork = sessions
            .load_session_preparation("fork")
            .await
            .expect("load")
            .expect("fork preparation");

        // Assert
        assert!(before.is_none());
        assert_eq!(queued.as_deref(), Some("queued"));
        assert_eq!(completed.as_deref(), Some("done"));
        assert_eq!(fork.start_ref, "frozen-commit");
        assert_eq!(fork.state, SessionPreparationState::Preparing);
    }
}
