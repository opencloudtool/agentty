use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{App, SessionStatsUsage};
use crate::domain::agent::{self, AgentKind};
use crate::domain::input::InputState;
use crate::infra::channel::{TurnPrompt, TurnPromptAttachment};
use crate::runtime::mode::{at_mention, input_key};
use crate::runtime::{EventResult, clipboard_image};
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode};
use crate::ui::state::prompt::{PromptAtMentionState, PromptSlashStage};
use crate::ui::util::{format_token_count, move_input_cursor_down, move_input_cursor_up};

struct PromptContext {
    can_delete_on_cancel: bool,
    is_at_mention: bool,
    is_draft_session: bool,
    is_new_session: bool,
    is_slash_command: bool,
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

/// Handles key input while the app is in `AppMode::Prompt`.
pub(crate) async fn handle<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let Some(prompt_context) = prompt_context(app) else {
        return Ok(EventResult::Continue);
    };

    if !prompt_context.is_slash_command {
        reset_prompt_slash_state(app);
    }

    if prompt_context.is_at_mention && handle_at_mention_key(app, key) {
        return Ok(EventResult::Continue);
    }

    handle_editing_key(app, terminal, key, &prompt_context).await?;

    Ok(EventResult::Continue)
}

/// Handles keys when the at-mention dropdown is active.
///
/// Returns `true` if the key was consumed by at-mention logic.
fn handle_at_mention_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => dismiss_at_mention(app),
        KeyCode::Enter if !input_key::should_insert_newline(key) => handle_at_mention_select(app),
        KeyCode::Tab => handle_at_mention_select(app),
        KeyCode::Up => handle_at_mention_up(app),
        KeyCode::Down => handle_at_mention_down(app),
        _ => return false,
    }

    true
}

/// Handles all editing, navigation, and submission keys in prompt mode.
async fn handle_editing_key<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
    prompt_context: &PromptContext,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    match key.code {
        KeyCode::Enter | KeyCode::Char('\r' | '\n') if input_key::should_insert_newline(key) => {
            reset_prompt_history_navigation(app);
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.insert_newline();
            }
        }
        KeyCode::Enter => handle_prompt_submit_key(app, prompt_context).await,
        KeyCode::Esc | KeyCode::Char('c') if is_prompt_cancel_key(key) => {
            handle_prompt_cancel_key(app, prompt_context).await;
        }
        KeyCode::Left => handle_prompt_left(app, key),
        KeyCode::Right => handle_prompt_right(app, key),
        KeyCode::Up => handle_prompt_up_key(app, terminal, prompt_context)?,
        KeyCode::Down => handle_prompt_down_key(app, terminal, prompt_context)?,
        KeyCode::Char('k') if prompt_context.is_slash_command && is_plain_char_key(key, 'k') => {
            handle_prompt_up_key(app, terminal, prompt_context)?;
        }
        KeyCode::Char('j') if prompt_context.is_slash_command && is_plain_char_key(key, 'j') => {
            handle_prompt_down_key(app, terminal, prompt_context)?;
        }
        KeyCode::Home => handle_prompt_input(app, InputState::move_home),
        KeyCode::End => handle_prompt_input(app, InputState::move_end),
        KeyCode::Backspace => handle_prompt_backspace(app, key),
        KeyCode::Delete => handle_prompt_delete(app),
        KeyCode::Char(character) if input_key::is_control_newline_key(key, character) => {
            reset_prompt_history_navigation(app);
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.insert_newline();
            }
        }
        KeyCode::Char('u') if input_key::is_control_key(key) => handle_prompt_line_delete(app),
        KeyCode::Char('v') if is_prompt_image_paste_key(key) => {
            handle_prompt_image_paste(app, prompt_context).await;
        }
        KeyCode::Char('a') if input_key::is_control_key(key) => {
            handle_prompt_input(app, InputState::move_line_start);
        }
        KeyCode::Char('e') if input_key::is_control_key(key) => {
            handle_prompt_input(app, InputState::move_line_end);
        }
        KeyCode::Char('f') if input_key::is_control_key(key) => {
            handle_prompt_input(app, InputState::move_right);
        }
        KeyCode::Char('b') if input_key::is_control_key(key) => {
            handle_prompt_input(app, InputState::move_left);
        }
        KeyCode::Char('p') if input_key::is_control_key(key) => {
            handle_prompt_up_key(app, terminal, prompt_context)?;
        }
        KeyCode::Char('n') if input_key::is_control_key(key) => {
            handle_prompt_down_key(app, terminal, prompt_context)?;
        }
        KeyCode::Char('d') if input_key::is_control_key(key) => handle_prompt_delete(app),
        KeyCode::Char('k') if input_key::is_control_key(key) => handle_prompt_kill_to_line_end(app),
        KeyCode::Char('w') if input_key::is_control_key(key) => handle_prompt_word_delete(app),
        KeyCode::Char('b') if input_key::is_alt_key(key) => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input_key::move_cursor_word_left(input);
            }

            sync_prompt_at_mention_state(app);
        }
        KeyCode::Char('f') if input_key::is_alt_key(key) => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input_key::move_cursor_word_right(input);
            }

            sync_prompt_at_mention_state(app);
        }
        KeyCode::Char(character) => handle_prompt_char(app, character),
        _ => {}
    }

    Ok(())
}

/// Applies one `InputState` method to the prompt input and keeps `@` mention
/// state aligned with the updated cursor location.
fn handle_prompt_input(app: &mut App, action: fn(&mut InputState)) {
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        action(input);
    }

    sync_prompt_at_mention_state(app);
}

/// Inserts pasted content into the prompt input while normalizing mixed
/// line-endings to `\n`.
pub(crate) fn handle_paste(app: &mut App, pasted_text: &str) {
    let normalized_text = input_key::normalize_pasted_text(pasted_text);
    if normalized_text.is_empty() {
        return;
    }

    if let AppMode::Prompt {
        history_state,
        input,
        slash_state,
        ..
    } = &mut app.mode
    {
        input.insert_text(&normalized_text);
        history_state.reset_navigation();
        slash_state.reset();
    }

    sync_prompt_at_mention_state(app);
}

fn prompt_context(app: &mut App) -> Option<PromptContext> {
    let (is_at_mention, is_slash_command, scroll_offset, session_id) = match &app.mode {
        AppMode::Prompt {
            at_mention_state,
            input,
            scroll_offset,
            session_id,
            ..
        } => (
            is_active_at_mention(at_mention_state.as_ref(), input),
            input.text().starts_with('/'),
            *scroll_offset,
            session_id.clone(),
        ),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    let (can_delete_on_cancel, is_draft_session, is_new_session) = app
        .sessions
        .sessions
        .get(session_index)
        .map_or((false, false, false), |session| {
            (
                session.status == crate::domain::session::Status::New
                    && !session.is_draft_session()
                    && !session.has_staged_drafts(),
                session.is_draft_session(),
                session.status == crate::domain::session::Status::New,
            )
        });

    Some(PromptContext {
        can_delete_on_cancel,
        is_at_mention,
        is_draft_session,
        is_new_session,
        is_slash_command,
        scroll_offset,
        session_id,
        session_index,
    })
}

fn is_active_at_mention(
    at_mention_state: Option<&PromptAtMentionState>,
    input: &InputState,
) -> bool {
    at_mention_state.is_some() && input.at_mention_query().is_some()
}

/// Reopens or dismisses the `@` mention dropdown to match the current prompt
/// cursor position.
///
/// This keeps previously inserted `@path` tokens editable after the user types
/// more text elsewhere and later moves the cursor back into the mention.
fn sync_prompt_at_mention_state(app: &mut App) {
    let Some(prompt_context) = prompt_context(app) else {
        return;
    };

    let sync_action = match &app.mode {
        AppMode::Prompt {
            at_mention_state,
            input,
            ..
        } => at_mention::sync_action(input, at_mention_state.as_ref()),
        _ => return,
    };

    match sync_action {
        at_mention::AtMentionSyncAction::Activate if !prompt_context.is_slash_command => {
            activate_at_mention(app, &prompt_context);
        }
        at_mention::AtMentionSyncAction::Dismiss => dismiss_at_mention(app),
        at_mention::AtMentionSyncAction::KeepOpen => {
            if let AppMode::Prompt {
                at_mention_state: Some(state),
                ..
            } = &mut app.mode
            {
                at_mention::reset_selection(state);
            }
        }
        at_mention::AtMentionSyncAction::Activate => {}
    }
}

fn reset_prompt_slash_state(app: &mut App) {
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.reset();
    }
}

