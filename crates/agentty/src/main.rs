use std::io;
use std::path::PathBuf;
use std::time::Duration;

use agentty::agent::AgentKind;
use agentty::app::{AGENTTY_WORKSPACE, App};
use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::model::{
    AppMode, InputState, PaletteCommand, PaletteFocus, PromptSlashStage, PromptSlashState,
};
use agentty::ui;
use agentty::ui::util::{move_input_cursor_down, move_input_cursor_up};
use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

/// Restores terminal state on all exit paths after raw mode is enabled.
///
/// The app uses `?` extensively inside the event loop and setup flow. Without
/// this guard, any early return after entering raw mode and the alternate
/// screen can leave the user's shell in a broken state.
///
/// Keeping cleanup in `Drop` guarantees restore runs during normal exit,
/// runtime errors, and unwinding panics.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = disable_raw_mode();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let base_path = PathBuf::from(AGENTTY_WORKSPACE);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_branch = agentty::git::detect_git_info(&working_dir);
    let lock_path = base_path.join("lock");
    let _lock = agentty::lock::acquire_lock(&lock_path)
        .map_err(|error| io::Error::other(format!("Error: {error}")))?;

    // setup terminal
    enable_raw_mode()?;
    let _terminal_guard = TerminalGuard;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let db_path = base_path.join(DB_DIR).join(DB_FILE);
    let db = Database::open(&db_path).await.map_err(io::Error::other)?;

    let mut app = App::new(base_path, working_dir, git_branch, db).await;
    let tick_rate = Duration::from_millis(50);

    // Spawn a dedicated thread for crossterm event reading so the main async
    // loop can yield to tokio between iterations.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(250)) {
                Ok(true) => {
                    if let Ok(evt) = crossterm::event::read() {
                        if event_tx.send(evt).is_err() {
                            break;
                        }
                    }
                }
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });

    let mut tick = tokio::time::interval(tick_rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    run_main_loop(&mut app, &mut terminal, &mut event_rx, &mut tick).await?;

    terminal.show_cursor()?;

    Ok(())
}

enum EventResult {
    Continue,
    Quit,
}

async fn run_main_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<()> {
    loop {
        render_frame(app, terminal)?;

        if matches!(
            process_events(app, terminal, event_rx, tick).await?,
            EventResult::Quit
        ) {
            break;
        }
    }

    Ok(())
}

