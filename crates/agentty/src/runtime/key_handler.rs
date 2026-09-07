use std::io;

use ag_session::{CreateSessionMode, CreateSessionRequest};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use tracing::warn;

use crate::app::App;
#[cfg(test)]
use crate::app::ReviewCacheEntry;
use crate::domain::orchestration::IntegrationApproach;
use crate::domain::session::{SessionId, can_append_session_to_stack};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, DiffSidebarFocus,
};
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, PresentationState, backend_err, mode};

/// Routes key events to the active mode handler and returns the next runtime
/// action.
///
/// Successful handlers mark the app dirty so the next loop iteration renders
/// the updated UI state.
pub(crate) async fn handle_key_event<B: Backend>(
    app: &mut App,
    presentation: &PresentationState,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let result = if let AppMode::Confirmation {
        selected_confirmation_index,
        ..
    } = &mut app.mode
    {
        let decision = mode::confirmation::handle(selected_confirmation_index, key);

        handle_confirmation_decision(app, decision).await
    } else if matches!(app.mode, AppMode::SessionCreation { .. }) {
        handle_session_creation_key(app, key).await
    } else if matches!(app.mode, AppMode::ProjectSwitcher { .. }) {
        handle_project_switcher_key(app, key).await
    } else if matches!(app.mode, AppMode::LaunchConfigurationSelector { .. }) {
        handle_launch_configuration_selector_key(app, key).await
    } else if matches!(app.mode, AppMode::PublishBranchInput { .. }) {
        Ok(handle_publish_branch_input_key(app, key).await)
    } else {
        match &app.mode {
            AppMode::List => mode::list::handle(app, key).await,
            AppMode::SessionCreation { .. } => {
                unreachable!("session creation mode is handled before dispatch matching")
            }
            AppMode::StackAppendParentSelection { .. } => {
                handle_stack_append_parent_key(app, key).await
            }
            AppMode::PreCommitHookWarning { .. } => {
                Ok(handle_pre_commit_hook_warning_key(app, key))
            }
            AppMode::ProjectSwitcher { .. } => {
                unreachable!("project switcher mode is handled before dispatch matching")
            }
            AppMode::SyncBlockedPopup { .. } => Ok(mode::sync_blocked::handle(app, key)),
            AppMode::ViewInfoPopup { .. } => Ok(handle_view_info_popup_key(app, key)),
            AppMode::Confirmation { .. } => {
                unreachable!("confirmation mode is handled before dispatch matching")
            }
            AppMode::View { .. } => {
                mode::session_view::handle_with_cache(
                    app,
                    presentation.render_cache_store(),
                    terminal,
                    key,
                )
                .await
            }
            AppMode::Prompt { .. } => {
                mode::prompt::handle_with_cache(
                    app,
                    presentation.render_cache_store(),
                    terminal,
                    key,
                )
                .await
            }
            AppMode::Question { .. } => handle_question_key(app, presentation, terminal, key).await,
            AppMode::DiffLoading { .. } => Ok(mode::diff::handle_loading(app, key)),
            AppMode::Diff {
                review_comments: Some(review_comments),
                ..
            } if review_comments.sidebar_focus == DiffSidebarFocus::Comments
                && !mode::diff::should_submit_line_comments(app, key)
                && !matches!(key.code, KeyCode::Char('?' | 'q')) =>
            {
                handle_review_comment_key(app, presentation, terminal, key).await
            }
            AppMode::Diff { .. } => {
                let size = terminal.size().map_err(backend_err)?;
                let terminal_rect = Rect::new(0, 0, size.width, size.height);
                let content_area = content_area_for_terminal(terminal_rect);
                let submit_line_comments = mode::diff::should_submit_line_comments(app, key);

                let result = mode::diff::handle_with_cache(
                    app,
                    presentation.render_cache_store(),
                    content_area,
                    key,
                );
                if submit_line_comments && matches!(app.mode, AppMode::Prompt { .. }) {
                    mode::prompt::submit_current_text_prompt(app).await;
                }

                Ok(result)
            }
            AppMode::Help { .. } => Ok(mode::help::handle(app, key)),
            AppMode::LaunchConfigurationSelector { .. } => {
                unreachable!(
                    "launch-configuration selector mode is handled before dispatch matching"
                )
            }
            AppMode::PublishBranchInput { .. } => {
                unreachable!("publish-branch input mode is handled before dispatch matching")
            }
        }
    };

    if result.is_ok() {
        app.mark_dirty();
    }

    result
}

/// Resolves the full terminal area and routes one clarification-question key
/// event through the shared render cache.
async fn handle_question_key<B: Backend>(
    app: &mut App,
    presentation: &PresentationState,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let size = terminal.size().map_err(backend_err)?;
    let terminal_rect = Rect::new(0, 0, size.width, size.height);

    Ok(mode::question::handle_with_cache(
        app,
        presentation.render_cache_store(),
        terminal_rect,
        key,
    )
    .await)
}

/// Resolves the page content area and routes one review-comment key event.
async fn handle_review_comment_key<B: Backend>(
    app: &mut App,
    presentation: &PresentationState,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let size = terminal.size().map_err(backend_err)?;
    let terminal_rect = Rect::new(0, 0, size.width, size.height);
    let content_area = content_area_for_terminal(terminal_rect);

    Ok(mode::review_comment::handle_with_cache(
        app,
        presentation.render_cache_store(),
        content_area,
        key,
    )
    .await)
}

/// Returns the central content area after removing the global status and
/// footer bars from the full terminal rectangle.
fn content_area_for_terminal(terminal_rect: Rect) -> Rect {
    let outer_chunks = Layout::default()
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(terminal_rect);

    outer_chunks[1]
}

/// Handles key input while the session creation selector is visible.
async fn handle_session_creation_key(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::List;
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            select_previous_session_creation_option(app);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            select_next_session_creation_option(app);
        }
        KeyCode::Enter => {
            create_selected_session(app).await?;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Updates the highlighted option in the session creation selector.
fn update_session_creation_selection(app: &mut App, selected_option_index: usize) {
    let mut selected_option_index = selected_option_index.min(4);
    while selected_option_index > 0
        && !session_creation_option_is_enabled(app, selected_option_index)
    {
        selected_option_index = selected_option_index.saturating_sub(1);
    }

    if let AppMode::SessionCreation {
        selected_option_index: current_index,
    } = &mut app.mode
    {
        *current_index = selected_option_index;
    }
}

/// Moves to the previous enabled session-creation option.
fn select_previous_session_creation_option(app: &mut App) {
    let current_index = current_session_creation_selection(app);
    let previous_index = (0..current_index)
        .rev()
        .find(|option_index| session_creation_option_is_enabled(app, *option_index))
        .unwrap_or(current_index);
    update_session_creation_selection(app, previous_index);
}

/// Moves to the next enabled session-creation option.
fn select_next_session_creation_option(app: &mut App) {
    let current_index = current_session_creation_selection(app);
    let next_index = ((current_index + 1)..=4)
        .find(|option_index| session_creation_option_is_enabled(app, *option_index))
        .unwrap_or(current_index);
    update_session_creation_selection(app, next_index);
}

/// Returns whether one creation-selector row can currently be chosen.
fn session_creation_option_is_enabled(app: &App, option_index: usize) -> bool {
    match option_index {
        0..=2 => true,
        3 => selected_stacked_parent_session_id(app).is_some(),
        4 => selected_stack_append_session_id(app).is_some(),
        _ => false,
    }
}

/// Creates the selected session type and opens its prompt composer.
async fn create_selected_session(app: &mut App) -> io::Result<()> {
    let selected_option_index = current_session_creation_selection(app);
    let mode = match selected_option_index {
        0 => CreateSessionMode::Regular,
        1 => CreateSessionMode::Draft,
        2 => CreateSessionMode::Orchestrator,
        3 => {
            let Some(parent_session_id) = selected_stacked_parent_session_id(app) else {
                return Ok(());
            };

            CreateSessionMode::Stacked { parent_session_id }
        }
        4 => {
            let Some(session_id) = selected_stack_append_session_id(app) else {
                return Ok(());
            };
            app.mode = AppMode::StackAppendParentSelection {
                selected_parent_index: 0,
                session_id,
            };

            return Ok(());
        }
        _ => return Ok(()),
    };
    let project_id = app.active_project_id();
    let service = app.session_service();
    let request = service.create_session(CreateSessionRequest {
        inherit_from_session_id: None,
        mode,
        project_id,
    });
    let session_id = match app.drive_session_request(request).await {
        Ok(session_id) => session_id,
        Err(error) => {
            app.mode = AppMode::SyncBlockedPopup {
                default_branch: None,
                is_loading: false,
                message: error.to_string(),
                project_name: None,
                title: "Session creation unavailable".to_string(),
            };

            return Ok(());
        }
    };
    mode::list::open_session_prompt(app, session_id.into());

    Ok(())
}

/// Handles the advisory shown before session-type selection.
fn handle_pre_commit_hook_warning_key(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Enter => {
            app.mode = AppMode::SessionCreation {
                selected_option_index: 0,
            };
        }
        KeyCode::Esc => app.mode = AppMode::List,
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        _ => {}
    }

    EventResult::Continue
}