fn reset_prompt_history_navigation(app: &mut App) {
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.reset_navigation();
    }
}

fn is_prompt_cancel_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc || key.modifiers.contains(event::KeyModifiers::CONTROL)
}

fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == event::KeyModifiers::NONE
}

/// Returns true when the key event should paste one clipboard image into the
/// prompt composer.
fn is_prompt_image_paste_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('v')
        && key
            .modifiers
            .intersects(event::KeyModifiers::ALT | event::KeyModifiers::CONTROL)
}

fn handle_prompt_up_key<B: Backend>(
    app: &mut App,
    terminal: &Terminal<B>,
    prompt_context: &PromptContext,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    if prompt_context.is_slash_command {
        if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
            slash_state.selected_index = slash_state.selected_index.saturating_sub(1);
        }

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        let next_cursor = move_input_cursor_up(input.text(), input_width, input.cursor);
        if next_cursor != input.cursor {
            input.cursor = next_cursor;
            sync_prompt_at_mention_state(app);

            return Ok(());
        }
    }

    navigate_prompt_history_up(app);
    sync_prompt_at_mention_state(app);

    Ok(())
}

fn handle_prompt_down_key<B: Backend>(
    app: &mut App,
    terminal: &Terminal<B>,
    prompt_context: &PromptContext,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    if prompt_context.is_slash_command {
        advance_prompt_slash_selection(app);

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        let next_cursor = move_input_cursor_down(input.text(), input_width, input.cursor);
        if next_cursor != input.cursor {
            input.cursor = next_cursor;
            sync_prompt_at_mention_state(app);

            return Ok(());
        }
    }

    navigate_prompt_history_down(app);
    sync_prompt_at_mention_state(app);

    Ok(())
}

fn navigate_prompt_history_up(app: &mut App) {
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &mut app.mode
    {
        if history_state.entries.is_empty() {
            return;
        }

        let next_index = if let Some(selected_index) = history_state.selected_index {
            selected_index.saturating_sub(1)
        } else {
            history_state.draft_text = Some(input.text().to_string());

            history_state.entries.len().saturating_sub(1)
        };

        history_state.selected_index = Some(next_index);
        *input = InputState::with_text(history_state.entries[next_index].clone());
    }
}

fn navigate_prompt_history_down(app: &mut App) {
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &mut app.mode
    {
        let Some(selected_index) = history_state.selected_index else {
            return;
        };

        if selected_index + 1 < history_state.entries.len() {
            let next_index = selected_index + 1;

            history_state.selected_index = Some(next_index);
            *input = InputState::with_text(history_state.entries[next_index].clone());

            return;
        }

        history_state.selected_index = None;
        *input = InputState::with_text(history_state.draft_text.take().unwrap_or_default());
    }
}

fn advance_prompt_slash_selection(app: &mut App) {
    let (available_agent_kinds, input_text, selected_agent, selected_index, stage) = match &app.mode
    {
        AppMode::Prompt {
            input, slash_state, ..
        } => (
            slash_state.available_agent_kinds.clone(),
            input.text().to_string(),
            slash_state.selected_agent,
            slash_state.selected_index,
            slash_state.stage,
        ),
        _ => return,
    };

    let option_count =
        prompt_slash_option_count(&input_text, stage, selected_agent, &available_agent_kinds);
    if option_count == 0 {
        return;
    }

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        let max_index = option_count.saturating_sub(1);
        slash_state.selected_index = (selected_index + 1).min(max_index);
    }
}

/// Submits the active prompt when it passes prompt-mode validation.
async fn handle_prompt_submit_key(app: &mut App, prompt_context: &PromptContext) {
    if prompt_context.is_slash_command {
        handle_prompt_slash_submit(app, prompt_context).await;

        return;
    }

    let prompt = take_submitted_turn_prompt(app);
    if prompt.is_empty() {
        return;
    }

    if prompt_context.is_draft_session {
        if let Err(error) = app
            .stage_draft_message(&prompt_context.session_id, prompt)
            .await
        {
            append_output_for_session(
                app,
                &prompt_context.session_id,
                &format!("\n[Error] {error}\n"),
            )
            .await;
        }
    } else if prompt_context.is_new_session {
        if let Err(error) = app.start_session(&prompt_context.session_id, prompt).await {
            append_output_for_session(
                app,
                &prompt_context.session_id,
                &format!("\n[Error] {error}\n"),
            )
            .await;
        }
    } else {
        app.reply(&prompt_context.session_id, prompt).await;
    }

    app.mode = AppMode::View {
        done_session_output_mode: DoneSessionOutputMode::Summary,
        review_status_message: None,
        review_text: None,
        session_id: prompt_context.session_id.clone(),
        scroll_offset: None,
    };
}

/// Pastes one clipboard image into the prompt composer as an inline
/// placeholder token.
async fn handle_prompt_image_paste(app: &mut App, prompt_context: &PromptContext) {
    let attachment_number = match &app.mode {
        AppMode::Prompt {
            attachment_state, ..
        } => attachment_state.next_attachment_number,
        _ => return,
    };

    match clipboard_image::persist_clipboard_image(&prompt_context.session_id, attachment_number)
        .await
    {
        Ok(persisted_image) => {
            insert_pasted_image_placeholder(app, persisted_image.local_image_path);
        }
        Err(error) => {
            append_prompt_status_line(
                app,
                &prompt_context.session_id,
                "Paste Image Error",
                &clipboard_image::normalize_clipboard_image_error(&error),
            )
            .await;
        }
    }
}

/// Inserts one persisted image placeholder into the prompt input and records
/// the attachment metadata in prompt state.
fn insert_pasted_image_placeholder(app: &mut App, local_image_path: std::path::PathBuf) {
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = &mut app.mode
    {
        let placeholder = attachment_state.register_local_image(local_image_path);
        input.insert_text(&placeholder);
        history_state.reset_navigation();
        slash_state.reset();
    }

    sync_prompt_at_mention_state(app);
}

/// Drains the prompt composer into the structured turn payload sent to the
/// session workflow.
///
/// Attachments are filtered against the submitted text so manually deleted
/// `[Image #n]` placeholders do not leave orphaned image inputs in the final
/// turn payload.
fn take_submitted_turn_prompt(app: &mut App) -> TurnPrompt {
    match &mut app.mode {
        AppMode::Prompt {
            attachment_state,
            input,
            ..
        } => {
            let text = input.take_text();
            let mut attachments = attachment_state
                .attachments
                .iter()
                .filter(|attachment| text.contains(&attachment.placeholder))
                .map(|attachment| TurnPromptAttachment {
                    placeholder: attachment.placeholder.clone(),
                    local_image_path: attachment.local_image_path.clone(),
                })
                .collect::<Vec<_>>();
            attachments
                .sort_by_key(|attachment| text.find(&attachment.placeholder).unwrap_or(usize::MAX));
            attachment_state.reset();

            TurnPrompt { attachments, text }
        }
        _ => TurnPrompt::from_text(String::new()),
    }
}

