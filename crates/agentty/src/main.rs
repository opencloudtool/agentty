use std::io;
use std::path::PathBuf;
use std::process::Command;
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
    match &mut app.mode {
        AppMode::List => match key.code {
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
                if let Some(i) = app.session_state.table_state.selected() {
                    if let Some(session_id) = app.session_id_for_index(i) {
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
            KeyCode::Char('o') => {
                if let Some(session) = app.selected_session() {
                    let folder = session.folder.clone();
                    disable_raw_mode()?;
                    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                    let _ = Command::new(&shell).current_dir(&folder).status();
                    enable_raw_mode()?;
                    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                    terminal.clear()?;
                }
            }
            _ => {}
        },
        AppMode::View {
            session_id,
            scroll_offset,
        } => {
            let session_id = session_id.clone();
            let Some(session_idx) = app
                .session_state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
            else {
                app.mode = AppMode::List;

                return Ok(EventResult::Continue);
            };
            let mut new_scroll = *scroll_offset;

            // Estimate view height (terminal height - margins/borders/footer)
            // Margin: 1 top/bottom (2) + Footer: 1 + Block borders: 2 = 5 overhead
            let term_height = terminal.size()?.height;
            let view_height = term_height.saturating_sub(5);
            let total_lines = u16::try_from(
                app.session_state
                    .sessions
                    .get(session_idx)
                    .and_then(|a| a.output.lock().ok())
                    .map(|o| o.lines().count())
                    .unwrap_or(0),
            )
            .unwrap_or(0);

            match key.code {
                KeyCode::Char('q') => {
                    app.mode = AppMode::List;
                }
                KeyCode::Char('r') => {
                    app.mode = AppMode::Prompt {
                        slash_state: PromptSlashState::new(),
                        session_id: session_id.clone(),
                        input: InputState::new(),
                        scroll_offset: new_scroll,
                    };
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if let Some(current) = new_scroll {
                        let next = current.saturating_add(1);
                        if next >= total_lines.saturating_sub(view_height) {
                            new_scroll = None;
                        } else {
                            new_scroll = Some(next);
                        }
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let current =
                        new_scroll.unwrap_or_else(|| total_lines.saturating_sub(view_height));
                    new_scroll = Some(current.saturating_sub(1));
                }
                KeyCode::Char('g') => {
                    new_scroll = Some(0);
                }
                KeyCode::Char('G') => {
                    new_scroll = None;
                }
                KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    if let Some(current) = new_scroll {
                        let next = current.saturating_add(view_height / 2);
                        if next >= total_lines.saturating_sub(view_height) {
                            new_scroll = None;
                        } else {
                            new_scroll = Some(next);
                        }
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    let current =
                        new_scroll.unwrap_or_else(|| total_lines.saturating_sub(view_height));
                    new_scroll = Some(current.saturating_sub(view_height / 2));
                }
                KeyCode::Char('d') if !key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    if let Some(session) = app.session_state.sessions.get(session_idx) {
                        let folder = session.folder.clone();
                        let diff = tokio::task::spawn_blocking(move || agentty::git::diff(&folder))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                            .unwrap_or_else(|e| format!("Failed to run git diff: {e}"));
                        app.mode = AppMode::Diff {
                            session_id: session_id.clone(),
                            diff,
                            scroll_offset: 0,
                        };
                    }
                }
                KeyCode::Char('c') => {
                    if let Some(session) = app.session_state.sessions.get(session_idx) {
                        let result_message = match app.commit_session(&session_id).await {
                            Ok(msg) => format!("\n[Commit] {msg}\n"),
                            Err(err) => format!("\n[Commit Error] {err}\n"),
                        };
                        session.append_output(&result_message);
                    }
                }
                KeyCode::Char('m') => {
                    if let Some(session) = app.session_state.sessions.get(session_idx) {
                        let result_message = match app.merge_session(&session_id).await {
                            Ok(msg) => format!("\n[Merge] {msg}\n"),
                            Err(err) => format!("\n[Merge Error] {err}\n"),
                        };
                        session.append_output(&result_message);
                    }
                }
                KeyCode::Char('p') => {
                    if let Err(e) = app.create_pr_session(&session_id).await {
                        if let Some(session) = app.session_state.sessions.get(session_idx) {
                            session.append_output(&format!("\n[PR Error] {e}\n"));
                        }
                    }
                }
                _ => {}
            }

            // Update state if changed (and not switching mode)
            if let AppMode::View { scroll_offset, .. } = &mut app.mode {
                *scroll_offset = new_scroll;
            }
        }
        AppMode::Prompt {
            session_id,
            input,
            scroll_offset,
            slash_state,
        } => {
            let session_id = session_id.clone();
            let Some(session_idx) = app
                .session_state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
            else {
                app.mode = AppMode::List;

                return Ok(EventResult::Continue);
            };
            let scroll_snapshot = *scroll_offset;
            let is_new_session = app
                .session_state
                .sessions
                .get(session_idx)
                .map(|s| s.prompt.is_empty())
                .unwrap_or(false);
            let is_slash_command = input.text().starts_with('/');
            if !is_slash_command {
                slash_state.reset();
            }

            match key.code {
                KeyCode::Enter if should_insert_newline(key) => {
                    input.insert_newline();
                }
                KeyCode::Enter => {
                    if is_slash_command {
                        match slash_state.stage {
                            PromptSlashStage::Command => {
                                let commands = prompt_slash_commands(input.text());
                                if commands.is_empty() {
                                    return Ok(EventResult::Continue);
                                }

                                slash_state.stage = PromptSlashStage::Agent;
                                slash_state.selected_agent = None;
                                slash_state.selected_index = 0;

                                return Ok(EventResult::Continue);
                            }
                            PromptSlashStage::Agent => {
                                let selected_agent =
                                    AgentKind::ALL.get(slash_state.selected_index).copied();
                                let Some(selected_agent) = selected_agent else {
                                    return Ok(EventResult::Continue);
                                };

                                slash_state.selected_agent = Some(selected_agent);
                                slash_state.stage = PromptSlashStage::Model;
                                slash_state.selected_index = 0;

                                return Ok(EventResult::Continue);
                            }
                            PromptSlashStage::Model => {
                                let fallback_agent = app
                                    .session_state
                                    .sessions
                                    .get(session_idx)
                                    .and_then(|session| session.agent.parse::<AgentKind>().ok())
                                    .unwrap_or(AgentKind::Gemini);
                                let selected_agent =
                                    slash_state.selected_agent.unwrap_or(fallback_agent);
                                let selected_model = selected_agent
                                    .models()
                                    .get(slash_state.selected_index)
                                    .copied();
                                let Some(selected_model) = selected_model else {
                                    return Ok(EventResult::Continue);
                                };

                                input.take_text();
                                slash_state.reset();
                                let _ = app.set_session_agent_and_model(
                                    &session_id,
                                    selected_agent,
                                    selected_model,
                                );

                                return Ok(EventResult::Continue);
                            }
                        }
                    }

                    let prompt = input.take_text();
                    if !prompt.is_empty() {
                        if is_new_session {
                            if let Err(error) = app.start_session(&session_id, prompt).await {
                                if let Some(session) = app.session_state.sessions.get(session_idx) {
                                    session.append_output(&format!("\n[Error] {error}\n"));
                                }
                            }
                        } else {
                            app.reply(&session_id, &prompt);
                        }
                        app.mode = AppMode::View {
                            session_id: session_id.clone(),
                            scroll_offset: None,
                        };
                    }
                }
                KeyCode::Esc | KeyCode::Char('c')
                    if key.code == KeyCode::Esc
                        || key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                {
                    if is_slash_command {
                        input.take_text();
                        slash_state.reset();

                        return Ok(EventResult::Continue);
                    }

                    if is_new_session {
                        app.delete_selected_session().await;
                        app.mode = AppMode::List;
                    } else {
                        app.mode = AppMode::View {
                            session_id: session_id.clone(),
                            scroll_offset: scroll_snapshot,
                        };
                    }
                }
                KeyCode::Left => {
                    input.move_left();
                }
                KeyCode::Right => {
                    input.move_right();
                }
                KeyCode::Up => {
                    if is_slash_command {
                        slash_state.selected_index = slash_state.selected_index.saturating_sub(1);

                        return Ok(EventResult::Continue);
                    }

                    let input_width = prompt_input_width(terminal)?;
                    let next_cursor = move_input_cursor_up(input.text(), input_width, input.cursor);
                    input.cursor = next_cursor;
                }
                KeyCode::Down => {
                    if is_slash_command {
                        let option_count = prompt_slash_option_count(
                            input.text(),
                            slash_state.stage,
                            slash_state.selected_agent,
                        );
                        if option_count > 0 {
                            let max_index = option_count.saturating_sub(1);
                            slash_state.selected_index =
                                (slash_state.selected_index + 1).min(max_index);
                        }

                        return Ok(EventResult::Continue);
                    }

                    let input_width = prompt_input_width(terminal)?;
                    let next_cursor =
                        move_input_cursor_down(input.text(), input_width, input.cursor);
                    input.cursor = next_cursor;
                }
                KeyCode::Home => {
                    input.move_home();
                }
                KeyCode::End => {
                    input.move_end();
                }
                KeyCode::Backspace => {
                    input.delete_backward();
                    slash_state.reset();
                }
                KeyCode::Delete => {
                    input.delete_forward();
                    slash_state.reset();
                }
                KeyCode::Char(c) => {
                    input.insert_char(c);
                    slash_state.reset();
                }
                _ => {}
            }
        }
        AppMode::Diff {
            session_id,
            diff: _,
            scroll_offset,
        } => match key.code {
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
        },
        AppMode::CommandPalette {
            input,
            selected_index,
            focus,
        } => match focus {
            PaletteFocus::Input | PaletteFocus::Dropdown => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    app.mode = AppMode::List;
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    let filtered = PaletteCommand::filter(input);
                    if filtered.is_empty() {
                        *focus = PaletteFocus::Input;
                    } else {
                        *selected_index = 0;
                        *focus = PaletteFocus::Dropdown;
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    let filtered = PaletteCommand::filter(input);
                    if input.is_empty() || filtered.is_empty() {
                        *selected_index = 0;
                        *focus = PaletteFocus::Input;
                    } else {
                        *selected_index = 0;
                        *focus = PaletteFocus::Dropdown;
                    }
                }
                KeyCode::Up if *focus == PaletteFocus::Dropdown => {
                    *selected_index = selected_index.saturating_sub(1);
                }
                KeyCode::Down if *focus == PaletteFocus::Dropdown => {
                    let filtered = PaletteCommand::filter(input);
                    if !filtered.is_empty() && *selected_index >= filtered.len() - 1 {
                        *focus = PaletteFocus::Input;
                    } else {
                        *selected_index += 1;
                    }
                }
                KeyCode::Enter if *focus == PaletteFocus::Dropdown => {
                    let filtered = PaletteCommand::filter(input);
                    if let Some(&command) = filtered.get(*selected_index) {
                        match command {
                            PaletteCommand::Projects => {
                                app.mode = AppMode::CommandOption {
                                    command,
                                    selected_index: 0,
                                };
                            }
                            PaletteCommand::Health => {
                                app.start_health_checks();
                                app.mode = AppMode::Health;
                            }
                        }
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
            },
        },
        AppMode::CommandOption {
            command,
            selected_index,
        } => match key.code {
            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                app.mode = AppMode::List;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let option_count = match command {
                    PaletteCommand::Health => 0,
                    PaletteCommand::Projects => app.projects.len(),
                };
                if option_count > 0 {
                    *selected_index = (*selected_index + 1).min(option_count - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *selected_index = selected_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                match command {
                    PaletteCommand::Health => {}
                    PaletteCommand::Projects => {
                        if let Some(project) = app.projects.get(*selected_index) {
                            let project_id = project.id;
                            let _ = app.switch_project(project_id).await;
                        }
                    }
                }
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
        },
        AppMode::Health => match key.code {
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
        },
    }
    Ok(EventResult::Continue)
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