/// Returns the current highlighted session-creation option.
fn current_session_creation_selection(app: &App) -> usize {
    match app.mode {
        AppMode::SessionCreation {
            selected_option_index,
        } => selected_option_index,
        _ => 0,
    }
}

/// Returns the selected session id when it can parent a stacked draft.
fn selected_stacked_parent_session_id(app: &App) -> Option<SessionId> {
    app.selected_session()
        .filter(|session| app.sessions.can_create_stacked_child(&session.id))
        .map(|session| session.id.clone())
}

/// Returns the selected review-ready session when it has an eligible parent.
fn selected_stack_append_session_id(app: &App) -> Option<SessionId> {
    let selected_session = app.selected_session()?;
    app.sessions
        .sessions()
        .iter()
        .any(|candidate| {
            can_append_session_to_stack(
                app.sessions.sessions(),
                selected_session.id.as_str(),
                candidate.id.as_str(),
            )
        })
        .then(|| selected_session.id.clone())
}

/// Handles navigation and confirmation in the stack-parent selector.
async fn handle_stack_append_parent_key(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::SessionCreation {
                selected_option_index: 4,
            };
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            update_stack_append_parent_selection(app, true);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            update_stack_append_parent_selection(app, false);
        }
        KeyCode::Enter => append_session_to_selected_stack(app).await,
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Moves the highlighted eligible parent up or down by one row.
fn update_stack_append_parent_selection(app: &mut App, move_up: bool) {
    let AppMode::StackAppendParentSelection {
        selected_parent_index,
        session_id,
    } = &app.mode
    else {
        return;
    };
    let parent_count = stack_append_parent_session_ids(app, session_id).len();
    let updated_index = if move_up {
        selected_parent_index.saturating_sub(1)
    } else {
        selected_parent_index
            .saturating_add(1)
            .min(parent_count.saturating_sub(1))
    };

    if let AppMode::StackAppendParentSelection {
        selected_parent_index,
        ..
    } = &mut app.mode
    {
        *selected_parent_index = updated_index;
    }
}

/// Returns eligible parent identifiers in visible session order.
fn stack_append_parent_session_ids(app: &App, session_id: &SessionId) -> Vec<SessionId> {
    app.sessions
        .sessions()
        .iter()
        .filter(|candidate| {
            can_append_session_to_stack(
                app.sessions.sessions(),
                session_id.as_str(),
                candidate.id.as_str(),
            )
        })
        .map(|session| session.id.clone())
        .collect()
}

/// Moves the source session beneath the highlighted parent and starts its
/// synchronization.
async fn append_session_to_selected_stack(app: &mut App) {
    let AppMode::StackAppendParentSelection {
        selected_parent_index,
        session_id,
    } = &app.mode
    else {
        return;
    };
    let session_id = session_id.clone();
    let parent_session_id = stack_append_parent_session_ids(app, &session_id)
        .get(*selected_parent_index)
        .cloned();
    app.mode = AppMode::List;

    let Some(parent_session_id) = parent_session_id else {
        return;
    };
    if let Err(error) = app
        .append_session_to_stack(session_id.as_str(), parent_session_id.as_str())
        .await
    {
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: false,
            message: error.to_string(),
            project_name: None,
            title: "Append to stack failed".to_string(),
        };
    }
}

/// Handles key input while the MRU project switcher popup is visible.
async fn handle_project_switcher_key(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::List;
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            update_project_switcher_selection(
                app,
                current_project_switcher_selection(app).saturating_sub(1),
            );
        }
        KeyCode::Down | KeyCode::Char('j') => {
            update_project_switcher_selection(
                app,
                current_project_switcher_selection(app).saturating_add(1),
            );
        }
        KeyCode::Enter => {
            switch_to_selected_switcher_project(app).await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Switches the active project to the highlighted MRU row and returns to the
/// sessions list.
///
/// Selecting the already-active project closes the popup without a switch. A
/// failed switch replaces the popup with the list informational popup so the
/// user sees why the project did not change instead of a no-op.
async fn switch_to_selected_switcher_project(app: &mut App) {
    let selected_project = app
        .projects
        .mru_project_items()
        .get(current_project_switcher_selection(app))
        .map(|project_item| {
            (
                project_item.project.id,
                project_item.project.display_label(),
            )
        });
    app.mode = AppMode::List;

    let Some((selected_project_id, selected_project_label)) = selected_project else {
        return;
    };

    if selected_project_id == app.active_project_id() {
        return;
    }

    if let Err(error) = app.switch_project(selected_project_id).await {
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: false,
            message: error.to_string(),
            project_name: Some(selected_project_label),
            title: "Project switch failed".to_string(),
        };
    }
}

/// Clamps and stores the highlighted row in the project switcher popup.
fn update_project_switcher_selection(app: &mut App, selected_option_index: usize) {
    let max_option_index = app.projects.mru_project_items().len().saturating_sub(1);

    if let AppMode::ProjectSwitcher {
        selected_option_index: current_index,
    } = &mut app.mode
    {
        *current_index = selected_option_index.min(max_option_index);
    }
}

/// Returns the currently highlighted project switcher row.
fn current_project_switcher_selection(app: &App) -> usize {
    match app.mode {
        AppMode::ProjectSwitcher {
            selected_option_index,
        } => selected_option_index,
        _ => 0,
    }
}

/// Handles key input while a session-scoped informational popup is visible.
fn handle_view_info_popup_key(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::ViewInfoPopup {
        is_loading,
        restore_view,
        ..
    } = &app.mode
    else {
        return EventResult::Continue;
    };

    if *is_loading {
        return EventResult::Continue;
    }

    match key.code {
        KeyCode::Enter | KeyCode::Esc => {
            app.mode = restore_view.clone().into_view_mode();
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = restore_view.clone().into_view_mode();
        }
        _ => {}
    }

    EventResult::Continue
}