fn render_frame(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<()> {
    let current_tab = app.current_tab;
    let current_working_dir = app.working_dir().clone();
    let current_git_branch = app.git_branch().map(std::string::ToString::to_string);
    let current_git_status = app.git_status_info();
    let health_checks = app.health_checks().clone();
    let current_active_project_id = app.active_project_id();

    terminal.draw(|f| {
        ui::render(
            f,
            ui::RenderContext {
                active_project_id: current_active_project_id,
                current_tab,
                git_branch: current_git_branch.as_deref(),
                git_status: current_git_status,
                health_checks: &health_checks,
                mode: &app.mode,
                projects: &app.projects,
                sessions: &app.session_state.sessions,
                table_state: &mut app.session_state.table_state,
                working_dir: &current_working_dir,
            },
        );
    })?;

    Ok(())
}

async fn process_events(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<EventResult> {
    enum LoopSignal {
        Event(Option<Event>),
        Tick,
    }

    // Wait for either a terminal event or the next tick (for redraws).
    // This yields to tokio so spawned tasks (agent output, git status) can
    // make progress on this worker thread.
    let signal = tokio::select! {
        biased;
        event = event_rx.recv() => LoopSignal::Event(event),
        _ = tick.tick() => LoopSignal::Tick,
    };
    let maybe_event = match signal {
        LoopSignal::Event(event) => event,
        LoopSignal::Tick => {
            app.refresh_sessions_if_needed().await;
            None
        }
    };

    if matches!(
        process_event(app, terminal, maybe_event).await?,
        EventResult::Quit
    ) {
        return Ok(EventResult::Quit);
    }

    // Drain remaining queued events before re-rendering so rapid key
    // presses are processed immediately instead of one-per-frame.
    while let Ok(event) = event_rx.try_recv() {
        if matches!(
            process_event(app, terminal, Some(event)).await?,
            EventResult::Quit
        ) {
            return Ok(EventResult::Quit);
        }
    }

    Ok(EventResult::Continue)
}

async fn process_event(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    event: Option<Event>,
) -> io::Result<EventResult> {
    if let Some(Event::Key(key)) = event {
        return handle_key_event(app, terminal, key).await;
    }

    Ok(EventResult::Continue)
}

async fn handle_key_event(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> io::Result<EventResult> {
    match &app.mode {
        AppMode::List => handle_list_key_event(app, key).await,
        AppMode::View { .. } => handle_view_key_event(app, terminal, key).await,
        AppMode::Prompt { .. } => handle_prompt_key_event(app, terminal, key).await,
        AppMode::Diff { .. } => Ok(handle_diff_key_event(app, key)),
        AppMode::CommandPalette { .. } => Ok(handle_command_palette_key_event(app, key)),
        AppMode::CommandOption { .. } => handle_command_option_key_event(app, key).await,
        AppMode::Health => Ok(handle_health_key_event(app, key)),
    }
}

async fn handle_list_key_event(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Char('q') => return Ok(EventResult::Quit),
        KeyCode::Tab => {
            app.next_tab();
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::CommandPalette {
                input: String::new(),
                selected_index: 0,
                focus: PaletteFocus::Dropdown,
            };
        }
        KeyCode::Char('a') => {
            if let Ok(session_id) = app.create_session().await {
                app.mode = AppMode::Prompt {
                    slash_state: PromptSlashState::new(),
                    session_id,
                    input: InputState::new(),
                    scroll_offset: None,
                };
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.next();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.previous();
        }
        KeyCode::Enter => {
            if let Some(session_index) = app.session_state.table_state.selected() {
                if let Some(session_id) = app.session_id_for_index(session_index) {
                    app.mode = AppMode::View {
                        session_id,
                        scroll_offset: None,
                    };
                }
            }
        }
        KeyCode::Char('d') => {
            app.delete_selected_session().await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

async fn handle_view_key_event(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> io::Result<EventResult> {
    let Some(view_context) = view_context(app) else {
        return Ok(EventResult::Continue);
    };

    let view_metrics = view_metrics(app, terminal, view_context.session_index)?;
    let mut next_scroll_offset = view_context.scroll_offset;

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('r') => {
            app.mode = AppMode::Prompt {
                slash_state: PromptSlashState::new(),
                session_id: view_context.session_id.clone(),
                input: InputState::new(),
                scroll_offset: next_scroll_offset,
            };
        }
        KeyCode::Char('j') | KeyCode::Down => {
            next_scroll_offset = scroll_offset_down(next_scroll_offset, view_metrics, 1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            next_scroll_offset = Some(scroll_offset_up(next_scroll_offset, view_metrics, 1));
        }
        KeyCode::Char('g') => {
            next_scroll_offset = Some(0);
        }
        KeyCode::Char('G') => {
            next_scroll_offset = None;
        }
        KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            next_scroll_offset = scroll_offset_down(
                next_scroll_offset,
                view_metrics,
                view_metrics.view_height / 2,
            );
        }
        KeyCode::Char('u') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            next_scroll_offset = Some(scroll_offset_up(
                next_scroll_offset,
                view_metrics,
                view_metrics.view_height / 2,
            ));
        }
        KeyCode::Char('d') if !key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            show_diff_for_view_session(app, &view_context).await;
        }
        KeyCode::Char('c') => {
            commit_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('m') => {
            merge_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('p') => {
            create_pr_for_view_session(app, &view_context.session_id).await;
        }
        _ => {}
    }

    if let AppMode::View { scroll_offset, .. } = &mut app.mode {
        *scroll_offset = next_scroll_offset;
    }

    Ok(EventResult::Continue)
}

fn view_context(app: &mut App) -> Option<ViewContext> {
    let (session_id, scroll_offset) = match &app.mode {
        AppMode::View {
            session_id,
            scroll_offset,
        } => (session_id.clone(), *scroll_offset),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    Some(ViewContext {
        scroll_offset,
        session_id,
        session_index,
    })
}

fn view_metrics(
    app: &App,
    terminal: &Terminal<CrosstermBackend<io::Stdout>>,
    session_index: usize,
) -> io::Result<ViewMetrics> {
    let terminal_height = terminal.size()?.height;
    let view_height = terminal_height.saturating_sub(5);
    let total_lines = u16::try_from(
        app.session_state
            .sessions
            .get(session_index)
            .and_then(|session| session.output.lock().ok())
            .map(|output| output.lines().count())
            .unwrap_or(0),
    )
    .unwrap_or(0);

    Ok(ViewMetrics {
        total_lines,
        view_height,
    })
}

fn scroll_offset_down(scroll_offset: Option<u16>, metrics: ViewMetrics, step: u16) -> Option<u16> {
    let current_offset = scroll_offset?;

    let next_offset = current_offset.saturating_add(step.max(1));
    if next_offset >= metrics.total_lines.saturating_sub(metrics.view_height) {
        return None;
    }

    Some(next_offset)
}

fn scroll_offset_up(scroll_offset: Option<u16>, metrics: ViewMetrics, step: u16) -> u16 {
    let current_offset =
        scroll_offset.unwrap_or_else(|| metrics.total_lines.saturating_sub(metrics.view_height));

    current_offset.saturating_sub(step.max(1))
}

async fn show_diff_for_view_session(app: &mut App, view_context: &ViewContext) {
    let Some(session) = app.session_state.sessions.get(view_context.session_index) else {
        return;
    };

    let session_folder = session.folder.clone();
    let diff = tokio::task::spawn_blocking(move || agentty::git::diff(&session_folder))
        .await
        .unwrap_or_else(|join_error| Err(join_error.to_string()))
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"));
    app.mode = AppMode::Diff {
        session_id: view_context.session_id.clone(),
        diff,
        scroll_offset: 0,
    };
}

async fn commit_view_session(app: &mut App, session_id: &str) {
    let result_message = match app.commit_session(session_id).await {
        Ok(message) => format!("\n[Commit] {message}\n"),
        Err(error) => format!("\n[Commit Error] {error}\n"),
    };

    append_output_for_session(app, session_id, &result_message);
}

async fn merge_view_session(app: &mut App, session_id: &str) {
    let result_message = match app.merge_session(session_id).await {
        Ok(message) => format!("\n[Merge] {message}\n"),
        Err(error) => format!("\n[Merge Error] {error}\n"),
    };

    append_output_for_session(app, session_id, &result_message);
}

async fn create_pr_for_view_session(app: &mut App, session_id: &str) {
    if let Err(error) = app.create_pr_session(session_id).await {
        append_output_for_session(app, session_id, &format!("\n[PR Error] {error}\n"));
    }
}

fn append_output_for_session(app: &App, session_id: &str, output: &str) {
    let Some(session_index) = app.session_index_for_id(session_id) else {
        return;
    };
    let Some(session) = app.session_state.sessions.get(session_index) else {
        return;
    };

    session.append_output(output);
}

async fn handle_prompt_key_event(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> io::Result<EventResult> {
    let Some(prompt_context) = prompt_context(app) else {
        return Ok(EventResult::Continue);
    };

    if !prompt_context.is_slash_command {
        reset_prompt_slash_state(app);
    }

    match key.code {
        KeyCode::Enter if should_insert_newline(key) => {
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
            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.delete_backward();
                slash_state.reset();
            }
        }
        KeyCode::Delete => {
            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.delete_forward();
                slash_state.reset();
            }
        }
        KeyCode::Char(character) => {
            if let AppMode::Prompt {
                input, slash_state, ..
            } = &mut app.mode
            {
                input.insert_char(character);
                slash_state.reset();
            }
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

fn prompt_context(app: &mut App) -> Option<PromptContext> {
    let (is_slash_command, scroll_offset, session_id) = match &app.mode {
        AppMode::Prompt {
            input,
            scroll_offset,
            session_id,
            ..
        } => (
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
        .session_state
        .sessions
        .get(session_index)
        .map(|session| session.prompt.is_empty())
        .unwrap_or(false);

    Some(PromptContext {
        is_new_session,
        is_slash_command,
        scroll_offset,
        session_id,
        session_index,
    })
}

fn reset_prompt_slash_state(app: &mut App) {
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.reset();
    }
}

fn is_prompt_cancel_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc || key.modifiers.contains(event::KeyModifiers::CONTROL)
}

fn handle_prompt_up_key(
    app: &mut App,
    terminal: &Terminal<CrosstermBackend<io::Stdout>>,
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
        input.cursor = move_input_cursor_up(input.text(), input_width, input.cursor);
    }

    Ok(())
}

fn handle_prompt_down_key(
    app: &mut App,
    terminal: &Terminal<CrosstermBackend<io::Stdout>>,
    prompt_context: &PromptContext,
) -> io::Result<()> {
    if prompt_context.is_slash_command {
        advance_prompt_slash_selection(app);

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = move_input_cursor_down(input.text(), input_width, input.cursor);
    }

    Ok(())
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
        handle_prompt_slash_submit(app, prompt_context);

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
            );
        }
    } else {
        app.reply(&prompt_context.session_id, &prompt);
    }

    app.mode = AppMode::View {
        session_id: prompt_context.session_id.clone(),
        scroll_offset: None,
    };
}

fn handle_prompt_slash_submit(app: &mut App, prompt_context: &PromptContext) {
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

            if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                slash_state.stage = PromptSlashStage::Agent;
                slash_state.selected_agent = None;
                slash_state.selected_index = 0;
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
                .session_state
                .sessions
                .get(prompt_context.session_index)
                .and_then(|session| session.agent.parse::<AgentKind>().ok())
                .unwrap_or(AgentKind::Gemini);
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

            let _ = app.set_session_agent_and_model(
                &prompt_context.session_id,
                selected_agent,
                selected_model,
            );
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

fn handle_diff_key_event(app: &mut App, key: KeyEvent) -> EventResult {
    if let AppMode::Diff {
        session_id,
        diff: _,
        scroll_offset,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                app.mode = AppMode::View {
                    session_id: session_id.clone(),
                    scroll_offset: None,
                };
            }
            KeyCode::Char('j') | KeyCode::Down => {
                *scroll_offset = scroll_offset.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *scroll_offset = scroll_offset.saturating_sub(1);
            }
            _ => {}
        }
    }

    EventResult::Continue
}

fn handle_command_palette_key_event(app: &mut App, key: KeyEvent) -> EventResult {
    let mut palette_action = PaletteAction::None;
    if let AppMode::CommandPalette {
        input,
        selected_index,
        focus,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                app.mode = AppMode::List;
            }
            KeyCode::Char(character) => {
                input.push(character);
                update_palette_focus(input, selected_index, focus);
            }
            KeyCode::Backspace => {
                input.pop();
                update_palette_focus(input, selected_index, focus);
            }
            KeyCode::Up if *focus == PaletteFocus::Dropdown => {
                *selected_index = selected_index.saturating_sub(1);
            }
            KeyCode::Down if *focus == PaletteFocus::Dropdown => {
                move_palette_selection_down(input, selected_index, focus);
            }
            KeyCode::Enter if *focus == PaletteFocus::Dropdown => {
                let filtered = PaletteCommand::filter(input);
                if let Some(&command) = filtered.get(*selected_index) {
                    palette_action = PaletteAction::Open(command);
                }
            }
            KeyCode::Esc => {
                if *focus == PaletteFocus::Dropdown {
                    *focus = PaletteFocus::Input;
                } else {
                    app.mode = AppMode::List;
                }
            }
            _ => {}
        }
    }

    match palette_action {
        PaletteAction::None => {}
        PaletteAction::Open(PaletteCommand::Projects) => {
            app.mode = AppMode::CommandOption {
                command: PaletteCommand::Projects,
                selected_index: 0,
            };
        }
        PaletteAction::Open(PaletteCommand::Health) => {
            app.start_health_checks();
            app.mode = AppMode::Health;
        }
    }

    EventResult::Continue
}

fn update_palette_focus(input: &str, selected_index: &mut usize, focus: &mut PaletteFocus) {
    let filtered = PaletteCommand::filter(input);
    *selected_index = 0;
    *focus = if filtered.is_empty() {
        PaletteFocus::Input
    } else {
        PaletteFocus::Dropdown
    };
}

fn move_palette_selection_down(input: &str, selected_index: &mut usize, focus: &mut PaletteFocus) {
    let filtered = PaletteCommand::filter(input);
    if filtered.is_empty() {
        *focus = PaletteFocus::Input;

        return;
    }
    if *selected_index >= filtered.len().saturating_sub(1) {
        *focus = PaletteFocus::Input;
    } else {
        *selected_index += 1;
    }
}

async fn handle_command_option_key_event(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    let (command, mut selected_index) = match &app.mode {
        AppMode::CommandOption {
            command,
            selected_index,
        } => (*command, *selected_index),
        _ => return Ok(EventResult::Continue),
    };

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let option_count = command_option_count(app, command);
            if option_count > 0 {
                selected_index = (selected_index + 1).min(option_count - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            selected_index = selected_index.saturating_sub(1);
        }
        KeyCode::Enter => {
            handle_command_option_enter(app, command, selected_index).await;
            app.mode = AppMode::List;
        }
        KeyCode::Esc => {
            app.mode = AppMode::CommandPalette {
                input: String::new(),
                selected_index: 0,
                focus: PaletteFocus::Dropdown,
            };
        }
        _ => {}
    }

    if let AppMode::CommandOption {
        selected_index: mode_selected_index,
        ..
    } = &mut app.mode
    {
        *mode_selected_index = selected_index;
    }

    Ok(EventResult::Continue)
}

fn command_option_count(app: &App, command: PaletteCommand) -> usize {
    match command {
        PaletteCommand::Health => 0,
        PaletteCommand::Projects => app.projects.len(),
    }
}

async fn handle_command_option_enter(
    app: &mut App,
    command: PaletteCommand,
    selected_index: usize,
) {
    if command != PaletteCommand::Projects {
        return;
    }
    let Some(project) = app.projects.get(selected_index) else {
        return;
    };

    let _ = app.switch_project(project.id).await;
}

fn handle_health_key_event(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('r') => {
            app.start_health_checks();
        }
        _ => {}
    }

    EventResult::Continue
}

#[derive(Clone)]
struct ViewContext {
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

#[derive(Clone, Copy)]
struct ViewMetrics {
    total_lines: u16,
    view_height: u16,
}

struct PromptContext {
    is_new_session: bool,
    is_slash_command: bool,
    scroll_offset: Option<u16>,
    session_id: String,
    session_index: usize,
}

enum PaletteAction {
    None,
    Open(PaletteCommand),
}

fn prompt_slash_commands(input: &str) -> Vec<&'static str> {
    let lowered = input.to_lowercase();
    let mut commands = vec!["/model"];
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

fn prompt_input_width(terminal: &Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<u16> {
    let terminal_width = terminal.size()?.width;

    Ok(terminal_width.saturating_sub(2))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_prompt_slash_commands_match_model() {
        // Arrange & Act
        let commands = prompt_slash_commands("/m");

        // Assert
        assert_eq!(commands, vec!["/model"]);
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
}
