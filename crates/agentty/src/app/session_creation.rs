//! Prepared session creation and foreground completion routing.

use std::path::PathBuf;

use ag_session::{CreateSessionRequest, SessionError, SessionId};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::app::session::{SessionCreationKind, SessionCreationSettings};
use crate::app::{App, AppEvent, SessionManager};
use crate::domain::input::InputState;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState};

/// Creation inputs captured before external work releases the foreground.
pub(super) enum PreparedSessionCreation {
    Materialized {
        base_branch: String,
        creation_kind: SessionCreationKind,
        project_id: i64,
        settings: SessionCreationSettings,
        working_dir: PathBuf,
    },
    Persisted(String),
}

/// Recipient and project captured when a creation request is accepted.
pub(crate) struct PendingSessionCreation {
    project_id: i64,
    response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
}

impl App {
    /// Starts creation without waiting for worktree setup. A missing response
    /// channel identifies an interactive request whose notice may open a
    /// composer.
    pub(crate) async fn start_session_creation(
        &mut self,
        request: CreateSessionRequest,
        response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    ) {
        let request_id = Uuid::new_v4().to_string();
        if response_tx.is_none() {
            self.interactive_session_creation = Some(request_id.clone());
            self.mode = AppMode::SyncBlockedPopup {
                default_branch: None,
                is_loading: false,
                message: "Creating session. Close this notice to continue browsing.".to_string(),
                project_name: None,
                title: "Creating session".to_string(),
            };
            self.mark_dirty();
        }
        self.pending_session_creations.insert(
            request_id.clone(),
            PendingSessionCreation {
                project_id: request.project_id,
                response_tx,
            },
        );

        match self.prepare_api_session_creation(request).await {
            Ok(PreparedSessionCreation::Materialized {
                base_branch,
                creation_kind,
                project_id,
                settings,
                working_dir,
            }) => {
                let services = self.services.clone();
                let completion_request_id = request_id.clone();
                let task = tokio::spawn(async move {
                    let result = SessionManager::materialize_session(
                        &services,
                        project_id,
                        &base_branch,
                        working_dir,
                        settings,
                        creation_kind,
                    )
                    .await
                    .map_err(|error| error.to_string());
                    services.emit_app_event(AppEvent::SessionCreationCompleted {
                        request_id: completion_request_id,
                        result,
                    });
                });
                self.services.track_session_creation_task(request_id, task);
            }
            Ok(PreparedSessionCreation::Persisted(session_id)) => {
                self.complete_session_creation(&request_id, Ok(session_id))
                    .await;
            }
            Err(error) => {
                self.complete_session_creation(&request_id, Err(error))
                    .await;
            }
        }
    }

    /// Applies background creation results in their arrival order.
    pub(crate) async fn complete_session_creations(
        &mut self,
        results: Vec<(String, Result<String, String>)>,
    ) {
        for (request_id, result) in results {
            self.complete_session_creation(&request_id, result.map_err(SessionError::Operation))
                .await;
        }
    }

    /// Refreshes the active snapshot before acknowledging creation, and never
    /// replaces navigation performed after the interactive notice was closed.
    pub(crate) async fn complete_session_creation(
        &mut self,
        request_id: &str,
        result: Result<String, SessionError>,
    ) {
        self.services.finish_session_creation_task(request_id).await;
        let Some(pending) = self.pending_session_creations.remove(request_id) else {
            return;
        };
        if let Ok(session_id) = &result {
            self.finish_api_session_creation(session_id).await;
        }
        if let Some(response_tx) = pending.response_tx {
            let _ = response_tx.send(result.map(SessionId::from));

            return;
        }

        if self.interactive_session_creation.as_deref() != Some(request_id) {
            return;
        }
        self.interactive_session_creation = None;
        if pending.project_id != self.active_project_id()
            || !matches!(&self.mode, AppMode::SyncBlockedPopup { title, .. } if title == "Creating session")
        {
            return;
        }
        self.mode = match result {
            Ok(session_id) => {
                let index = self
                    .sessions
                    .sessions()
                    .iter()
                    .position(|session| session.id == session_id);
                self.sessions.select_session_index(index);

                AppMode::Prompt {
                    at_mention_state: None,
                    attachment_state: PromptAttachmentState::default(),
                    focus: ChatFocus::Input,
                    history_state: PromptHistoryState::new(Vec::new()),
                    slash_state: self.prompt_slash_state(),
                    session_id: session_id.into(),
                    input: InputState::default(),
                    scroll_offset: None,
                }
            }
            Err(error) => AppMode::SyncBlockedPopup {
                default_branch: None,
                is_loading: false,
                message: error.to_string(),
                project_name: None,
                title: "Session creation unavailable".to_string(),
            },
        };
        self.mark_dirty();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ag_git::{GitError, MockGitClient};
    use ag_session::CreateSessionMode;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::Notify;

    use super::*;
    use crate::app::{AppServiceDeps, AppServices, SessionRuntimeCommand};
    use crate::runtime::PresentationState;

    /// Pauses worktree setup at its injected Git boundary until released.
    async fn delayed_creation_app() -> (App, tempfile::TempDir, Arc<Notify>) {
        let (mut app, directory) = crate::test_support::new_git_test_app().await;
        let release = Arc::new(Notify::new());
        let mut git = MockGitClient::new();
        git.expect_find_git_repo_root()
            .once()
            .returning(|path| Box::pin(async move { Some(path) }));
        git.expect_create_worktree().once().returning({
            let release = Arc::clone(&release);
            move |_, _, _, _| {
                let release = Arc::clone(&release);

                Box::pin(async move {
                    release.notified().await;

                    Err(GitError::CommandFailed {
                        command: "git worktree add".to_string(),
                        stderr: "delayed creation failed".to_string(),
                    })
                })
            }
        });
        app.services = AppServices::new_with_agent_clis(
            app.services.base_path().to_path_buf(),
            app.services.clock(),
            app.services.event_sender(),
            AppServiceDeps {
                app_server_client_override: app.services.app_server_client_override(),
                available_agent_kinds: app.services.available_agent_kinds(),
                clipboard_image_client_override: Some(app.services.clipboard_image_client()),
                fs_client: app.services.fs_client(),
                git_client: Arc::new(git),
                one_shot_client_override: Some(app.services.one_shot_client()),
                personality_catalog_client_override: Some(
                    app.services.personality_catalog_client(),
                ),
                repositories: app.services.db().clone(),
                review_request_client: app.services.review_request_client(),
            },
            app.services.available_agent_clis(),
        );

        (app, directory, release)
    }

    fn regular_request(app: &App) -> CreateSessionRequest {
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id: app.active_project_id(),
        }
    }