async fn handle_prompt_slash_submit(app: &mut App, prompt_context: &PromptContext) {
    let (available_agent_kinds, input_text, selected_agent, selected_index, stage) = match &app.mode
    {
        AppMode::Prompt {
            input, slash_state, ..
        } => (
            slash_state.available_agent_kinds.clone(),
            input.text().to_string(),
            slash_state.selected_agent,
            slash_state.selected_index,
            slash_state.stage,
        ),
        _ => return,
    };

    match stage {
        PromptSlashStage::Command => {
            let commands = prompt_slash_commands(&input_text);
            if commands.is_empty() {
                return;
            }

            let selected_command = commands.get(selected_index).copied().unwrap_or(commands[0]);

            match selected_command {
                "/stats" => {
                    if let AppMode::Prompt {
                        input, slash_state, ..
                    } = &mut app.mode
                    {
                        input.take_text();
                        slash_state.reset();
                    }
                    handle_stats_command(app, prompt_context).await;
                }
                _ => {
                    // /model — advance to Agent stage
                    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                        slash_state.stage = PromptSlashStage::Agent;
                        slash_state.selected_agent = None;
                        slash_state.selected_index = 0;
                    }
                }
            }
        }
        PromptSlashStage::Agent => {
            let Some(selected_agent) = available_agent_kinds.get(selected_index).copied() else {
                return;
            };

            if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                slash_state.selected_agent = Some(selected_agent);
                slash_state.stage = PromptSlashStage::Model;
                slash_state.selected_index = 0;
            }
        }
        PromptSlashStage::Model => {
            let fallback_agent = app
                .sessions
                .sessions
                .get(prompt_context.session_index)
                .and_then(|session| {
                    resolve_prompt_model_agent(session.model.kind(), &available_agent_kinds)
                });
            let Some(selected_agent) = selected_agent.or(fallback_agent) else {
                return;
            };
            let Some(selected_model) = selected_agent.models().get(selected_index).copied() else {
                return;
            };

            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.take_text();
                slash_state.reset();
            }

            // Best-effort: model switch failure is non-critical.
            let _ = app
                .set_session_model(&prompt_context.session_id, selected_model)
                .await;
        }
    }
}

/// Cancels the active prompt and drops any composer-owned attachment files.
async fn handle_prompt_cancel_key(app: &mut App, prompt_context: &PromptContext) {
    if prompt_context.is_slash_command {
        if let AppMode::Prompt {
            input, slash_state, ..
        } = &mut app.mode
        {
            input.take_text();
            slash_state.reset();
        }

        return;
    }

    cleanup_prompt_attachment_state(app).await;

    if prompt_context.can_delete_on_cancel {
        app.delete_selected_session_deferred_cleanup().await;
        app.mode = AppMode::List;

        return;
    }

    app.mode = AppMode::View {
        done_session_output_mode: DoneSessionOutputMode::Summary,
        review_status_message: None,
        review_text: None,
        session_id: prompt_context.session_id.clone(),
        scroll_offset: prompt_context.scroll_offset,
    };
}

async fn append_output_for_session(app: &App, session_id: &str, output: &str) {
    app.append_output_for_session(session_id, output).await;
}

/// Appends one prompt-mode status line to the session transcript shown above
/// the composer.
async fn append_prompt_status_line(app: &App, session_id: &str, label: &str, message: &str) {
    append_output_for_session(app, session_id, &format!("\n[{label}] {message}\n")).await;
}

/// Removes any prompt attachment files still owned by the active composer and
/// resets attachment state before leaving prompt mode.
async fn cleanup_prompt_attachment_state(app: &mut App) {
    let prompt = match &mut app.mode {
        AppMode::Prompt {
            attachment_state, ..
        } => {
            let attachments = attachment_state
                .attachments
                .iter()
                .map(|attachment| TurnPromptAttachment {
                    placeholder: attachment.placeholder.clone(),
                    local_image_path: attachment.local_image_path.clone(),
                })
                .collect::<Vec<_>>();
            attachment_state.reset();

            TurnPrompt {
                attachments,
                text: String::new(),
            }
        }
        _ => return,
    };

    app.cleanup_prompt_attachment_files(&prompt).await;
}

/// Handles `/stats` by loading stats through the app layer and appending the
/// rendered output to the session transcript.
async fn handle_stats_command(app: &App, prompt_context: &PromptContext) {
    let session_stats = app.stats_for_session(&prompt_context.session_id).await;
    let session_time = session_stats
        .session_duration_seconds
        .map_or_else(|| "Unavailable".to_string(), format_duration);
    let usage_rows_result = build_token_usage_rows(session_stats.usage_rows_result);
    let stats_output =
        build_stats_markdown(&prompt_context.session_id, &session_time, usage_rows_result);

    append_output_for_session(app, &prompt_context.session_id, &stats_output).await;
}

struct TokenUsageRow {
    in_tokens: String,
    model: String,
    out_tokens: String,
}

fn build_token_usage_rows(
    usage_rows_result: Result<Vec<SessionStatsUsage>, String>,
) -> Result<Vec<TokenUsageRow>, String> {
    match usage_rows_result {
        Ok(usage_rows) => {
            let rows = usage_rows
                .into_iter()
                .map(|row| TokenUsageRow {
                    in_tokens: format_token_count(row.input_tokens),
                    model: row.model,
                    out_tokens: format_token_count(row.output_tokens),
                })
                .collect();

            Ok(rows)
        }
        Err(error) => Err(error),
    }
}

fn build_stats_markdown(
    session_id: &str,
    session_time: &str,
    usage_rows_result: Result<Vec<TokenUsageRow>, String>,
) -> String {
    let mut lines = vec![
        format_stats_metric_line("Session ID", session_id),
        format_stats_metric_line("Session Time", session_time),
        String::new(),
        "Tokens Usage".to_string(),
    ];

    lines.extend(build_token_usage_lines(usage_rows_result));

    format!(
        "\n## Session Stats\n\n```stats\n{}\n```\n",
        lines.join("\n")
    )
}

fn format_stats_metric_line(metric: &str, value: &str) -> String {
    format!("{metric}\t{value}")
}

fn build_token_usage_lines(usage_rows_result: Result<Vec<TokenUsageRow>, String>) -> Vec<String> {
    match usage_rows_result {
        Ok(usage_rows) if usage_rows.is_empty() => vec!["No token usage recorded.".to_string()],
        Ok(usage_rows) => render_token_usage_table_lines(&usage_rows),
        Err(error) => vec![
            "Usage unavailable.".to_string(),
            format_stats_metric_line("Error", &error),
        ],
    }
}

fn render_token_usage_table_lines(usage_rows: &[TokenUsageRow]) -> Vec<String> {
    let model_width = usage_rows
        .iter()
        .map(|row| row.model.chars().count())
        .max()
        .unwrap_or_default()
        .max("Model".chars().count());
    let in_width = usage_rows
        .iter()
        .map(|row| row.in_tokens.chars().count())
        .max()
        .unwrap_or_default()
        .max("In".chars().count());
    let out_width = usage_rows
        .iter()
        .map(|row| row.out_tokens.chars().count())
        .max()
        .unwrap_or_default()
        .max("Out".chars().count());

    let mut lines = vec![format!(
        "{:<model_width$}  {:>in_width$}  {:>out_width$}",
        "Model", "In", "Out"
    )];

    lines.extend(usage_rows.iter().map(|row| {
        format!(
            "{:<model_width$}  {:>in_width$}  {:>out_width$}",
            row.model, row.in_tokens, row.out_tokens
        )
    }));

    lines
}

fn format_duration(total_seconds: i64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn prompt_slash_commands(input: &str) -> Vec<&'static str> {
    let lowered = input.to_lowercase();
    let mut commands = vec!["/model", "/stats"];
    commands.retain(|command| command.starts_with(&lowered));

    commands
}

fn prompt_slash_option_count(
    input: &str,
    stage: PromptSlashStage,
    selected_agent: Option<AgentKind>,
    available_agent_kinds: &[AgentKind],
) -> usize {
    match stage {
        PromptSlashStage::Command => prompt_slash_commands(input).len(),
        PromptSlashStage::Agent => available_agent_kinds.len(),
        PromptSlashStage::Model => selected_agent
            .or_else(|| available_agent_kinds.first().copied())
            .map_or(0, |selected_agent| selected_agent.models().len()),
    }
}

/// Resolves the agent used by `/model` model selection, preserving the
/// current session agent when it is still locally runnable.
fn resolve_prompt_model_agent(
    session_agent_kind: AgentKind,
    available_agent_kinds: &[AgentKind],
) -> Option<AgentKind> {
    agent::resolve_prompt_model_agent_kind(session_agent_kind, available_agent_kinds)
}

