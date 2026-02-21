use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::domain::agent::AgentKind;
use crate::app::App;
use crate::file_list;
use crate::domain::input::InputState;
use crate::ui::state::app_mode::AppMode;
use crate::ui::state::prompt::{PromptAtMentionState, PromptSlashStage};
use crate::runtime::{EventResult, TuiTerminal};
use crate::ui::util::{move_input_cursor_down, move_input_cursor_up};

struct PromptContext {
    is_at_mention: bool,
    is_new_session: bool,
    is_slash_command: bool,
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

pub(crate) async fn handle(
    app: &mut App,
    terminal: &mut TuiTerminal,
    key: KeyEvent,
) -> io::Result<EventResult> {
    let Some(prompt_context) = prompt_context(app) else {
        return Ok(EventResult::Continue);
    };

    if !prompt_context.is_slash_command {
        reset_prompt_slash_state(app);
    }

    match key.code {
        KeyCode::Esc if prompt_context.is_at_mention => {
            dismiss_at_mention(app);
        }
        KeyCode::Enter if prompt_context.is_at_mention && !should_insert_newline(key) => {
            handle_at_mention_select(app);
        }
        KeyCode::Tab if prompt_context.is_at_mention => {
            handle_at_mention_select(app);
        }
        KeyCode::Up if prompt_context.is_at_mention => {
            handle_at_mention_up(app);
        }
        KeyCode::Down if prompt_context.is_at_mention => {
            handle_at_mention_down(app);
        }
        KeyCode::Enter if should_insert_newline(key) => {
            reset_prompt_history_navigation(app);

            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.insert_newline();
            }
        }
        KeyCode::Enter => {
            handle_prompt_submit_key(app, &prompt_context).await;
        }
        KeyCode::Esc | KeyCode::Char('c') if is_prompt_cancel_key(key) => {
            handle_prompt_cancel_key(app, &prompt_context).await;
        }
        KeyCode::Left => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_left();
            }
        }
        KeyCode::Right => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_right();
            }
        }
        KeyCode::Up => {
            handle_prompt_up_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Down => {
            handle_prompt_down_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Char('k') if prompt_context.is_slash_command && is_plain_char_key(key, 'k') => {
            handle_prompt_up_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Char('j') if prompt_context.is_slash_command && is_plain_char_key(key, 'j') => {
            handle_prompt_down_key(app, terminal, &prompt_context)?;
        }
        KeyCode::Home => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_home();
            }
        }
        KeyCode::End => {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                input.move_end();
            }
        }
        KeyCode::Backspace => {
            handle_prompt_backspace(app);
        }
        KeyCode::Delete => {
            handle_prompt_delete(app);
        }
        KeyCode::BackTab => {
            let _ = app
                .toggle_session_permission_mode(&prompt_context.session_id)
                .await;
        }
        KeyCode::Char(character) => {
            handle_prompt_char(app, character, &prompt_context).await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
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

    let is_new_session = app
        .sessions
        .sessions
        .get(session_index)
        .is_some_and(|session| session.prompt.is_empty());

    Some(PromptContext {
        is_at_mention,
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

fn handle_prompt_up_key(
    app: &mut App,
    terminal: &TuiTerminal,
    prompt_context: &PromptContext,
) -> io::Result<()> {
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

            return Ok(());
        }
    }

    navigate_prompt_history_up(app);

    Ok(())
}

fn handle_prompt_down_key(
    app: &mut App,
    terminal: &TuiTerminal,
    prompt_context: &PromptContext,
) -> io::Result<()> {
    if prompt_context.is_slash_command {
        advance_prompt_slash_selection(app);

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        let next_cursor = move_input_cursor_down(input.text(), input_width, input.cursor);
        if next_cursor != input.cursor {
            input.cursor = next_cursor;

            return Ok(());
        }
    }

    navigate_prompt_history_down(app);

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
    let (input_text, selected_agent, selected_index, stage) = match &app.mode {
        AppMode::Prompt {
            input, slash_state, ..
        } => (
            input.text().to_string(),
            slash_state.selected_agent,
            slash_state.selected_index,
            slash_state.stage,
        ),
        _ => return,
    };

    let option_count = prompt_slash_option_count(&input_text, stage, selected_agent);
    if option_count == 0 {
        return;
    }

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        let max_index = option_count.saturating_sub(1);
        slash_state.selected_index = (selected_index + 1).min(max_index);
    }
}