/// Handles key input while the publish-branch input overlay is visible.
///
/// Only `Esc` cancels the overlay. Plain character keys continue to edit the
/// branch name so session-view shortcuts like `q` and `p` do not leak through
/// while the text field has focus.
async fn handle_publish_branch_input_key(app: &mut App, key: KeyEvent) -> EventResult {
    let publish_branch_input =
        PublishBranchInputModeState::from_mode(std::mem::replace(&mut app.mode, AppMode::List));
    let input_locked = publish_branch_input.locked_upstream_ref.is_some();

    match key.code {
        KeyCode::Esc => {
            app.mode = publish_branch_input.restore_view.into_view_mode();
        }
        KeyCode::Enter => {
            let remote_branch_name = if input_locked {
                Some(publish_branch_input.input.text().trim().to_string())
            } else {
                (!publish_branch_input.input.text().trim().is_empty())
                    .then(|| publish_branch_input.input.text().trim().to_string())
            };
            let session_id = publish_branch_input.restore_view.session_id.clone();

            app.start_publish_branch_action(
                publish_branch_input.restore_view,
                &session_id,
                publish_branch_input.publish_branch_action,
                remote_branch_name,
            )
            .await;
        }
        _ if !input_locked => {
            app.mode = if let Some(command) = mode::input_key::command_for_key(
                key,
                mode::input_key::InputCapabilities::SINGLE_LINE,
            ) {
                publish_branch_input.apply_input_edit(|input| {
                    input.apply(command);
                })
            } else {
                publish_branch_input.into_mode()
            };
        }
        _ => {
            app.mode = publish_branch_input.into_mode();
        }
    }

    EventResult::Continue
}

/// Captures `AppMode::PublishBranchInput` fields so key handlers can rebuild
/// the overlay consistently after input edits.
struct PublishBranchInputModeState {
    default_branch_name: String,
    input: crate::domain::input::InputState,
    locked_upstream_ref: Option<String>,
    publish_branch_action: crate::domain::session::PublishBranchAction,
    restore_view: ConfirmationViewMode,
}

impl PublishBranchInputModeState {
    /// Extracts publish-branch overlay fields from an app mode value.
    fn from_mode(mode: AppMode) -> Self {
        let AppMode::PublishBranchInput {
            default_branch_name,
            input,
            locked_upstream_ref,
            publish_branch_action,
            restore_view,
        } = mode
        else {
            unreachable!("mode must be publish-branch input in this handler");
        };

        Self {
            default_branch_name,
            input,
            locked_upstream_ref,
            publish_branch_action,
            restore_view,
        }
    }

    /// Applies one input edit and rebuilds the publish-branch overlay mode.
    fn apply_input_edit(
        mut self,
        edit: impl FnOnce(&mut crate::domain::input::InputState),
    ) -> AppMode {
        edit(&mut self.input);

        self.into_mode()
    }

    /// Rebuilds `AppMode::PublishBranchInput` from the stored overlay fields.
    fn into_mode(self) -> AppMode {
        AppMode::PublishBranchInput {
            default_branch_name: self.default_branch_name,
            input: self.input,
            locked_upstream_ref: self.locked_upstream_ref,
            publish_branch_action: self.publish_branch_action,
            restore_view: self.restore_view,
        }
    }
}

/// Handles key input while the app is in launch-configuration selector overlay
/// mode.
async fn handle_launch_configuration_selector_key(
    app: &mut App,
    key: KeyEvent,
) -> io::Result<EventResult> {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::LaunchConfigurationSelector {
        commands,
        restore_view,
        selected_command_index,
    } = mode
    else {
        unreachable!("mode must be launch-configuration selector in this handler");
    };

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = restore_view.into_view_mode();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.mode = AppMode::LaunchConfigurationSelector {
                selected_command_index: next_launch_configuration_index(
                    selected_command_index,
                    &commands,
                ),
                commands,
                restore_view,
            };
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.mode = AppMode::LaunchConfigurationSelector {
                selected_command_index: previous_launch_configuration_index(
                    selected_command_index,
                    &commands,
                ),
                commands,
                restore_view,
            };
        }
        KeyCode::Enter => {
            let selected_launch_configuration = commands
                .get(selected_command_index)
                .map(std::string::String::as_str);
            app.mode = restore_view.into_view_mode();
            app.open_session_worktree_in_tmux_with_command(selected_launch_configuration)
                .await;
        }
        _ => {
            app.mode = AppMode::LaunchConfigurationSelector {
                commands,
                restore_view,
                selected_command_index,
            };
        }
    }

    Ok(EventResult::Continue)
}

/// Returns the next command index with wrap-around.
fn next_launch_configuration_index(current_index: usize, commands: &[String]) -> usize {
    if commands.is_empty() {
        return 0;
    }

    (current_index + 1) % commands.len()
}

/// Returns the previous command index with wrap-around.
fn previous_launch_configuration_index(current_index: usize, commands: &[String]) -> usize {
    if commands.is_empty() {
        return 0;
    }

    if current_index == 0 {
        commands.len() - 1
    } else {
        current_index - 1
    }
}

/// Applies the semantic result of a generic confirmation interaction.
async fn handle_confirmation_decision(
    app: &mut App,
    decision: ConfirmationDecision,
) -> io::Result<EventResult> {
    match decision {
        ConfirmationDecision::Confirm => handle_confirmation_confirm(app).await,
        ConfirmationDecision::Reject => handle_confirmation_reject(app).await,
        ConfirmationDecision::Cancel => {
            app.mode = confirmation_cancel_mode(&app.mode);

            Ok(EventResult::Continue)
        }
        ConfirmationDecision::Continue => Ok(EventResult::Continue),
    }
}

/// Resolves target mode for `Cancel` in confirmation overlays.
fn confirmation_cancel_mode(mode: &AppMode) -> AppMode {
    if let AppMode::Confirmation {
        confirmation_intent:
            ConfirmationIntent::ContinueSession
            | ConfirmationIntent::ForkSession
            | ConfirmationIntent::MergeSession
            | ConfirmationIntent::RegenerateReview
            | ConfirmationIntent::DetachManagedSession
            | ConfirmationIntent::OpenManagedWorktree
            | ConfirmationIntent::ChooseIntegrationApproach,
        restore_view: Some(restore_view),
        ..
    } = mode
    {
        return restore_view.clone().into_view_mode();
    }

    AppMode::List
}

/// Resolves a positive confirmation by dispatching the configured action
/// intent.
async fn handle_confirmation_confirm(app: &mut App) -> io::Result<EventResult> {
    let (confirmation_intent, confirmation_session_id, restore_view) = match &app.mode {
        AppMode::Confirmation {
            confirmation_intent,
            restore_view,
            session_id,
            ..
        } => (
            *confirmation_intent,
            session_id.clone(),
            restore_view.clone(),
        ),
        _ => return Ok(EventResult::Continue),
    };

    match confirmation_intent {
        ConfirmationIntent::Quit => {
            app.mode = AppMode::List;

            Ok(EventResult::Quit)
        }
        ConfirmationIntent::CancelSession => {
            handle_cancel_session_confirmation(app, confirmation_session_id).await
        }
        ConfirmationIntent::ContinueSession => {
            handle_continue_session_confirmation(app, confirmation_session_id, restore_view).await
        }
        ConfirmationIntent::ForkSession => {
            handle_fork_session_confirmation(app, confirmation_session_id, restore_view).await
        }
        ConfirmationIntent::MergeSession => {
            handle_merge_confirmation(app, confirmation_session_id, restore_view).await
        }
        ConfirmationIntent::RegenerateReview => Ok(handle_regenerate_review_confirmation(
            app,
            confirmation_session_id,
            restore_view,
        )),
        ConfirmationIntent::DetachManagedSession => {
            handle_detach_managed_session_confirmation(app, confirmation_session_id, restore_view)
                .await
        }
        ConfirmationIntent::OpenManagedWorktree => {
            handle_open_managed_worktree_confirmation(app, confirmation_session_id, restore_view)
                .await
        }
        ConfirmationIntent::ChooseIntegrationApproach => {
            handle_integration_approach_confirmation(
                app,
                confirmation_session_id,
                restore_view,
                IntegrationApproach::LocalMerge,
            )
            .await
        }
    }
}