fn prompt_input_width<B: Backend>(terminal: &Terminal<B>) -> io::Result<u16>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let terminal_width = terminal.size().map_err(crate::runtime::backend_err)?.width;

    Ok(terminal_width.saturating_sub(2))
}

/// Moves the prompt cursor left with modifier-aware behavior.
///
/// `Cmd`+`Left` (`SUPER`) moves to the start of the current line,
/// `Option`+`Left` (`ALT`) and `Shift`+`Left` move to the previous word
/// start, and a plain `Left` moves one character. When the move lands inside
/// an existing `@path` token, the file dropdown is reopened.
fn handle_prompt_left(app: &mut App, key: KeyEvent) {
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        if key.modifiers.contains(event::KeyModifiers::SUPER) {
            input.move_line_start();
        } else if key
            .modifiers
            .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT)
        {
            input_key::move_cursor_word_left(input);
        } else {
            input.move_left();
        }
    }

    sync_prompt_at_mention_state(app);
}

/// Moves the prompt cursor right with modifier-aware behavior.
///
/// `Cmd`+`Right` (`SUPER`) moves to the end of the current line,
/// `Option`+`Right` (`ALT`) and `Shift`+`Right` move to the next word
/// start, and a plain `Right` moves one character. When the move lands inside
/// an existing `@path` token, the file dropdown is reopened.
fn handle_prompt_right(app: &mut App, key: KeyEvent) {
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        if key.modifiers.contains(event::KeyModifiers::SUPER) {
            input.move_line_end();
        } else if key
            .modifiers
            .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT)
        {
            input_key::move_cursor_word_right(input);
        } else {
            input.move_right();
        }
    }

    sync_prompt_at_mention_state(app);
}

/// Handles `Ctrl+u` line deletion by clearing the current line content.
///
/// This is the standard Unix "kill line" binding and also the sequence macOS
/// terminals send for `Cmd`+`Backspace`.
fn handle_prompt_line_delete(app: &mut App) {
    if let AppMode::Prompt { input, .. } = &app.mode
        && let Some((start, end)) = current_line_delete_range(input)
    {
        apply_prompt_delete_range(app, start, end);
    }
}

/// Handles `Ctrl+k` kill-to-end-of-line by deleting text from the cursor to
/// the end of the current line (stopping before the newline).
fn handle_prompt_kill_to_line_end(app: &mut App) {
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.delete_to_line_end();
    }
}

/// Handles `Ctrl+w` word deletion by deleting the previous word.
fn handle_prompt_word_delete(app: &mut App) {
    if let AppMode::Prompt { input, .. } = &app.mode
        && let Some((start, end)) = input_key::word_delete_range(input.text(), input.cursor)
    {
        apply_prompt_delete_range(app, start, end);
    }
}

/// Handles prompt backspace by deleting one character or one whole word when
/// `Option`/`Alt` (or `Shift` for compatibility) is pressed.
///
/// `Cmd`+`Backspace` takes precedence and clears the current line content.
fn handle_prompt_backspace(app: &mut App, key: KeyEvent) {
    let Some(delete_range) = prompt_backspace_range(app, key) else {
        return;
    };

    apply_prompt_delete_range(app, delete_range.0, delete_range.1);
}

/// Returns the character range deleted by one prompt backspace key press.
fn prompt_backspace_range(app: &App, key: KeyEvent) -> Option<(usize, usize)> {
    let AppMode::Prompt { input, .. } = &app.mode else {
        return None;
    };

    if input_key::is_line_delete_backspace(key) {
        return current_line_delete_range(input);
    }

    if input_key::is_word_delete_backspace(key) {
        return input_key::word_delete_range(input.text(), input.cursor);
    }

    if input.cursor == 0 {
        return None;
    }

    Some((input.cursor - 1, input.cursor))
}

fn handle_prompt_delete(app: &mut App) {
    let Some(delete_range) = prompt_delete_range(app) else {
        return;
    };

    apply_prompt_delete_range(app, delete_range.0, delete_range.1);
}

/// Returns the character range deleted by one prompt forward-delete key press.
fn prompt_delete_range(app: &App) -> Option<(usize, usize)> {
    let AppMode::Prompt { input, .. } = &app.mode else {
        return None;
    };

    let char_count = input.text().chars().count();
    if input.cursor >= char_count {
        return None;
    }

    Some((input.cursor, input.cursor + 1))
}

/// Applies one prompt deletion range, expanding it to cover full image
/// placeholder tokens and removing orphaned attachments from prompt state.
fn apply_prompt_delete_range(app: &mut App, start: usize, end: usize) {
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = &mut app.mode
    {
        let (delete_start, delete_end) =
            expand_delete_range_to_image_tokens(input.text(), start, end);
        if delete_start >= delete_end {
            return;
        }

        input.replace_range(delete_start, delete_end, "");
        attachment_state
            .attachments
            .retain(|attachment| input.text().contains(&attachment.placeholder));
        attachment_state.refresh_next_attachment_number();

        history_state.reset_navigation();
        slash_state.reset();
    }

    sync_prompt_at_mention_state(app);
}

/// Returns the character range deleted by one current-line delete action.
fn current_line_delete_range(input: &InputState) -> Option<(usize, usize)> {
    let characters: Vec<char> = input.text().chars().collect();
    if characters.is_empty() {
        return None;
    }

    let cursor = input.cursor.min(characters.len());
    let mut line_start = cursor;
    while line_start > 0 && characters[line_start - 1] != '\n' {
        line_start -= 1;
    }

    let mut line_end = cursor;
    while line_end < characters.len() && characters[line_end] != '\n' {
        line_end += 1;
    }

    let delete_range = if line_start > 0 {
        (line_start - 1, line_end)
    } else if line_end < characters.len() {
        (line_start, line_end + 1)
    } else {
        (line_start, line_end)
    };

    if delete_range.0 == delete_range.1 {
        None
    } else {
        Some(delete_range)
    }
}

/// Expands one deletion range to cover any overlapping `[Image #n]`
/// placeholders so partial token edits remove the whole placeholder.
fn expand_delete_range_to_image_tokens(text: &str, start: usize, end: usize) -> (usize, usize) {
    let mut expanded_start = start;
    let mut expanded_end = end;

    for (token_start, token_end, _) in image_token_ranges(text) {
        if token_start < expanded_end && expanded_start < token_end {
            expanded_start = expanded_start.min(token_start);
            expanded_end = expanded_end.max(token_end);
        }
    }

    (expanded_start, expanded_end)
}

/// Returns all valid `[Image #n]` placeholder token ranges in `text`.
fn image_token_ranges(text: &str) -> Vec<(usize, usize, String)> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut index = 0;

    while index < characters.len() {
        if let Some(end_index) = image_token_end_index(&characters, index) {
            let placeholder = characters[index..end_index].iter().collect::<String>();
            ranges.push((index, end_index, placeholder));
            index = end_index;

            continue;
        }

        index += 1;
    }

    ranges
}

/// Returns the exclusive end index for an `[Image #n]` placeholder token that
/// starts at `start_index`.
fn image_token_end_index(characters: &[char], start_index: usize) -> Option<usize> {
    let token_body = characters.get(start_index..)?;
    if token_body.len() < "[Image #1]".chars().count() || token_body.first() != Some(&'[') {
        return None;
    }

    let image_prefix = ['[', 'I', 'm', 'a', 'g', 'e', ' ', '#'];
    if token_body.get(..image_prefix.len())? != image_prefix {
        return None;
    }

    let mut scan_index = start_index + image_prefix.len();
    let mut saw_digit = false;
    while let Some(ch) = characters.get(scan_index) {
        if ch.is_ascii_digit() {
            saw_digit = true;
            scan_index += 1;

            continue;
        }

        if *ch == ']' && saw_digit {
            return Some(scan_index + 1);
        }

        return None;
    }

    None
}

/// Inserts one typed character into prompt input and keeps at-mention state
/// in sync.
fn handle_prompt_char(app: &mut App, character: char) {
    if let AppMode::Prompt {
        input,
        history_state,
        slash_state,
        ..
    } = &mut app.mode
    {
        input.insert_char(character);
        history_state.reset_navigation();
        slash_state.reset();
    }

    sync_prompt_at_mention_state(app);
}