    async fn finish_creation(app: &mut App) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !app.pending_session_creations.is_empty() {
                let event = app.next_app_event().await.expect("creation completion");
                app.apply_app_events(event).await;
            }
        })
        .await
        .expect("creation must finish");
    }

    #[tokio::test]
    async fn delayed_creation_allows_input_and_rendering_without_stealing_navigation() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app().await;
        let request = regular_request(&app);
        let presentation = PresentationState::default();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");

        // Act
        tokio::time::timeout(
            Duration::from_secs(1),
            app.start_session_creation(request, None),
        )
        .await
        .expect("creation must return before Git finishes");
        crate::runtime::mode::sync_blocked::handle(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        crate::runtime::mode::list::handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        )
        .await
        .expect("help input");
        terminal
            .draw(|frame| presentation.render(&app.view_snapshot(), frame))
            .expect("draw help");

        // Assert
        assert!(matches!(app.mode, AppMode::Help { .. }));
        assert_eq!(app.pending_session_creations.len(), 1);
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "?")
        );
        release.notify_one();
        finish_creation(&mut app).await;
        assert!(matches!(app.mode, AppMode::Help { .. }));
    }

    #[tokio::test]
    async fn delayed_api_creation_allows_other_commands_before_answering() {
        // Arrange
        let (mut app, _directory, release) = delayed_creation_app().await;
        let (response_tx, mut response_rx) = oneshot::channel();
        let request = regular_request(&app);
        let (lookup_tx, lookup_rx) = oneshot::channel();

        // Act
        tokio::time::timeout(
            Duration::from_secs(1),
            app.apply_session_runtime_command(SessionRuntimeCommand::Create {
                request,
                response_tx,
            }),
        )
        .await
        .expect("command must return before Git finishes");
        app.apply_session_runtime_command(SessionRuntimeCommand::Get {
            response_tx: lookup_tx,
            session_id: "missing".into(),
        })
        .await;

        // Assert
        assert!(
            lookup_rx
                .await
                .expect("lookup response")
                .expect("lookup")
                .is_none()
        );
        assert!(matches!(
            response_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(app.mode, AppMode::List));
        release.notify_one();
        finish_creation(&mut app).await;
        assert!(
            response_rx
                .await
                .expect("creation response")
                .expect_err("delayed setup should fail")
                .to_string()
                .contains("delayed creation failed")
        );
    }

    #[tokio::test]
    async fn creation_completion_preserves_newer_notices_and_project_switches() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        let project_id = app.active_project_id();
        app.interactive_session_creation = Some("newer".to_string());
        for (request_id, creation_project_id) in [("older", project_id), ("newer", project_id + 1)]
        {
            app.pending_session_creations.insert(
                request_id.to_string(),
                PendingSessionCreation {
                    project_id: creation_project_id,
                    response_tx: None,
                },
            );
        }

        // Act
        app.complete_session_creation("older", Err(SessionError::NotFound))
            .await;
        app.complete_session_creation("newer", Err(SessionError::NotFound))
            .await;
        app.complete_session_creation("duplicate", Err(SessionError::NotFound))
            .await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(app.pending_session_creations.is_empty());
        assert!(app.interactive_session_creation.is_none());
    }

    #[tokio::test]
    async fn creation_rejection_preserves_typed_api_error_even_when_caller_disconnects() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        let (response_tx, response_rx) = oneshot::channel();
        let request = CreateSessionRequest {
            mode: CreateSessionMode::Stacked {
                parent_session_id: "missing".into(),
            },
            ..regular_request(&app)
        };

        // Act
        app.start_session_creation(request, Some(response_tx)).await;
        let result = response_rx.await.expect("creation response");
        let (response_tx, response_rx) = oneshot::channel();
        drop(response_rx);
        app.pending_session_creations.insert(
            "disconnected".to_string(),
            PendingSessionCreation {
                project_id: app.active_project_id(),
                response_tx: Some(response_tx),
            },
        );
        app.complete_session_creation("disconnected", Err(SessionError::NotFound))
            .await;

        // Assert
        assert_eq!(result, Err(SessionError::NotFound));
        assert!(app.pending_session_creations.is_empty());
    }
}