/// Opens a managed worker worktree after the user acknowledges write access.
async fn handle_open_managed_worktree_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(restore_view) = restore_view else {
        app.mode = AppMode::List;

        return Ok(EventResult::Continue);
    };
    if confirmation_session_id.as_ref() != Some(&restore_view.session_id) {
        app.mode = restore_view.into_view_mode();

        return Ok(EventResult::Continue);
    }

    mode::session_view::open_worktree_for_view_session(app, restore_view).await;

    Ok(EventResult::Continue)
}

/// Resolves the second binary choice, which only has distinct semantics for
/// orchestration integration; ordinary confirmations treat it as dismissal.
async fn handle_confirmation_reject(app: &mut App) -> io::Result<EventResult> {
    let AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
        restore_view,
        session_id,
        ..
    } = &app.mode
    else {
        app.mode = confirmation_cancel_mode(&app.mode);

        return Ok(EventResult::Continue);
    };
    let confirmation_session_id = session_id.clone();
    let restore_view = restore_view.clone();

    handle_integration_approach_confirmation(
        app,
        confirmation_session_id,
        restore_view,
        IntegrationApproach::ReviewRequest,
    )
    .await
}

/// Advances one verified campaign using the selected integration destination.
async fn handle_integration_approach_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
    integration_approach: IntegrationApproach,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };
    app.approve_orchestration(&session_id, Some(integration_approach))
        .await;
    app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

    Ok(EventResult::Continue)
}

/// Transfers one confirmed managed worker to ordinary user ownership.
async fn handle_detach_managed_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };
    app.detach_managed_child(&session_id).await;
    app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

    Ok(EventResult::Continue)
}

/// Cancels the confirmed cancelable session, when still present, and returns
/// to list mode.
async fn handle_cancel_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
) -> io::Result<EventResult> {
    app.mode = AppMode::List;

    let Some(session_id) = confirmation_session_id else {
        return Ok(EventResult::Continue);
    };
    let service = app.session_service();
    let request = service.cancel_session(&session_id);
    if let Err(error) = app.drive_session_request(request).await {
        warn!(
            session_id = %session_id,
            error = %error,
            "failed to cancel confirmed session"
        );
    }

    Ok(EventResult::Continue)
}

/// Creates a continuation draft for the confirmed terminal session and opens
/// its prompt composer.
async fn handle_continue_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };

    if let Err(error) = app.continue_terminal_session(&session_id).await {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);
        app.append_output_for_session(&session_id, &TranscriptNotice::ContinueError.format(error))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Creates a fork of the confirmed source session and opens the forked
/// session view.
async fn handle_fork_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };

    if let Err(error) = app.fork_session(&session_id).await {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);
        app.append_output_for_session(&session_id, &TranscriptNotice::ForkError.format(error))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Restores view mode and attempts to add confirmed session to merge queue.