/// Starts asynchronous loading of at-mention file entries for the prompt
/// session.
fn activate_at_mention(app: &mut App, prompt_context: &PromptContext) {
    let session_folder = app
        .sessions
        .sessions
        .get(prompt_context.session_index)
        .map_or_else(
            || app.working_dir().to_path_buf(),
            |session| session.folder.clone(),
        );
    let session_id = prompt_context.session_id.clone();
    let event_tx = app.services.event_sender();

    at_mention::start_loading_entries(event_tx, session_folder, session_id);

    if let AppMode::Prompt {
        at_mention_state, ..
    } = &mut app.mode
    {
        *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
    }
}

/// Clears the at-mention state.
fn dismiss_at_mention(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state, ..
    } = &mut app.mode
    {
        at_mention::dismiss(at_mention_state);
    }
}

/// Moves the at-mention selection up.
fn handle_at_mention_up(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_up(state);
    }
}

/// Moves the at-mention selection down.
fn handle_at_mention_down(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        input,
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_down(input, state);
    }
}

/// Selects the currently highlighted file and inserts it into the input.
fn handle_at_mention_select(app: &mut App) {
    let replacement = match &app.mode {
        AppMode::Prompt {
            at_mention_state: Some(state),
            input,
            ..
        } => at_mention::selected_replacement(input, state),
        _ => return,
    };

    if replacement.is_none() {
        dismiss_at_mention(app);

        return;
    }

    if let Some(selection) = replacement
        && let AppMode::Prompt { input, .. } = &mut app.mode
    {
        input.replace_range(selection.at_start, selection.cursor, &selection.text);
    }

    dismiss_at_mention(app);
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::infra::db::Database;
    use crate::infra::file_index::FileEntry;
    use crate::ui::state::prompt::{
        PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashState,
    };

    fn setup_test_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        std::fs::write(path.join("README.md"), "test").expect("write failed");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("git commit failed");
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(path)
            .output()
            .expect("git branch failed");
    }

    async fn new_test_prompt_app(
        input_text: &str,
        at_mention_state: Option<PromptAtMentionState>,
    ) -> (App, tempfile::TempDir) {
        new_test_prompt_app_with_session_mode(input_text, at_mention_state, false).await
    }

    /// Builds one prompt-mode test app backed by either an immediate-start or
    /// explicit draft session.
    async fn new_test_prompt_app_with_session_mode(
        input_text: &str,
        at_mention_state: Option<PromptAtMentionState>,
        is_draft_session: bool,
    ) -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        setup_test_git_repo(base_dir.path());
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            true,
            base_path.clone(),
            base_path,
            Some("main".to_string()),
            database,
        )
        .await
        .expect("failed to build app");

        let session_id = if is_draft_session {
            app.create_draft_session()
                .await
                .expect("failed to create draft session")
        } else {
            app.create_session()
                .await
                .expect("failed to create session")
        };
        app.mode = AppMode::Prompt {
            at_mention_state,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::new(Vec::new()),
            slash_state: PromptSlashState::new(),
            session_id,
            input: InputState::with_text(input_text.to_string()),
            scroll_offset: None,
        };

        (app, base_dir)
    }

    /// Builds one prompt-mode test app whose active session uses the explicit
    /// staged-draft workflow.
    async fn new_test_draft_prompt_app(
        input_text: &str,
        at_mention_state: Option<PromptAtMentionState>,
    ) -> (App, tempfile::TempDir) {
        new_test_prompt_app_with_session_mode(input_text, at_mention_state, true).await
    }

    #[test]
    fn test_is_plain_char_key_for_plain_character() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

        // Act
        let result = is_plain_char_key(key, 'j');

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_plain_char_key_rejects_modifier_keys() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('k'), event::KeyModifiers::SHIFT);

        // Act
        let result = is_plain_char_key(key, 'k');

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_plain_char_key_rejects_other_character() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

        // Act
        let result = is_plain_char_key(key, 'k');

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_prompt_image_paste_key_accepts_alt_v() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::ALT);

        // Act
        let result = is_prompt_image_paste_key(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_prompt_image_paste_key_accepts_ctrl_v() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::CONTROL);

        // Act
        let result = is_prompt_image_paste_key(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_prompt_image_paste_key_rejects_plain_v() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::NONE);

        // Act
        let result = is_prompt_image_paste_key(key);

        // Assert
        assert!(!result);
    }

    #[tokio::test]
    async fn test_handle_paste_inserts_multiline_content_with_normalized_newlines() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("prefix ", None).await;

        // Act
        handle_paste(&mut app, "line 1\r\nline 2\rline 3");

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.text(), "prefix line 1\nline 2\nline 3");
            assert_eq!(
                input.cursor,
                "prefix line 1\nline 2\nline 3".chars().count()
            );
        }
    }

    #[tokio::test]
    async fn test_insert_pasted_image_placeholder_records_attachment_and_resets_prompt_state() {
        // Arrange
        let mut at_mention_state = PromptAtMentionState::new(vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }]);
        at_mention_state.selected_index = 4;
        let (mut app, _base_dir) = new_test_prompt_app("Review ", Some(at_mention_state)).await;
        if let AppMode::Prompt {
            history_state,
            slash_state,
            ..
        } = &mut app.mode
        {
            history_state.selected_index = Some(0);
            history_state.draft_text = Some("draft".to_string());
            slash_state.selected_index = 2;
        }

        // Act
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));

        // Assert
        if let AppMode::Prompt {
            at_mention_state,
            attachment_state,
            history_state,
            input,
            slash_state,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "Review [Image #1]");
            assert_eq!(attachment_state.attachments.len(), 1);
            assert_eq!(
                attachment_state.attachments[0].local_image_path,
                std::path::PathBuf::from("/tmp/image-1.png")
            );
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
            assert_eq!(*slash_state, PromptSlashState::new());
            assert!(at_mention_state.is_none());
        }
    }

    #[tokio::test]
    async fn test_take_submitted_turn_prompt_drains_text_and_attachment_state() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));

        // Act
        let prompt = take_submitted_turn_prompt(&mut app);

        // Assert
        assert_eq!(prompt.text, "Review [Image #1]");
        assert_eq!(prompt.attachments.len(), 1);
        assert_eq!(prompt.attachments[0].placeholder, "[Image #1]");
        assert_eq!(
            prompt.attachments[0].local_image_path,
            std::path::PathBuf::from("/tmp/image-1.png")
        );
        if let AppMode::Prompt {
            attachment_state,
            input,
            ..
        } = &app.mode
        {
            assert!(input.text().is_empty());
            assert!(attachment_state.attachments.is_empty());
            assert_eq!(attachment_state.next_attachment_number, 1);
        }
    }

    #[tokio::test]
    async fn test_take_submitted_turn_prompt_filters_deleted_attachment_placeholders() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-2.png"));
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            *input = InputState::with_text("Review [Image #2]".to_string());
        }

        // Act
        let prompt = take_submitted_turn_prompt(&mut app);

        // Assert
        assert_eq!(prompt.text, "Review [Image #2]");
        assert_eq!(prompt.attachments.len(), 1);
        assert_eq!(prompt.attachments[0].placeholder, "[Image #2]");
        assert_eq!(
            prompt.attachments[0].local_image_path,
            std::path::PathBuf::from("/tmp/image-2.png")
        );
        if let AppMode::Prompt {
            attachment_state,
            input,
            ..
        } = &app.mode
        {
            assert!(input.text().is_empty());
            assert!(attachment_state.attachments.is_empty());
            assert_eq!(attachment_state.next_attachment_number, 1);
        }
    }

    #[tokio::test]
    async fn test_take_submitted_turn_prompt_sorts_attachments_by_text_position() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("", None).await;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-2.png"));
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            *input = InputState::with_text("[Image #2] then [Image #1]".to_string());
        }

        // Act
        let prompt = take_submitted_turn_prompt(&mut app);

        // Assert
        assert_eq!(prompt.attachments.len(), 2);
        assert_eq!(prompt.attachments[0].placeholder, "[Image #2]");
        assert_eq!(prompt.attachments[1].placeholder, "[Image #1]");
    }

    #[test]
    fn test_prompt_slash_commands_match_model() {
        // Arrange & Act
        let commands = prompt_slash_commands("/m");

        // Assert
        assert_eq!(commands, vec!["/model"]);
    }

    #[test]
    fn test_prompt_slash_commands_lists_all_commands() {
        // Arrange & Act
        let commands = prompt_slash_commands("/");

        // Assert
        assert_eq!(commands, vec!["/model", "/stats"]);
    }

    #[test]
    fn test_prompt_slash_commands_match_stats() {
        // Arrange & Act
        let commands = prompt_slash_commands("/s");

        // Assert
        assert_eq!(commands, vec!["/stats"]);
    }

    #[test]
    fn test_prompt_slash_commands_no_match() {
        // Arrange & Act
        let commands = prompt_slash_commands("/x");

        // Assert
        assert!(commands.is_empty());
    }

    #[test]
    fn test_prompt_slash_option_count_for_agent_stage() {
        // Arrange & Act
        let count =
            prompt_slash_option_count("/model", PromptSlashStage::Agent, None, AgentKind::ALL);

        // Assert
        assert_eq!(count, AgentKind::ALL.len());
    }

    #[test]
    fn test_prompt_slash_option_count_for_model_stage() {
        // Arrange & Act
        let count = prompt_slash_option_count(
            "/model",
            PromptSlashStage::Model,
            Some(AgentKind::Claude),
            AgentKind::ALL,
        );

        // Assert
        assert_eq!(count, AgentKind::Claude.models().len());
    }

    #[test]
    fn test_prompt_slash_option_count_for_agent_stage_uses_available_agent_kinds() {
        // Arrange
        let available_agent_kinds = [AgentKind::Codex];

        // Act
        let count = prompt_slash_option_count(
            "/model",
            PromptSlashStage::Agent,
            None,
            &available_agent_kinds,
        );

        // Assert
        assert_eq!(count, 1);
    }

    #[test]
    fn test_resolve_prompt_model_agent_prefers_current_session_agent_when_available() {
        // Arrange
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Codex];

        // Act
        let resolved_agent = resolve_prompt_model_agent(AgentKind::Codex, &available_agent_kinds);

        // Assert
        assert_eq!(resolved_agent, Some(AgentKind::Codex));
    }

    #[test]
    fn test_resolve_prompt_model_agent_falls_back_to_first_available_agent() {
        // Arrange
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Codex];

        // Act
        let resolved_agent = resolve_prompt_model_agent(AgentKind::Claude, &available_agent_kinds);

        // Assert
        assert_eq!(resolved_agent, Some(AgentKind::Gemini));
    }

    #[tokio::test]
    async fn test_navigate_prompt_history_up_stays_on_first_entry() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &mut app.mode
        {
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(0);
            *input = InputState::with_text("first".to_string());
        }

        // Act
        navigate_prompt_history_up(&mut app);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "first");
            assert_eq!(history_state.selected_index, Some(0));
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_navigate_prompt_history_up_selects_latest_entry_and_saves_draft() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.entries = vec!["first".to_string(), "second".to_string()];
        }

        // Act
        navigate_prompt_history_up(&mut app);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "second");
            assert_eq!(history_state.selected_index, Some(1));
            assert_eq!(history_state.draft_text.as_deref(), Some("draft"));
        }
    }

    #[tokio::test]
    async fn test_navigate_prompt_history_down_restores_draft_after_latest_entry() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.entries = vec!["first".to_string(), "second".to_string()];
        }
        navigate_prompt_history_up(&mut app);

        // Act
        navigate_prompt_history_down(&mut app);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "draft");
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_advance_prompt_slash_selection_stays_on_last_agent() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
        if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
            slash_state.stage = PromptSlashStage::Agent;
            slash_state.selected_index = AgentKind::ALL.len().saturating_sub(1);
        }

        // Act
        advance_prompt_slash_selection(&mut app);

        // Assert
        if let AppMode::Prompt { slash_state, .. } = &app.mode {
            assert_eq!(slash_state.stage, PromptSlashStage::Agent);
            assert_eq!(
                slash_state.selected_index,
                AgentKind::ALL.len().saturating_sub(1)
            );
        }
    }

    /// Verifies slash navigation leaves selection unchanged when the current
    /// command text matches no slash-command options.
    #[tokio::test]
    async fn test_advance_prompt_slash_selection_ignores_empty_command_matches() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/x", None).await;
        if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
            slash_state.selected_index = 2;
        }

        // Act
        advance_prompt_slash_selection(&mut app);

        // Assert
        if let AppMode::Prompt { slash_state, .. } = &app.mode {
            assert_eq!(slash_state.selected_index, 2);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_slash_submit_advances_model_command_to_agent_stage() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_slash_submit(&mut app, &prompt_context).await;

        // Assert
        if let AppMode::Prompt {
            input, slash_state, ..
        } = &app.mode
        {
            assert_eq!(input.text(), "/model");
            assert_eq!(slash_state.stage, PromptSlashStage::Agent);
            assert_eq!(slash_state.selected_agent, None);
            assert_eq!(slash_state.selected_index, 0);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_slash_submit_selects_agent_and_advances_to_model_stage() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
        let selected_index = AgentKind::ALL
            .iter()
            .position(|agent_kind| *agent_kind == AgentKind::Claude)
            .expect("expected Claude agent");
        if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
            slash_state.stage = PromptSlashStage::Agent;
            slash_state.selected_index = selected_index;
        }
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_slash_submit(&mut app, &prompt_context).await;

        // Assert
        if let AppMode::Prompt { slash_state, .. } = &app.mode {
            assert_eq!(slash_state.stage, PromptSlashStage::Model);
            assert_eq!(slash_state.selected_agent, Some(AgentKind::Claude));
            assert_eq!(slash_state.selected_index, 0);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_slash_submit_sets_selected_model_and_resets_input() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
        let expected_model = AgentKind::Claude.models()[0];
        if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
            slash_state.stage = PromptSlashStage::Model;
            slash_state.selected_agent = Some(AgentKind::Claude);
            slash_state.selected_index = 0;
        }
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_slash_submit(&mut app, &prompt_context).await;
        app.process_pending_app_events().await;

        // Assert
        if let AppMode::Prompt {
            input, slash_state, ..
        } = &app.mode
        {
            assert_eq!(input.text(), "");
            assert_eq!(*slash_state, PromptSlashState::new());
        }
        assert_eq!(app.sessions.sessions[0].model, expected_model);
    }

    #[tokio::test]
    async fn test_handle_prompt_slash_submit_runs_stats_command_and_resets_slash_state() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/stats", None).await;
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_slash_submit(&mut app, &prompt_context).await;

        // Assert
        app.sessions.sync_from_handles();
        if let AppMode::Prompt {
            input, slash_state, ..
        } = &app.mode
        {
            assert_eq!(input.text(), "");
            assert_eq!(*slash_state, PromptSlashState::new());
        }
        assert!(app.sessions.sessions[0].output.contains("## Session Stats"));
    }

    /// Verifies slash submit ignores unmatched commands and preserves the
    /// prompt state.
    #[tokio::test]
    async fn test_handle_prompt_slash_submit_ignores_unknown_command() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/x", None).await;
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_slash_submit(&mut app, &prompt_context).await;

        // Assert
        if let AppMode::Prompt {
            input, slash_state, ..
        } = &app.mode
        {
            assert_eq!(input.text(), "/x");
            assert_eq!(*slash_state, PromptSlashState::new());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_left_with_shift_moves_cursor_to_previous_word_start() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "hello brave world".chars().count();
        }

        // Act
        let key = KeyEvent::new(KeyCode::Left, event::KeyModifiers::SHIFT);
        handle_prompt_left(&mut app, key);

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello brave ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_left_with_shift_skips_whitespace_separators() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello\t \nworld", None).await;
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "hello\t \nworld".chars().count();
        }

        // Act
        let key = KeyEvent::new(KeyCode::Left, event::KeyModifiers::SHIFT);
        handle_prompt_left(&mut app, key);

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello\t \n".chars().count());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_right_with_shift_moves_cursor_to_next_word_start() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = 0;
        }

        // Act
        let key = KeyEvent::new(KeyCode::Right, event::KeyModifiers::SHIFT);
        handle_prompt_right(&mut app, key);

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_left_with_alt_moves_cursor_to_previous_word_start() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "hello brave world".chars().count();
        }

        // Act
        let key = KeyEvent::new(KeyCode::Left, event::KeyModifiers::ALT);
        handle_prompt_left(&mut app, key);

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello brave ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_right_with_alt_moves_cursor_to_next_word_start() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = 0;
        }

        // Act
        let key = KeyEvent::new(KeyCode::Right, event::KeyModifiers::ALT);
        handle_prompt_right(&mut app, key);

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_left_with_super_moves_cursor_to_line_start() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("first\nsecond\nthird", None).await;
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "first\nseco".chars().count();
        }

        // Act
        let key = KeyEvent::new(KeyCode::Left, event::KeyModifiers::SUPER);
        handle_prompt_left(&mut app, key);

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.cursor, "first\n".chars().count());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_right_with_super_moves_cursor_to_line_end() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("first\nsecond\nthird", None).await;
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "first\nse".chars().count();
        }

        // Act
        let key = KeyEvent::new(KeyCode::Right, event::KeyModifiers::SUPER);
        handle_prompt_right(&mut app, key);

        // Assert
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.cursor, "first\nsecond".chars().count());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_backspace_resets_history_navigation() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("second", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.draft_text = Some("draft".to_string());
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(1);
        }

        // Act
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::NONE);
        handle_prompt_backspace(&mut app, key);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "secon");
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_backspace_removes_whole_image_token_and_attachment() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &mut app.mode
        {
            history_state.selected_index = Some(0);
            history_state.draft_text = Some("draft".to_string());
            input.cursor = input.text().chars().count();
        }

        // Act
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::NONE);
        handle_prompt_backspace(&mut app, key);

        // Assert
        if let AppMode::Prompt {
            attachment_state,
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "Review ");
            assert!(attachment_state.attachments.is_empty());
            assert_eq!(attachment_state.next_attachment_number, 1);
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_backspace_with_shift_removes_whole_word() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.draft_text = Some("draft".to_string());
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(1);
        }

        // Act
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::SHIFT);
        handle_prompt_backspace(&mut app, key);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "hello brave");
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_delete_removes_whole_image_token_and_attachment() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &mut app.mode
        {
            history_state.selected_index = Some(0);
            history_state.draft_text = Some("draft".to_string());
            input.cursor = "Review ".chars().count();
        }

        // Act
        handle_prompt_delete(&mut app);

        // Assert
        if let AppMode::Prompt {
            attachment_state,
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "Review ");
            assert!(attachment_state.attachments.is_empty());
            assert_eq!(attachment_state.next_attachment_number, 1);
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_delete_reuses_deleted_image_number_on_next_paste() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("", None).await;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-2.png"));
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-3.png"));
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "[Image #1][Image #2]".chars().count();
        }

        // Act
        handle_prompt_delete(&mut app);
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-4.png"));

        // Assert
        if let AppMode::Prompt {
            attachment_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "[Image #1][Image #2][Image #3]");
            assert_eq!(attachment_state.attachments.len(), 3);
            assert_eq!(attachment_state.next_attachment_number, 4);
            assert_eq!(attachment_state.attachments[2].placeholder, "[Image #3]");
            assert_eq!(
                attachment_state.attachments[2].local_image_path,
                std::path::PathBuf::from("/tmp/image-4.png")
            );
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_backspace_with_alt_removes_whole_word() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.draft_text = Some("draft".to_string());
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(1);
        }

        // Act
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::ALT);
        handle_prompt_backspace(&mut app, key);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "hello brave");
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_backspace_with_super_deletes_full_line() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("first line\nsecond line", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.draft_text = Some("draft".to_string());
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(1);
        }
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "first line\nsecond".chars().count();
        }

        // Act
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::SUPER);
        handle_prompt_backspace(&mut app, key);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "first line");
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_line_delete_with_ctrl_u_deletes_full_line() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("first line\nsecond line", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.draft_text = Some("draft".to_string());
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(1);
        }
        if let AppMode::Prompt { input, .. } = &mut app.mode {
            input.cursor = "first line\nsecond".chars().count();
        }

        // Act
        handle_prompt_line_delete(&mut app);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "first line");
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_backspace_with_shift_removes_whitespace_separators() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("hello\t \nworld", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.draft_text = Some("draft".to_string());
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(1);
        }

        // Act
        let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::SHIFT);
        handle_prompt_backspace(&mut app, key);

        // Assert
        if let AppMode::Prompt {
            history_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.text(), "hello");
            assert_eq!(history_state.selected_index, None);
            assert_eq!(history_state.draft_text, None);
        }
    }

    #[test]
    fn test_is_active_at_mention_true_for_valid_query() {
        // Arrange
        let at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
        let input = InputState::with_text("@read".to_string());

        // Act
        let result = is_active_at_mention(at_mention_state.as_ref(), &input);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_active_at_mention_false_for_email_pattern() {
        // Arrange
        let at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
        let input = InputState::with_text("email@test".to_string());

        // Act
        let result = is_active_at_mention(at_mention_state.as_ref(), &input);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_active_at_mention_false_without_state() {
        // Arrange
        let at_mention_state = None;
        let input = InputState::with_text("@read".to_string());

        // Act
        let result = is_active_at_mention(at_mention_state.as_ref(), &input);

        // Assert
        assert!(!result);
    }

    #[tokio::test]
    async fn test_prompt_context_marks_email_pattern_as_inactive_mention() {
        // Arrange
        let state = PromptAtMentionState::new(Vec::new());
        let (mut app, _base_dir) = new_test_prompt_app("email@test", Some(state)).await;

        // Act
        let context = prompt_context(&mut app).expect("expected prompt context");

        // Assert
        assert!(!context.is_at_mention);
    }

    #[tokio::test]
    async fn test_prompt_context_falls_back_to_list_when_session_is_missing() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("follow up", None).await;
        app.mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::new(Vec::new()),
            input: InputState::with_text("follow up".to_string()),
            session_id: "missing-session".to_string(),
            slash_state: PromptSlashState::new(),
            scroll_offset: Some(2),
        };

        // Act
        let context = prompt_context(&mut app);

        // Assert
        assert!(context.is_none());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_at_mention_select_dismisses_stale_mention_state() {
        // Arrange
        let state = PromptAtMentionState::new(vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }]);
        let (mut app, _base_dir) = new_test_prompt_app("email@test", Some(state)).await;

        // Act
        handle_at_mention_select(&mut app);

        // Assert
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        if let AppMode::Prompt {
            at_mention_state,
            input,
            ..
        } = &app.mode
        {
            assert!(at_mention_state.is_none());
            assert_eq!(input.text(), "email@test");
        }
    }

    #[tokio::test]
    async fn test_handle_at_mention_select_inserts_directory_with_trailing_slash() {
        // Arrange
        let state = PromptAtMentionState::new(vec![FileEntry {
            is_dir: true,
            path: "src".to_string(),
        }]);
        let (mut app, _base_dir) = new_test_prompt_app("@src", Some(state)).await;

        // Act
        handle_at_mention_select(&mut app);

        // Assert
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        if let AppMode::Prompt { input, .. } = &app.mode {
            assert_eq!(input.text(), "@src/ ");
        }
    }

    /// Verifies stale at-mention selections are clamped to the filtered entry
    /// list before insertion.
    #[tokio::test]
    async fn test_handle_at_mention_select_clamps_stale_selected_index() {
        // Arrange
        let mut state = PromptAtMentionState::new(vec![
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "tests/main.rs".to_string(),
            },
        ]);
        state.selected_index = 9;
        let (mut app, _base_dir) = new_test_prompt_app("@src/ma", Some(state)).await;

        // Act
        handle_at_mention_select(&mut app);

        // Assert
        if let AppMode::Prompt {
            at_mention_state,
            input,
            ..
        } = &app.mode
        {
            assert!(at_mention_state.is_none());
            assert_eq!(input.text(), "@src/main.rs ");
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_char_activates_and_clears_at_mention_state() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("", None).await;

        // Act
        handle_prompt_char(&mut app, '@');

        // Assert
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        if let AppMode::Prompt {
            at_mention_state, ..
        } = &app.mode
        {
            assert!(at_mention_state.is_some());
        }

        // Act
        handle_prompt_char(&mut app, ' ');

        // Assert
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        if let AppMode::Prompt {
            at_mention_state, ..
        } = &app.mode
        {
            assert!(at_mention_state.is_none());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_left_reactivates_existing_at_mention_without_cached_state() {
        // Arrange
        let input_text = "@src/main.rs more";
        let (mut app, _base_dir) = new_test_prompt_app(input_text, None).await;
        let moves_back_into_mention = " more".chars().count();

        // Act
        for _ in 0..moves_back_into_mention {
            handle_prompt_left(
                &mut app,
                KeyEvent::new(KeyCode::Left, event::KeyModifiers::NONE),
            );
        }

        // Assert
        if let AppMode::Prompt {
            at_mention_state,
            input,
            ..
        } = &app.mode
        {
            assert_eq!(input.cursor, "@src/main.rs".chars().count());
            assert!(at_mention_state.is_some());
        }
    }

    #[tokio::test]
    async fn test_handle_prompt_cancel_key_deletes_blank_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("", None).await;
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");
        assert!(prompt_context.is_new_session);
        assert_eq!(app.sessions.sessions.len(), 1);

        // Act
        handle_prompt_cancel_key(&mut app, &prompt_context).await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(app.sessions.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_handle_prompt_cancel_key_keeps_empty_draft_session() {
        // Arrange
        let (mut app, _base_dir) = new_test_draft_prompt_app("", None).await;
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");
        assert!(prompt_context.is_new_session);
        assert!(!prompt_context.can_delete_on_cancel);
        assert_eq!(app.sessions.sessions.len(), 1);

        // Act
        handle_prompt_cancel_key(&mut app, &prompt_context).await;

        // Assert
        assert!(matches!(app.mode, AppMode::View { .. }));
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(
            app.sessions.sessions[0].status,
            crate::domain::session::Status::New
        );
        assert!(app.sessions.sessions[0].prompt.is_empty());
    }

    #[tokio::test]
    async fn test_handle_prompt_submit_key_ignores_empty_prompt() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("", None).await;
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_submit_key(&mut app, &prompt_context).await;

        // Assert
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(app.sessions.sessions[0].prompt, "");
    }

    #[tokio::test]
    async fn test_handle_prompt_submit_key_drains_supported_image_turn() {
        // Arrange
        let (mut app, _base_dir) = new_test_draft_prompt_app("Review ", None).await;
        app.sessions.sessions[0].model = crate::domain::agent::AgentModel::ClaudeSonnet46;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_submit_key(&mut app, &prompt_context).await;

        // Assert
        assert!(matches!(app.mode, AppMode::View { .. }));
        assert_eq!(app.sessions.sessions[0].prompt, "Review [Image #1]");
        assert_eq!(
            app.sessions.sessions[0].status,
            crate::domain::session::Status::New
        );
        assert_eq!(app.sessions.sessions[0].draft_attachments.len(), 1);
        assert_eq!(
            app.sessions.sessions[0].draft_attachments[0].placeholder,
            "[Image #1]"
        );
    }

    #[tokio::test]
    async fn test_handle_prompt_submit_key_starts_regular_session_with_image_turn() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
        app.sessions.sessions[0].model = crate::domain::agent::AgentModel::ClaudeSonnet46;
        insert_pasted_image_placeholder(&mut app, std::path::PathBuf::from("/tmp/image-1.png"));
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_submit_key(&mut app, &prompt_context).await;

        // Assert
        assert!(matches!(app.mode, AppMode::View { .. }));
        assert_eq!(app.sessions.sessions[0].prompt, "Review [Image #1]");
        assert_eq!(
            app.sessions.sessions[0].title.as_deref(),
            Some("Review [Image #1]")
        );
        assert!(app.sessions.sessions[0].draft_attachments.is_empty());
    }

    #[tokio::test]
    async fn test_handle_prompt_cancel_key_keeps_new_session_with_staged_drafts() {
        // Arrange
        let (mut app, _base_dir) = new_test_draft_prompt_app("Another draft", None).await;
        app.sessions.sessions[0].prompt = "First draft".to_string();
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_cancel_key(&mut app, &prompt_context).await;

        // Assert
        assert!(matches!(app.mode, AppMode::View { .. }));
        assert_eq!(app.sessions.sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_handle_prompt_cancel_key_resets_existing_session_draft_attachments() {
        // Arrange
        let (mut app, base_dir) = new_test_prompt_app("Review ", None).await;
        app.sessions.sessions[0].prompt = "Earlier prompt".to_string();
        app.sessions.sessions[0].status = crate::domain::session::Status::Review;
        let image_directory = base_dir.path().join("images");
        std::fs::create_dir_all(&image_directory).expect("image directory should exist");
        let image_path = image_directory.join("image-1.png");
        std::fs::write(&image_path, b"png").expect("image file should be written");
        insert_pasted_image_placeholder(&mut app, image_path.clone());
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_cancel_key(&mut app, &prompt_context).await;

        // Assert
        assert!(matches!(app.mode, AppMode::View { .. }));
        assert!(image_path.exists());
        assert!(image_directory.exists());
    }

    #[test]
    fn test_format_duration_zero() {
        // Arrange & Act
        let result = format_duration(0);

        // Assert
        assert_eq!(result, "00:00:00");
    }

    #[test]
    fn test_format_duration_mixed() {
        // Arrange & Act
        let result = format_duration(3661);

        // Assert
        assert_eq!(result, "01:01:01");
    }

    #[test]
    fn test_format_duration_large() {
        // Arrange & Act
        let result = format_duration(86400);

        // Assert
        assert_eq!(result, "24:00:00");
    }

    #[test]
    fn test_format_stats_metric_line_uses_tab_delimiter() {
        // Arrange & Act
        let session_line = format_stats_metric_line("Session ID", "session-id");
        let error_line = format_stats_metric_line("Error", "boom");

        // Assert
        assert_eq!(session_line, "Session ID\tsession-id");
        assert_eq!(error_line, "Error\tboom");
    }

    #[test]
    fn test_build_stats_markdown_renders_aligned_usage_table_without_box() {
        // Arrange
        let usage_rows_result = Ok(vec![TokenUsageRow {
            in_tokens: "1.2k".to_string(),
            model: "gemini-2.5-flash".to_string(),
            out_tokens: "650".to_string(),
        }]);

        // Act
        let result = build_stats_markdown("session-id", "00:20:15", usage_rows_result);

        // Assert
        assert!(result.starts_with("\n## Session Stats\n\n```stats\n"));
        assert!(result.contains("Session ID\tsession-id"));
        assert!(result.contains("Session Time\t00:20:15"));
        assert!(result.contains("Tokens Usage"));
        assert!(result.contains("Model"));
        assert!(result.contains("gemini-2.5-flash"));
        assert!(result.contains("1.2k"));
        assert!(result.contains("650"));
        assert!(!result.contains('+'));
        assert!(!result.contains('|'));

        let session_id_index = result.find("Session ID").expect("expected session id");
        let session_time_index = result.find("Session Time").expect("expected session time");
        let token_usage_index = result
            .find("Tokens Usage")
            .expect("expected token usage title");
        let model_header_index = result.find("Model").expect("expected model header");

        assert!(session_id_index < session_time_index);
        assert!(session_time_index < token_usage_index);
        assert!(token_usage_index < model_header_index);
    }

    #[test]
    fn test_build_stats_markdown_renders_no_usage_message() {
        // Arrange
        let usage_rows_result = Ok(Vec::new());

        // Act
        let result = build_stats_markdown("session-id", "00:20:15", usage_rows_result);

        // Assert
        assert!(result.contains("Tokens Usage"));
        assert!(result.contains("No token usage recorded."));
    }
}