async fn handle_prompt_submit_key(app: &mut App, prompt_context: &PromptContext) {
    if prompt_context.is_slash_command {
        handle_prompt_slash_submit(app, prompt_context).await;

        return;
    }

    let prompt = match &mut app.mode {
        AppMode::Prompt { input, .. } => input.take_text(),
        _ => String::new(),
    };
    if prompt.is_empty() {
        return;
    }

    if prompt_context.is_new_session {
        if let Err(error) = app.start_session(&prompt_context.session_id, prompt).await {
            append_output_for_session(
                app,
                &prompt_context.session_id,
                &format!("\n[Error] {error}\n"),
            )
            .await;
        }
    } else {
        app.reply(&prompt_context.session_id, &prompt).await;
    }

    app.mode = AppMode::View {
        session_id: prompt_context.session_id.clone(),
        scroll_offset: None,
    };
}

async fn handle_prompt_slash_submit(app: &mut App, prompt_context: &PromptContext) {
    let (input_text, selected_agent, selected_index, stage) = match &app.mode {
        AppMode::Prompt {
            input, slash_state, ..
        } => (
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
                "/clear" => {
                    if let AppMode::Prompt {
                        input, slash_state, ..
                    } = &mut app.mode
                    {
                        input.take_text();
                        slash_state.reset();
                    }
                    let _ = app.clear_session_history(&prompt_context.session_id).await;
                }
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
            let Some(selected_agent) = AgentKind::ALL.get(selected_index).copied() else {
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
                .map_or(AgentKind::Gemini, |session| session.model.kind());
            let selected_agent = selected_agent.unwrap_or(fallback_agent);
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

            let _ = app
                .set_session_model(&prompt_context.session_id, selected_model)
                .await;
        }
    }
}

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

    if prompt_context.is_new_session {
        app.delete_selected_session().await;
        app.mode = AppMode::List;

        return;
    }

    app.mode = AppMode::View {
        session_id: prompt_context.session_id.clone(),
        scroll_offset: prompt_context.scroll_offset,
    };
}

async fn append_output_for_session(app: &App, session_id: &str, output: &str) {
    app.append_output_for_session(session_id, output).await;
}

async fn handle_stats_command(app: &App, prompt_context: &PromptContext) {
    let stats_output = build_stats_output(app.services.db(), &prompt_context.session_id).await;
    append_output_for_session(app, &prompt_context.session_id, &stats_output).await;
}

struct TokenUsageRow {
    in_tokens: String,
    model: String,
    out_tokens: String,
}

async fn build_stats_output(db: &crate::db::Database, session_id: &str) -> String {
    let session_time = load_session_time_text(db, session_id).await;
    let usage_rows_result = build_token_usage_rows(db.load_session_usage(session_id).await);

    build_stats_markdown(session_id, &session_time, usage_rows_result)
}

async fn load_session_time_text(db: &crate::db::Database, session_id: &str) -> String {
    match db.load_session_timestamps(session_id).await {
        Ok(Some((created_at, updated_at))) => {
            let duration_seconds = (updated_at - created_at).max(0);

            format_duration(duration_seconds)
        }
        Ok(None) | Err(_) => "Unavailable".to_string(),
    }
}