async fn handle_merge_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

    let Some(session_id) = confirmation_session_id else {
        return Ok(EventResult::Continue);
    };
    let service = app.session_service();
    let request = service.merge_session(&session_id);
    if let Err(error) = app.drive_session_request(request).await {
        app.append_output_for_session(&session_id, &TranscriptNotice::MergeError.format(error))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Clears focused review cache state, requests the diff in the background,
/// then restores session view with responsive loading state.
fn handle_regenerate_review_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> EventResult {
    let Some(session_id) = confirmation_session_id else {
        app.mode = AppMode::List;

        return EventResult::Continue;
    };

    app.clear_review_output(session_id.as_str());

    if !app
        .sessions
        .sessions()
        .iter()
        .any(|session| session.id == session_id)
    {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return EventResult::Continue;
    }

    app.start_manual_review_diff_load(&session_id);

    let view_mode = restore_view.unwrap_or(ConfirmationViewMode {
        scroll_offset: None,
        session_id,
    });
    app.mode = view_mode.into_view_mode();

    EventResult::Continue
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use mockall::predicate::eq;

    use super::*;
    use crate::domain::orchestration::OrchestrationStatus;
    use crate::domain::session::SessionHandles;
    use crate::domain::session_message::SessionTranscript;
    use crate::infra::tmux::MockTmuxClient;
    use crate::presentation::app_mode::{
        ConfirmationViewMode, DiffCommentTarget, DiffFocus, DiffLineCommentAnchor,
        DiffLineCommentTarget, DiffLineComments, DiffLineSide, DiffPreview, DiffRestoreTarget,
        DiffReviewComments, DiffSidebarFocus, PromptModeSnapshot,
    };
    use crate::presentation::prompt::{
        PromptAttachmentState, PromptHistoryState, PromptSlashState,
    };

    fn session_replay_text(session: &crate::domain::session::Session) -> String {
        session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .unwrap_or_default()
    }

    async fn appendable_stack_test_app() -> (App, tempfile::TempDir, String, String) {
        let (mut app, base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let parent_session_id = app.create_session().await.expect("failed to create parent");
        let source_session_id = app.create_session().await.expect("failed to create source");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &parent_session_id,
            crate::domain::session::Status::Review,
        );
        crate::test_support::set_session_status_for_test(
            &mut app,
            &source_session_id,
            crate::domain::session::Status::Review,
        );

        (app, base_dir, parent_session_id, source_session_id)
    }

    #[test]
    fn test_content_area_for_terminal_excludes_global_bars() {
        // Arrange
        let terminal_rect = Rect::new(0, 0, 120, 30);

        // Act
        let content_area = content_area_for_terminal(terminal_rect);

        // Assert
        assert_eq!(content_area, Rect::new(0, 1, 120, 28));
    }

    #[tokio::test]
    async fn test_handle_session_creation_key_creates_each_root_session_type() {
        for (selected_option_index, is_draft, role) in [
            (0, false, ag_session::SessionRole::Worker),
            (1, true, ag_session::SessionRole::Worker),
            (2, false, ag_session::SessionRole::Orchestrator),
        ] {
            // Arrange
            let (mut app, _base_dir) =
                crate::test_support::new_git_test_app_with_mock_tmux_client().await;
            app.mode = AppMode::SessionCreation {
                selected_option_index: 0,
            };
            update_session_creation_selection(&mut app, selected_option_index);

            // Act
            let result = handle_session_creation_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            )
            .await;

            // Assert
            assert!(matches!(result, Ok(EventResult::Continue)));
            assert_eq!(app.sessions.sessions().len(), 1);
            assert_eq!(app.sessions.sessions()[0].is_draft_session(), is_draft);
            assert_eq!(app.sessions.sessions()[0].role, role);
            assert!(matches!(
                app.mode,
                AppMode::Prompt {
                    ref session_id,
                    scroll_offset: None,
                    ..
                } if !session_id.is_empty()
            ));
        }
    }

    #[tokio::test]
    async fn test_session_creation_rejection_stays_in_terminal_ui() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let project_id = app.active_project_id();
        app.project_sync_status = Some(crate::app::ProjectSyncStatus {
            context: crate::app::ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id,
                project_name: "agentty".to_string(),
            },
            phase: crate::app::ProjectSyncPhase::Running,
        });
        app.mode = AppMode::SessionCreation {
            selected_option_index: 0,
        };

        // Act
        let result = handle_session_creation_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: false,
                ref message,
                ref title,
                ..
            } if title == "Session creation unavailable"
                && message.contains("is synchronizing `main`")
        ));
    }

    #[tokio::test]
    async fn test_pre_commit_warning_enter_opens_session_creation_options() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::PreCommitHookWarning {
            message: "Missing pre-commit hook".to_string(),
        };

        // Act
        let result = handle_pre_commit_hook_warning_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(
            app.mode,
            AppMode::SessionCreation {
                selected_option_index: 0,
            }
        ));
    }

    #[tokio::test]
    async fn test_pre_commit_warning_escape_returns_to_session_list() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::PreCommitHookWarning {
            message: "Missing pre-commit hook".to_string(),
        };

        // Act
        let result = handle_pre_commit_hook_warning_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_session_creation_key_creates_stacked_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let parent_session_id = app
            .create_session()
            .await
            .expect("parent session should be created");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &parent_session_id,
            crate::domain::session::Status::Review,
        );
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::SessionCreation {
            selected_option_index: 2,
        };

        // Act
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .expect("failed to select stacked session");
        let result = handle_session_creation_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        let child_session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| {
                session.parent_session_id.as_deref() == Some(parent_session_id.as_str())
            })
            .expect("stacked child should be created");
        let child_session_id = child_session.id.clone();
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(child_session.is_draft_session());
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == &child_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_session_creation_key_opens_parent_selector_for_review_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let parent_session_id = app.create_session().await.expect("failed to create parent");
        let source_session_id = app.create_session().await.expect("failed to create source");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &parent_session_id,
            crate::domain::session::Status::Review,
        );
        crate::test_support::set_session_status_for_test(
            &mut app,
            &source_session_id,
            crate::domain::session::Status::AgentReview,
        );
        let source_index = app
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == source_session_id)
            .expect("source session should exist");
        app.sessions.select_session_index(Some(source_index));
        app.mode = AppMode::SessionCreation {
            selected_option_index: 3,
        };

        // Act
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .expect("failed to select append option");
        let result = handle_session_creation_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::StackAppendParentSelection {
                selected_parent_index: 0,
                ref session_id,
            } if session_id.as_str() == source_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_key_event_routes_stack_parent_escape_to_creation_selector() {
        // Arrange
        let (mut app, _base_dir, _parent_session_id, source_session_id) =
            appendable_stack_test_app().await;
        app.mode = AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            session_id: source_session_id.into(),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::SessionCreation {
                selected_option_index: 4,
            }
        ));
    }

    #[tokio::test]
    async fn test_session_creation_navigation_moves_up_and_clamps_disabled_options() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::SessionCreation {
            selected_option_index: 4,
        };

        // Act
        update_session_creation_selection(&mut app, 4);
        let clamped_selection = current_session_creation_selection(&app);
        let result =
            handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
                .await;
        let moved_selection = current_session_creation_selection(&app);
        let unknown_option_enabled = session_creation_option_is_enabled(&app, usize::MAX);
        app.mode = AppMode::SessionCreation {
            selected_option_index: 4,
        };
        let disabled_append_result = handle_session_creation_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert_eq!(clamped_selection, 2);
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert_eq!(moved_selection, 1);
        assert!(!unknown_option_enabled);
        assert!(matches!(disabled_append_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::SessionCreation {
                selected_option_index: 4,
            }
        ));
    }

    #[tokio::test]
    async fn test_stack_parent_selector_appends_session_and_returns_to_list() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let parent_session_id = app.create_session().await.expect("failed to create parent");
        let source_session_id = app.create_session().await.expect("failed to create source");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &parent_session_id,
            crate::domain::session::Status::Review,
        );
        crate::test_support::set_session_status_for_test(
            &mut app,
            &source_session_id,
            crate::domain::session::Status::Review,
        );
        app.mode = AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            session_id: source_session_id.clone().into(),
        };

        // Act
        let result = handle_stack_append_parent_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        let source_session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == source_session_id)
            .expect("source session should remain loaded");
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
        assert_eq!(
            source_session.parent_session_id.as_deref(),
            Some(parent_session_id.as_str())
        );
    }

    #[tokio::test]
    async fn test_stack_parent_selector_handles_navigation_close_and_unbound_keys() {
        // Arrange
        let (mut app, _base_dir, _parent_session_id, source_session_id) =
            appendable_stack_test_app().await;
        app.mode = AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            session_id: source_session_id.clone().into(),
        };

        // Act
        for key_code in [KeyCode::Down, KeyCode::Up, KeyCode::Char('x')] {
            handle_stack_append_parent_key(&mut app, KeyEvent::new(key_code, KeyModifiers::NONE))
                .await
                .expect("parent navigation should continue");
        }
        assert!(matches!(
            app.mode,
            AppMode::StackAppendParentSelection {
                selected_parent_index: 0,
                ..
            }
        ));
        handle_stack_append_parent_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await
        .expect("parent selector should close");

        // Assert
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_stack_parent_selector_selects_parent_beyond_compact_viewport() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        for _ in 0..8 {
            let parent_session_id = app.create_session().await.expect("failed to create parent");
            crate::test_support::set_session_status_for_test(
                &mut app,
                &parent_session_id,
                crate::domain::session::Status::Review,
            );
        }
        let source_session_id = app.create_session().await.expect("failed to create source");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &source_session_id,
            crate::domain::session::Status::Review,
        );
        app.mode = AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            session_id: source_session_id.clone().into(),
        };
        let eligible_parent_ids =
            stack_append_parent_session_ids(&app, &SessionId::from(source_session_id.as_str()));
        let expected_parent_id = eligible_parent_ids
            .last()
            .expect("expected eligible parents")
            .clone();

        // Act
        for _ in 1..eligible_parent_ids.len() {
            handle_stack_append_parent_key(
                &mut app,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            )
            .await
            .expect("failed to move parent selection");
        }
        handle_stack_append_parent_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to append to selected parent");
        let source_session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == source_session_id)
            .expect("source session should remain loaded");

        // Assert
        assert_eq!(
            source_session.parent_session_id.as_deref(),
            Some(expected_parent_id.as_str())
        );
    }

    #[tokio::test]
    async fn test_stack_parent_helpers_ignore_inactive_or_empty_selection() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;

        // Act
        update_stack_append_parent_selection(&mut app, true);
        append_session_to_selected_stack(&mut app).await;
        app.mode = AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            session_id: "missing-source".into(),
        };
        let result = handle_stack_append_parent_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_stack_parent_selector_surfaces_sync_start_failure() {
        // Arrange
        let (mut app, _base_dir, _parent_session_id, source_session_id) =
            appendable_stack_test_app().await;
        app.sessions
            .session_handles_mut()
            .remove(source_session_id.as_str());
        app.mode = AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            session_id: source_session_id.into(),
        };

        // Act
        let result = handle_stack_append_parent_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup { ref title, .. } if title == "Append to stack failed"
        ));
    }

    #[tokio::test]
    async fn test_session_creation_skips_append_option_for_non_review_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let parent_session_id = app.create_session().await.expect("failed to create parent");
        let source_session_id = app.create_session().await.expect("failed to create source");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &parent_session_id,
            crate::domain::session::Status::Review,
        );
        crate::test_support::set_session_status_for_test(
            &mut app,
            &source_session_id,
            crate::domain::session::Status::InProgress,
        );
        let source_index = app
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == source_session_id)
            .expect("source session should exist");
        app.sessions.select_session_index(Some(source_index));
        app.mode = AppMode::SessionCreation {
            selected_option_index: 3,
        };

        // Act
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .expect("failed to navigate creation options");

        // Assert
        assert_eq!(current_session_creation_selection(&app), 3);
    }

    #[tokio::test]
    async fn test_handle_session_creation_key_escape_returns_to_list() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::SessionCreation {
            selected_option_index: 0,
        };

        // Act
        let result =
            handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    /// Builds one in-memory project row for project switcher handler tests.
    fn switcher_project_item(
        project_id: i64,
        name: &str,
        path: std::path::PathBuf,
        last_opened_at: Option<i64>,
    ) -> crate::domain::project::ProjectListItem {
        crate::domain::project::ProjectListItem {
            active_session_count: 0,
            input_tokens: 0,
            last_session_updated_at: None,
            output_tokens: 0,
            project: crate::domain::project::Project {
                created_at: 0,
                display_name: Some(name.to_string()),
                git_branch: None,
                id: project_id,
                is_favorite: false,
                last_opened_at,
                path,
                updated_at: 0,
            },
            session_count: 0,
        }
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_escape_returns_to_list() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.mode = AppMode::ProjectSwitcher {
            selected_option_index: 0,
        };

        // Act
        let result =
            handle_project_switcher_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_navigation_clamps_to_project_count() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        let second_project = switcher_project_item(999, "beta", base_dir.path().join("beta"), None);
        let active_project = switcher_project_item(
            app.active_project_id(),
            "alpha",
            app.projects.working_dir().to_path_buf(),
            Some(20),
        );
        app.projects
            .replace_project_items(vec![active_project, second_project]);
        app.mode = AppMode::ProjectSwitcher {
            selected_option_index: 0,
        };

        // Act
        for _ in 0..3 {
            handle_project_switcher_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            )
            .await
            .expect("failed to move selection down");
        }
        let clamped_down_index = match app.mode {
            AppMode::ProjectSwitcher {
                selected_option_index,
            } => selected_option_index,
            _ => usize::MAX,
        };
        handle_project_switcher_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to move selection up");

        // Assert
        assert_eq!(clamped_down_index, 1);
        assert!(matches!(
            app.mode,
            AppMode::ProjectSwitcher {
                selected_option_index: 0,
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_enter_switches_to_selected_project() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        let second_project_dir = base_dir.path().join("beta-project");
        std::fs::create_dir_all(&second_project_dir).expect("failed to create second project dir");
        let second_project_path = second_project_dir
            .canonicalize()
            .expect("failed to canonicalize second project dir");
        let second_project_id = app
            .services
            .db()
            .projects()
            .upsert_project(&second_project_path.to_string_lossy(), None)
            .await
            .expect("failed to seed second project");
        let active_project = switcher_project_item(
            app.active_project_id(),
            "alpha",
            app.projects.working_dir().to_path_buf(),
            Some(20),
        );
        let second_project =
            switcher_project_item(second_project_id, "beta-project", second_project_path, None);
        app.projects
            .replace_project_items(vec![active_project, second_project]);
        app.mode = AppMode::ProjectSwitcher {
            selected_option_index: 1,
        };

        // Act
        let result = handle_project_switcher_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert_eq!(app.active_project_id(), second_project_id);
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_enter_surfaces_switch_failure() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        let missing_project_id = 987_654;
        let active_project = switcher_project_item(
            app.active_project_id(),
            "alpha",
            app.projects.working_dir().to_path_buf(),
            Some(20),
        );
        let missing_project = switcher_project_item(
            missing_project_id,
            "beta-project",
            base_dir.path().join("beta-project"),
            None,
        );
        app.projects
            .replace_project_items(vec![active_project, missing_project]);
        app.mode = AppMode::ProjectSwitcher {
            selected_option_index: 1,
        };

        // Act
        let result = handle_project_switcher_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert_ne!(app.active_project_id(), missing_project_id);

        let missing_project_id_text = missing_project_id.to_string();
        assert!(matches!(
            &app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: false,
                message,
                project_name,
                title,
                ..
            } if title == "Project switch failed"
                && project_name.as_deref() == Some("beta-project")
                && message.contains(&missing_project_id_text)
        ));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_enter_on_active_project_only_closes_popup() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let active_project_id = app.active_project_id();
        app.mode = AppMode::ProjectSwitcher {
            selected_option_index: 0,
        };

        // Act
        let result = handle_project_switcher_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert_eq!(app.active_project_id(), active_project_id);
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_view_info_popup_key_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::ViewInfoPopup {
            is_loading: false,
            loading_label: "Refreshing review request...".to_string(),
            message: "Review request refreshed.".to_string(),
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(2),
                session_id: "session-id".into(),
            },
            title: "Review request refreshed".to_string(),
        };

        // Act
        let event_result =
            handle_view_info_popup_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_quits_when_no_session_context() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::Quit,
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            restore_view: None,
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Quit)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_reject_and_cancel_return_to_list() {
        for decision in [ConfirmationDecision::Reject, ConfirmationDecision::Cancel] {
            // Arrange
            let (mut app, _base_dir) =
                crate::test_support::new_test_app_with_mock_tmux_client().await;
            app.mode = AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                confirmation_message: "Quit agentty?".to_string(),
                confirmation_title: "Confirm Quit".to_string(),
                restore_view: None,
                session_id: None,
                selected_confirmation_index: 0,
            };

            // Act
            let event_result = handle_confirmation_decision(&mut app, decision).await;

            // Assert
            assert!(matches!(event_result, Ok(EventResult::Continue)));
            assert!(matches!(app.mode, AppMode::List));
        }
    }

    #[tokio::test]
    async fn integration_choice_persists_local_merge_or_review_request() {
        for (decision, expected_approach) in [
            (
                ConfirmationDecision::Confirm,
                IntegrationApproach::LocalMerge,
            ),
            (
                ConfirmationDecision::Reject,
                IntegrationApproach::ReviewRequest,
            ),
        ] {
            // Arrange
            let (mut app, _base_dir) =
                crate::test_support::new_git_test_app_with_mock_tmux_client().await;
            let session_id = app
                .create_session()
                .await
                .expect("failed to create controller fixture");
            app.services
                .db()
                .orchestrations()
                .insert_orchestration(
                    &session_id,
                    &OrchestrationStatus::AwaitingIntegration.to_string(),
                    2,
                )
                .await
                .expect("failed to insert orchestration");
            app.mode = AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
                confirmation_message: "Choose integration".to_string(),
                confirmation_title: "Integration Approach".to_string(),
                restore_view: Some(ConfirmationViewMode {
                    scroll_offset: Some(3),
                    session_id: session_id.clone().into(),
                }),
                session_id: Some(session_id.clone().into()),
                selected_confirmation_index: 0,
            };

            // Act
            let event_result = handle_confirmation_decision(&mut app, decision).await;
            let orchestration = app
                .services
                .db()
                .orchestrations()
                .load_orchestration_for_controller(&session_id)
                .await
                .expect("failed to load orchestration")
                .expect("orchestration should exist");
            let approach = app
                .services
                .db()
                .orchestrations()
                .load_orchestration_integration_approach(orchestration.id)
                .await
                .expect("failed to load integration approach");

            // Assert
            assert!(matches!(event_result, Ok(EventResult::Continue)));
            assert_eq!(approach, expected_approach.to_string());
            assert_eq!(
                orchestration.status,
                OrchestrationStatus::Integrating.to_string()
            );
            assert!(matches!(
                app.mode,
                AppMode::View {
                    scroll_offset: Some(3),
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn integration_choice_without_session_returns_to_restore_target() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
            confirmation_message: "Choose integration".to_string(),
            confirmation_title: "Integration Approach".to_string(),
            restore_view: None,
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_cancels_session_when_context_exists() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            crate::domain::session::Status::Review,
        );
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::CancelSession,
            confirmation_message: "Cancel session \"test\"?".to_string(),
            confirmation_title: "Confirm Cancel".to_string(),
            restore_view: None,
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
        app.sessions.sync_from_handles();
        assert!(matches!(
            app.sessions.sessions().first(),
            Some(session) if session.id == session_id
                && session.status == crate::domain::session::Status::Canceled
        ));
    }

    #[tokio::test]
    async fn test_cancel_session_confirmation_tolerates_a_missing_session() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

        // Act
        let event_result =
            handle_cancel_session_confirmation(&mut app, Some("missing-session".into())).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_session_confirmation_handlers_accept_no_selected_session() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

        // Act
        let cancel_result = handle_cancel_session_confirmation(&mut app, None).await;
        let merge_result = handle_merge_confirmation(&mut app, None, None).await;
        let open_result = handle_open_managed_worktree_confirmation(&mut app, None, None).await;
        let regenerate_result = handle_regenerate_review_confirmation(&mut app, None, None);
        let missing_regenerate_result = handle_regenerate_review_confirmation(
            &mut app,
            Some("missing-session".into()),
            Some(ConfirmationViewMode {
                scroll_offset: Some(5),
                session_id: "restore-session".into(),
            }),
        );

        // Assert
        assert!(matches!(cancel_result, Ok(EventResult::Continue)));
        assert!(matches!(merge_result, Ok(EventResult::Continue)));
        assert!(matches!(open_result, Ok(EventResult::Continue)));
        assert!(matches!(regenerate_result, EventResult::Continue));
        assert!(matches!(missing_regenerate_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(5),
            } if session_id == "restore-session"
        ));
    }

    #[tokio::test]
    async fn managed_worktree_confirmation_validates_target_then_opens_selector() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        let restore_view = ConfirmationViewMode {
            scroll_offset: Some(3),
            session_id: session_id.clone().into(),
        };

        // Act
        let mismatched_result = handle_open_managed_worktree_confirmation(
            &mut app,
            Some(SessionId::from("other-session")),
            Some(restore_view.clone()),
        )
        .await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::OpenManagedWorktree,
            confirmation_message: "Open?".to_string(),
            confirmation_title: "Open Managed Worktree".to_string(),
            restore_view: Some(restore_view),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };
        let confirmed_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(mismatched_result, Ok(EventResult::Continue)));
        assert!(matches!(confirmed_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                ref commands,
                ref restore_view,
                selected_command_index: 0,
            } if commands == &["cargo test".to_string(), "npm run dev".to_string()]
                && restore_view.session_id == session_id
                && restore_view.scroll_offset == Some(3)
        ));
    }

    #[tokio::test]
    async fn test_handle_key_event_routes_done_session_continue_shortcut() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create source session");
        app.services
            .db()
            .sessions()
            .update_session_merged_commit_hash(&source_session_id, Some("abc1234".to_string()))
            .await
            .expect("failed to persist merged commit hash");
        let source_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == source_session_id)
            .expect("expected source session");
        source_session.status = crate::domain::session::Status::Done;
        source_session.title = Some("Done source".to_string());
        app.mode = AppMode::View {
            session_id: source_session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let event_result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ContinueSession,
                ref confirmation_title,
                ref restore_view,
                ref session_id,
                ..
            } if confirmation_title == "Confirm Continue"
                && matches!(restore_view, Some(restore_view) if restore_view.session_id == source_session_id)
                && matches!(session_id, Some(session_id) if session_id.as_str() == source_session_id)
        ));
    }

    #[tokio::test]
    async fn test_diff_comment_queue_cancel_restores_comments() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            crate::domain::session::Status::InProgress,
        );
        app.sessions.session_handles_mut().insert(
            session_id.clone().into(),
            SessionHandles::new(crate::domain::session::Status::InProgress),
        );
        let mut line_comments = DiffLineComments::default();
        line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
        line_comments
            .editing_input_mut()
            .expect("file comment should be editable")
            .insert_text("Keep this queued comment");
        line_comments.finish_editing();
        app.mode = AppMode::Diff {
            diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            selected_diff_line_index: 0,
            session_id: session_id.clone().into(),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act — submit from Diff while another turn runs, then retract the
        // queued prompt.
        handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("diff comment submission should succeed");
        handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await
        .expect("queued comment cancellation should succeed");
        mode::diff::enter_diff_mode(
            &mut app,
            &session_id,
            "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
            None,
            DiffSidebarFocus::Files,
        );

        // Assert
        let queued_messages = app
            .sessions
            .session_handles()
            .get(session_id.as_str())
            .expect("session handles should remain available")
            .queued_messages
            .lock()
            .expect("queued message lock should remain available");
        assert!(queued_messages.is_empty());
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.prompt_text().contains("Keep this queued comment")
        ));
        assert!(!app.diff_comment_progress.contains_key(session_id.as_str()));
    }

    #[tokio::test]
    async fn test_handle_key_event_routes_question_input_through_terminal_bounds() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            current_index: 0,
            focus: crate::presentation::app_mode::ChatFocus::Input,
            input: crate::domain::input::InputState::default(),
            questions: vec![crate::domain::question::QuestionItem::new("Which branch?")],
            responses: Vec::new(),
            scroll_offset: None,
            selected_option_index: None,
            session_id: "session-id".into(),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let event_result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::Question { ref input, .. } if input.text() == "x"
        ));
    }

    #[tokio::test]
    async fn test_handle_key_event_routes_loading_diff_cancel() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::DiffLoading {
            fallback_view_scroll_offset: Some(3),
            request_id: 1,
            restore: None,
            session_id: "session-id".into(),
            sidebar_focus: DiffSidebarFocus::Files,
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let event_result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(3),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_key_event_routes_review_comment_escape_to_files() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..DiffReviewComments::loading(1)
            }),
            restore: None,
            scroll_cache: None,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let event_result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    sidebar_focus: DiffSidebarFocus::Files,
                    ..
                }),
                ref session_id,
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_key_event_submits_completed_diff_comments() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_test_app().await;
        let session = crate::test_support::SessionFixtureBuilder::new()
            .id("session-id")
            .folder(base_dir.path().to_path_buf())
            .status(crate::domain::session::Status::Review)
            .build();
        app.sessions =
            crate::test_support::session_manager_with_handles(vec![session], HashMap::new()).into();
        let mut line_comments = DiffLineComments::default();
        line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
            content: "review();".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        }));
        line_comments
            .editing_input_mut()
            .expect("seeded comment should be editable")
            .insert_text("Explain this call");
        line_comments.finish_editing();
        app.mode = AppMode::Diff {
            diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments,
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..DiffReviewComments::loading(1)
            }),
            restore: Some(Box::new(DiffRestoreTarget::Prompt(PromptModeSnapshot {
                at_mention_state: None,
                attachment_state: PromptAttachmentState::default(),
                history_state: PromptHistoryState::default(),
                input: crate::domain::input::InputState::with_text("/keep draft".to_string()),
                scroll_offset: None,
                session_id: "session-id".into(),
                slash_state: PromptSlashState::default(),
            }))),
            scroll_cache: None,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let event_result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::View { .. }));

        // Arrange
        app.mode = AppMode::Diff {
            diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
            file_explorer_selected_index: 1,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };
        app.clear_redraw();
        assert!(!app.needs_redraw());

        // Act
        let no_submit_result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(no_submit_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::Diff { .. }));
        assert!(app.needs_redraw());
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_merge_confirmation() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            confirmation_message: "Add this session to merge queue?".to_string(),
            confirmation_title: "Confirm Merge".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(6),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref session_id_in_mode,
                scroll_offset: Some(6),
            } if session_id_in_mode == &session_id
        ));
    }

    #[tokio::test]
    async fn detach_confirmation_restores_view_with_or_without_a_session_target() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::DetachManagedSession,
            confirmation_message: "Detach?".to_string(),
            confirmation_title: "Confirm Detach".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(3),
                session_id: SessionId::from("worker"),
            }),
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let without_target =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(without_target, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                scroll_offset: Some(3),
                ref session_id,
            } if session_id == "worker"
        ));

        // Arrange
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::DetachManagedSession,
            confirmation_message: "Detach?".to_string(),
            confirmation_title: "Confirm Detach".to_string(),
            restore_view: None,
            session_id: Some(SessionId::from("missing-worker")),
            selected_confirmation_index: 0,
        };
        let with_target =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(with_target, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_continue_confirmation() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            confirmation_message: "Create a new draft session with initial context from this \
                                   session?"
                .to_string(),
            confirmation_title: "Confirm Continue".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 1,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref session_id_in_mode,
                scroll_offset: Some(4),
                ..
            } if session_id_in_mode == &session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_opens_continuation_draft_prompt() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.services
            .db()
            .sessions()
            .update_session_merged_commit_hash(
                &source_session_id,
                Some(merged_commit_hash.to_string()),
            )
            .await
            .expect("failed to persist merged commit hash");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &source_session_id,
            crate::domain::session::Status::Done,
        );
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            confirmation_message: "Create a new draft session with initial context from this \
                                   session?"
                .to_string(),
            confirmation_title: "Confirm Continue".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: source_session_id.clone().into(),
            }),
            session_id: Some(source_session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref input,
                ref session_id,
                ..
            } if session_id.as_str() != source_session_id
                && input.text().is_empty()
        ));
        let continued_session_id = match &app.mode {
            AppMode::Prompt { session_id, .. } => session_id.as_str().to_string(),
            _ => unreachable!("expected prompt mode"),
        };
        let continued_session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == continued_session_id)
            .expect("expected created continuation draft");
        assert_eq!(
            continued_session.prompt,
            format!("Use {merged_commit_hash} commit as an initial context for this session")
        );
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_queues_merge_with_view_restore() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            confirmation_message: "Add this session to merge queue?".to_string(),
            confirmation_title: "Confirm Merge".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(2),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref session_id_in_mode,
                scroll_offset: Some(2),
            } if session_id_in_mode == &session_id
        ));
        app.sessions.sync_from_handles();
        let output = session_replay_text(&app.sessions.sessions()[0]);
        assert!(output.contains("[Merge Error]"));
    }

    #[tokio::test]
    async fn test_merge_confirmation_accepts_an_already_active_merge() {
        // Arrange
        let base_dir = tempfile::tempdir().expect("failed to create base dir");
        let project_dir = tempfile::tempdir().expect("failed to create project dir");
        crate::test_support::setup_test_git_repo(project_dir.path());
        let repositories = crate::infra::db::AppRepositories::in_memory()
            .await
            .expect("db should open");
        let clients = crate::test_support::test_app_clients_with_mock_app_server()
            .with_tmux_client(Arc::new(MockTmuxClient::new()));
        let mut app = App::new_with_clients(
            base_dir.path().to_path_buf(),
            project_dir.path().to_path_buf(),
            Some("main".to_string()),
            repositories,
            clients,
        )
        .await
        .expect("failed to create app");
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            crate::domain::session::Status::Review,
        );
        app.merge_session(&session_id)
            .await
            .expect("merge should become active");

        // Act
        let event_result = handle_merge_confirmation(&mut app, Some(session_id.into()), None).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_key_event_routes_publish_branch_input() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(7),
                session_id: "session-id".into(),
            },
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let event_result = handle_key_event(
            &mut app,
            &PresentationState::default(),
            &mut terminal,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(7),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_escape_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(7),
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(7),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_enter_starts_pull_request_publish() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            crate::domain::session::Status::Review,
        );
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::PublishPullRequest,
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: session_id.clone().into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref viewed_session_id,
                scroll_offset: Some(4),
            } if viewed_session_id == &session_id
        ));
        assert_eq!(
            app.sessions
                .session_at(0)
                .and_then(|session| {
                    session
                        .transient_messages
                        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
                })
                .map(|message| message.body.text()),
            Some("Publishing review request...")
        );
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_char_updates_input_state() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::default(),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                input: ref input_state,
                ..
            } if input_state.cursor == 1 && input_state.text() == "r"
        ));
        let AppMode::PublishBranchInput { input, .. } = &app.mode else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(input.text(), "r");
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_shortcut_chars_are_inserted() {
        // Arrange
        let typed_shortcut_characters = ['q', 'p', 'd', 'f', 'm', 'r', 'j', 'k', 'g', 'G', '?'];
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;

        for character in typed_shortcut_characters {
            app.mode = AppMode::PublishBranchInput {
                default_branch_name: "wt/session".to_string(),
                input: crate::domain::input::InputState::default(),
                locked_upstream_ref: None,
                publish_branch_action: crate::domain::session::PublishBranchAction::Push,
                restore_view: ConfirmationViewMode {
                    scroll_offset: None,
                    session_id: "session-id".into(),
                },
            };
            let modifiers = if character.is_ascii_uppercase() || character == '?' {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };

            // Act
            let event_result = handle_publish_branch_input_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), modifiers),
            )
            .await;

            // Assert
            assert!(matches!(event_result, EventResult::Continue));
            assert!(matches!(
                app.mode,
                AppMode::PublishBranchInput {
                    input: ref input_state,
                    ..
                } if input_state.cursor == 1 && input_state.text() == character.to_string()
            ));
        }
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_left_moves_cursor() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        let AppMode::PublishBranchInput { input, .. } = &app.mode else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(input.text(), "review/custom");
        assert_eq!(input.cursor, "review/custom".chars().count() - 1);
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_supports_word_delete_undo_and_redo() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        )
        .await;
        handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        )
        .await;
        assert!(matches!(
            &app.mode,
            AppMode::PublishBranchInput { input, .. } if input.text() == "review custom"
        ));
        handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::PublishBranchInput { input, .. } if input.text() == "review"
        ));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_preserves_mode_for_unmapped_key() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        let result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(
            &app.mode,
            AppMode::PublishBranchInput { input, .. } if input.text() == "review/custom"
        ));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_char_keeps_locked_branch_name() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: Some("origin/review/custom".to_string()),
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        let AppMode::PublishBranchInput {
            input,
            locked_upstream_ref,
            ..
        } = &app.mode
        else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(locked_upstream_ref.as_deref(), Some("origin/review/custom"));
        assert_eq!(input.text(), "review/custom");
    }

    #[test]
    fn test_next_launch_configuration_index_wraps_to_start() {
        // Arrange
        let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

        // Act
        let index = next_launch_configuration_index(1, &commands);

        // Assert
        assert_eq!(index, 0);
    }

    #[test]
    fn test_previous_launch_configuration_index_wraps_to_end() {
        // Arrange
        let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

        // Act
        let index = previous_launch_configuration_index(0, &commands);

        // Assert
        assert_eq!(index, 1);
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_escape_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(3),
                session_id: "session-id".into(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(3),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_j_updates_selected_index() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                selected_command_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_with_empty_commands_keeps_index_zero() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: Vec::new(),
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                selected_command_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_enter_restores_view_without_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: "session-id".into(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(4),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_enter_runs_selected_command_in_tmux() {
        // Arrange
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client
            .expect_open_window_for_folder()
            .times(1)
            .returning(|_| Box::pin(async { Some("@24".to_string()) }));
        mock_tmux_client
            .expect_run_command_in_window()
            .with(eq("@24".to_string()), eq("npm run dev".to_string()))
            .times(1)
            .returning(|_, _| Box::pin(async {}));
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_tmux_client(Arc::new(mock_tmux_client))
                .await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(2),
                session_id: expected_session_id.clone().into(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(2),
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_unknown_key_preserves_state() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(1),
                session_id: "session-id".into(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                selected_command_index: 1,
                ref commands,
                restore_view:
                    ConfirmationViewMode {
                        scroll_offset: Some(1),
                        ref session_id,
                    },
            } if commands == &vec!["cargo test".to_string(), "npm run dev".to_string()]
                && session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_regenerate_confirmation() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 1,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                scroll_offset: Some(4),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_regenerates_review() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_folder = app.sessions.sessions()[0].folder.clone();
        std::fs::write(session_folder.join("README.md"), "regenerate test\n")
            .expect("failed to write");
        app.review_cache.insert(
            session_id.clone().into(),
            ReviewCacheEntry::Ready {
                text: "Old review".to_string(),
                diff_hash: 99,
            },
        );
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: None,
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert — view is restored with loading state, cache shows new Loading
        // entry
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::View { .. }));
        assert!(matches!(
            app.review_cache.get(session_id.as_str()),
            Some(ReviewCacheEntry::Loading { .. })
        ));
    }
}
