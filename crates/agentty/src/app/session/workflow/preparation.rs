//! Background workspace preparation for persisted conversation identities.

use std::path::PathBuf;

use ag_agent as agent;

use super::{isolation, session_branch, session_folder};
use crate::app::session::SessionError;
use crate::app::{AppServices, SessionManager};
use crate::domain::agent::parse_persisted_session_agent_model;
use crate::infra::db::{SessionPreparationRow, SessionPreparationState, SessionRow};

impl SessionManager {
    /// Prepares one reserved workspace and durably records success or failure.
    pub(crate) async fn prepare_reserved_session(
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let preparation = services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
            .ok_or(SessionError::NotFound)?;
        if preparation.state == SessionPreparationState::Canceled {
            Self::cleanup_canceled_preparation(services, session_id);

            return Err(SessionError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }
        if preparation.state != SessionPreparationState::Preparing {
            return Err(SessionError::Workflow(
                "Workspace setup is not active".to_string(),
            ));
        }
        let row = services
            .db()
            .sessions()
            .load_session(session_id)
            .await?
            .ok_or(SessionError::NotFound)?;
        let selection = parse_persisted_session_agent_model(Some(&row.agent), &row.model);
        let backend = agent::create_backend(selection.kind());
        let result = Self::prepare_workspace(services, &preparation, &row, backend.as_ref()).await;
        let error = result.as_ref().err().map(ToString::to_string);
        let state = if result.is_ok() {
            SessionPreparationState::Ready
        } else {
            SessionPreparationState::Failed
        };
        let applied = services
            .db()
            .sessions()
            .update_session_preparation(session_id, state, error.as_deref())
            .await?;
        if !applied {
            Self::cleanup_canceled_preparation(services, session_id);

            return Err(SessionError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }

        result
    }

    /// Reclaims a canceled attempt, including a retry canceled before its
    /// worker starts and finds an existing checkout.
    fn cleanup_canceled_preparation(services: &AppServices, session_id: &str) {
        let folder = session_folder(services.base_path(), session_id);
        let has_worktree = services.fs_client().is_dir(folder.clone());
        Self::spawn_canceled_session_cleanup(
            services,
            folder,
            session_branch(session_id),
            has_worktree,
            session_id.to_string(),
        );
    }

    /// Materializes an owned checkout or validates one left by interrupted
    /// setup.
    async fn prepare_workspace(
        services: &AppServices,
        preparation: &SessionPreparationRow,
        row: &SessionRow,
        backend: &dyn agent::AgentBackend,
    ) -> Result<(), SessionError> {
        let session_id = &preparation.session_id;
        let project = services
            .db()
            .projects()
            .get_project(row.project_id.ok_or(SessionError::NotFound)?)
            .await?
            .ok_or(SessionError::NotFound)?;
        let folder = session_folder(services.base_path(), session_id);
        let fs_client = services.fs_client();
        let git_client = services.git_client();
        if fs_client.exists(folder.clone()) {
            isolation::validate_session_worktree(
                fs_client.as_ref(),
                git_client.as_ref(),
                &folder,
                session_id,
            )
            .await?;
        } else {
            let repo_root = git_client
                .find_git_repo_root(PathBuf::from(project.path))
                .await
                .ok_or_else(|| {
                    SessionError::Workflow("Failed to find git repository root".to_string())
                })?;
            Self::create_session_worktree(
                services,
                session_id,
                &folder,
                &repo_root,
                &session_branch(session_id),
                &preparation.start_ref,
            )
            .await?;
        }
        backend.setup(&folder).map_err(|error| {
            SessionError::Workflow(format!("Failed to setup session backend: {error}"))
        })?;
        if row.parent_session_id.is_some() {
            let hash = git_client.head_hash(folder).await?;
            services
                .db()
                .sessions()
                .update_session_stack_base_commit_hash(session_id, Some(hash))
                .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retry_canceled_before_worker_start_removes_its_existing_checkout() {
        // Arrange
        let (mut app, directory) = crate::test_support::new_git_test_app().await;
        let session_id = app.create_session().await.expect("session");
        let folder = session_folder(app.services.base_path(), &session_id);
        app.services
            .db()
            .sessions()
            .update_session_preparation(&session_id, SessionPreparationState::Preparing, None)
            .await
            .expect("retry");
        app.refresh_workspace_preparation(&session_id).await;

        // Act
        app.cancel_session(&session_id).await.expect("cancel");
        let result = SessionManager::prepare_reserved_session(&app.services, &session_id).await;
        app.wait_for_background_cleanup_tasks().await;
        let branch = app
            .services
            .git_client()
            .ref_hash(directory.path().to_path_buf(), session_branch(&session_id))
            .await;

        // Assert
        assert!(
            result
                .expect_err("canceled")
                .to_string()
                .contains("canceled")
        );
        assert!(!folder.exists());
        assert!(branch.is_err());
    }

    #[tokio::test]
    async fn retry_validates_owned_checkout_and_preserves_it_on_backend_failure() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_git_test_app().await;
        let id = app.create_session().await.expect("session");
        let row = app
            .services
            .db()
            .sessions()
            .load_session(&id)
            .await
            .expect("load")
            .expect("row");
        let preparation = app
            .services
            .db()
            .sessions()
            .load_session_preparation(&id)
            .await
            .expect("load")
            .expect("preparation");
        let mut backend = agent::MockAgentBackend::new();
        backend.expect_setup().once().returning(|_| {
            Err(agent::AgentBackendError::Setup(
                "retry backend failed".to_string(),
            ))
        });

        // Act
        let inactive = SessionManager::prepare_reserved_session(&app.services, &id).await;
        let failed =
            SessionManager::prepare_workspace(&app.services, &preparation, &row, &backend).await;
        app.services
            .db()
            .sessions()
            .update_session_preparation(&id, SessionPreparationState::Preparing, None)
            .await
            .expect("retry");
        let retried = SessionManager::prepare_reserved_session(&app.services, &id).await;

        // Assert
        assert!(
            inactive
                .expect_err("inactive")
                .to_string()
                .contains("not active")
        );
        assert!(
            failed
                .expect_err("backend")
                .to_string()
                .contains("retry backend failed")
        );
        assert!(retried.is_ok());
        assert!(session_folder(app.services.base_path(), &id).is_dir());
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_session_preparation(&id)
                .await
                .expect("load")
                .expect("preparation")
                .state,
            SessionPreparationState::Ready
        );
    }
}