fn build_token_usage_rows(
    usage_rows_result: Result<Vec<crate::db::SessionUsageRow>, String>,
) -> Result<Vec<TokenUsageRow>, String> {
    match usage_rows_result {
        Ok(usage_rows) => {
            let rows = usage_rows
                .into_iter()
                .map(|row| TokenUsageRow {
                    in_tokens: crate::ui::util::format_token_count(row.input_tokens.unsigned_abs()),
                    model: row.model,
                    out_tokens: crate::ui::util::format_token_count(
                        row.output_tokens.unsigned_abs(),
                    ),
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
    let mut commands = vec!["/clear", "/model", "/stats"];
    commands.retain(|command| command.starts_with(&lowered));

    commands
}

fn prompt_slash_option_count(
    input: &str,
    stage: PromptSlashStage,
    selected_agent: Option<AgentKind>,
) -> usize {
    match stage {
        PromptSlashStage::Command => prompt_slash_commands(input).len(),
        PromptSlashStage::Agent => AgentKind::ALL.len(),
        PromptSlashStage::Model => selected_agent.unwrap_or(AgentKind::Gemini).models().len(),
    }
}

fn should_insert_newline(key: KeyEvent) -> bool {
    is_enter_key(key.code) && key.modifiers.contains(event::KeyModifiers::ALT)
}

fn is_enter_key(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Enter | KeyCode::Char('\r' | '\n'))
}

fn prompt_input_width(terminal: &TuiTerminal) -> io::Result<u16> {
    let terminal_width = terminal.size()?.width;

    Ok(terminal_width.saturating_sub(2))
}

fn handle_prompt_backspace(app: &mut App) {
    if let AppMode::Prompt {
        input,
        history_state,
        slash_state,
        at_mention_state,
        ..
    } = &mut app.mode
    {
        input.delete_backward();
        history_state.reset_navigation();
        slash_state.reset();
        if at_mention_state.is_some() && input.at_mention_query().is_none() {
            *at_mention_state = None;
        }
    }
}

fn handle_prompt_delete(app: &mut App) {
    if let AppMode::Prompt {
        input,
        history_state,
        slash_state,
        at_mention_state,
        ..
    } = &mut app.mode
    {
        input.delete_forward();
        history_state.reset_navigation();
        slash_state.reset();
        if at_mention_state.is_some() && input.at_mention_query().is_none() {
            *at_mention_state = None;
        }
    }
}

async fn handle_prompt_char(app: &mut App, character: char, prompt_context: &PromptContext) {
    let mut should_activate = false;

    if let AppMode::Prompt {
        input,
        history_state,
        slash_state,
        at_mention_state,
        ..
    } = &mut app.mode
    {
        input.insert_char(character);
        history_state.reset_navigation();
        slash_state.reset();

        if character == ' ' || input.at_mention_query().is_none() {
            *at_mention_state = None;
        } else if character == '@' && at_mention_state.is_none() {
            should_activate = true;
        } else if let Some(state) = at_mention_state.as_mut() {
            state.selected_index = 0;
        }
    }

    if should_activate && !prompt_context.is_slash_command {
        activate_at_mention(app, prompt_context).await;
    }
}

/// Populates the at-mention file list from the session's worktree folder.
async fn activate_at_mention(app: &mut App, prompt_context: &PromptContext) {
    let session_folder = app
        .sessions
        .sessions
        .get(prompt_context.session_index)
        .map_or_else(
            || app.working_dir().to_path_buf(),
            |session| session.folder.clone(),
        );

    let entries = tokio::task::spawn_blocking(move || file_list::list_files(&session_folder))
        .await
        .unwrap_or_default();

    if let AppMode::Prompt {
        at_mention_state, ..
    } = &mut app.mode
    {
        *at_mention_state = Some(PromptAtMentionState::new(entries));
    }
}

/// Clears the at-mention state.
fn dismiss_at_mention(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state, ..
    } = &mut app.mode
    {
        *at_mention_state = None;
    }
}

/// Moves the at-mention selection up.
fn handle_at_mention_up(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        state.selected_index = state.selected_index.saturating_sub(1);
    }
}

/// Moves the at-mention selection down.
fn handle_at_mention_down(app: &mut App) {
    let filtered_count = match &app.mode {
        AppMode::Prompt {
            at_mention_state: Some(state),
            input,
            ..
        } => {
            let query = input
                .at_mention_query()
                .map_or(String::new(), |(_, query)| query);

            file_list::filter_entries(&state.all_entries, &query).len()
        }
        _ => return,
    };

    if let AppMode::Prompt {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        let max_index = filtered_count.saturating_sub(1);
        state.selected_index = (state.selected_index + 1).min(max_index);
    }
}

/// Selects the currently highlighted file and inserts it into the input.
fn handle_at_mention_select(app: &mut App) {
    let mut should_dismiss = false;
    let replacement = match &app.mode {
        AppMode::Prompt {
            at_mention_state: Some(state),
            input,
            ..
        } => {
            if let Some((at_start, query)) = input.at_mention_query() {
                let filtered = file_list::filter_entries(&state.all_entries, &query);
                let clamped_index = state.selected_index.min(filtered.len().saturating_sub(1));

                filtered.get(clamped_index).map(|entry| {
                    let path = if entry.is_dir {
                        format!("@{}/ ", entry.path)
                    } else {
                        format!("@{} ", entry.path)
                    };

                    (at_start, input.cursor, path)
                })
            } else {
                should_dismiss = true;

                None
            }
        }
        _ => return,
    };

    if should_dismiss {
        dismiss_at_mention(app);

        return;
    }

    if let Some((at_start, cursor, text)) = replacement
        && let AppMode::Prompt { input, .. } = &mut app.mode
    {
        input.replace_range(at_start, cursor, &text);
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
    use crate::ui::state::prompt::{PromptAtMentionState, PromptHistoryState};

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
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        setup_test_git_repo(base_dir.path());
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(
            base_path.clone(),
            base_path,
            Some("main".to_string()),
            database,
        )
        .await;

        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Prompt {
            at_mention_state,
            history_state: PromptHistoryState::new(Vec::new()),
            slash_state: crate::ui::state::prompt::PromptSlashState::new(),
            session_id,
            input: InputState::with_text(input_text.to_string()),
            scroll_offset: None,
        };

        (app, base_dir)
    }
    #[test]
    fn test_should_insert_newline_for_alt_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::ALT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_alt_shift_enter() {
        // Arrange
        let key = KeyEvent::new(
            KeyCode::Enter,
            event::KeyModifiers::ALT | event::KeyModifiers::SHIFT,
        );

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_alt_carriage_return() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\r'), event::KeyModifiers::ALT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_insert_newline_for_alt_line_feed() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\n'), event::KeyModifiers::ALT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_should_not_insert_newline_for_plain_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::NONE);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_shift_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_shift_carriage_return() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\r'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_shift_line_feed() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('\n'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_control_enter() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::CONTROL);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_should_not_insert_newline_for_non_enter_key() {
        // Arrange
        let key = KeyEvent::new(KeyCode::Char('x'), event::KeyModifiers::SHIFT);

        // Act
        let result = should_insert_newline(key);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_is_enter_key_for_enter() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Enter);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_enter_key_for_carriage_return() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Char('\r'));

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_enter_key_for_line_feed() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Char('\n'));

        // Assert
        assert!(result);
    }

    #[test]
    fn test_is_enter_key_for_other_key() {
        // Arrange & Act
        let result = is_enter_key(KeyCode::Char('x'));

        // Assert
        assert!(!result);
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
    fn test_prompt_slash_commands_match_model() {
        // Arrange & Act
        let commands = prompt_slash_commands("/m");

        // Assert
        assert_eq!(commands, vec!["/model"]);
    }

    #[test]
    fn test_prompt_slash_commands_match_clear() {
        // Arrange & Act
        let commands = prompt_slash_commands("/c");

        // Assert
        assert_eq!(commands, vec!["/clear"]);
    }

    #[test]
    fn test_prompt_slash_commands_lists_all_commands() {
        // Arrange & Act
        let commands = prompt_slash_commands("/");

        // Assert
        assert_eq!(commands, vec!["/clear", "/model", "/stats"]);
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
        let count = prompt_slash_option_count("/model", PromptSlashStage::Agent, None);

        // Assert
        assert_eq!(count, AgentKind::ALL.len());
    }

    #[test]
    fn test_prompt_slash_option_count_for_model_stage() {
        // Arrange & Act
        let count =
            prompt_slash_option_count("/model", PromptSlashStage::Model, Some(AgentKind::Claude));

        // Assert
        assert_eq!(count, AgentKind::Claude.models().len());
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
    async fn test_handle_prompt_backspace_resets_history_navigation() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("second", None).await;
        if let AppMode::Prompt { history_state, .. } = &mut app.mode {
            history_state.draft_text = Some("draft".to_string());
            history_state.entries = vec!["first".to_string(), "second".to_string()];
            history_state.selected_index = Some(1);
        }

        // Act
        handle_prompt_backspace(&mut app);

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
    async fn test_handle_at_mention_select_dismisses_stale_mention_state() {
        // Arrange
        let state = PromptAtMentionState::new(vec![file_list::FileEntry {
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
        let state = PromptAtMentionState::new(vec![file_list::FileEntry {
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

    #[tokio::test]
    async fn test_handle_prompt_char_activates_and_clears_at_mention_state() {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("", None).await;
        let prompt_context = prompt_context(&mut app).expect("expected prompt context");

        // Act
        handle_prompt_char(&mut app, '@', &prompt_context).await;
        handle_prompt_char(&mut app, ' ', &prompt_context).await;

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
