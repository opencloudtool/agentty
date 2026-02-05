use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use ag_cli::agent::AgentKind;
use ag_cli::app::{AGENTTY_WORKSPACE, App};
use ag_cli::model::AppMode;
use ag_cli::ui;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

fn main() -> io::Result<()> {
    let base_path = PathBuf::from(AGENTTY_WORKSPACE);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_branch = ag_cli::git::detect_git_info(&working_dir);
    let lock_path = base_path.join("lock");
    let _lock = match ag_cli::lock::acquire_lock(&lock_path) {
        Ok(file) => file,
        Err(e) => {
            #[allow(clippy::print_stderr)]
            {
                let _ = writeln!(io::stderr(), "Error: {e}");
            }
            #[allow(clippy::exit)]
            std::process::exit(1);
        }
    };

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let agent_kind = AgentKind::from_env();
    let backend = agent_kind.create_backend();
    let mut app = App::new(base_path, working_dir, git_branch, agent_kind, backend);
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(100);

    loop {
        let current_agent_kind = app.agent_kind();
        let current_tab = app.current_tab;
        let current_working_dir = app.working_dir().clone();
        let current_git_branch = app.git_branch().map(std::string::ToString::to_string);
        terminal.draw(|f| {
            ui::render(
                f,
                &app.mode,
                &app.sessions,
                &mut app.table_state,
                current_agent_kind,
                current_tab,
                &current_working_dir,
                current_git_branch.as_deref(),
            );
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match &mut app.mode {
                    AppMode::List => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Tab => {
                            app.next_tab();
                        }
                        KeyCode::Char('a') => {
                            app.mode = AppMode::Prompt {
                                input: String::new(),
                            };
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            app.next();
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            app.previous();
                        }
                        KeyCode::Enter => {
                            if let Some(i) = app.table_state.selected() {
                                if i < app.sessions.len() {
                                    app.mode = AppMode::View {
                                        session_index: i,
                                        scroll_offset: None,
                                    };
                                }
                            }
                        }
                        KeyCode::Char('d') => {
                            app.delete_selected_session();
                        }
                        KeyCode::Char('o') => {
                            if let Some(session) = app.selected_session() {
                                let folder = session.folder.clone();
                                disable_raw_mode()?;
                                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                                let shell =
                                    std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
                                let _ = Command::new(&shell).current_dir(&folder).status();
                                enable_raw_mode()?;
                                execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                                terminal.clear()?;
                            }
                        }
                        _ => {}
                    },
                    AppMode::View {
                        session_index,
                        scroll_offset,
                    } => {
                        let session_idx = *session_index;
                        let mut new_scroll = *scroll_offset;

                        // Estimate view height (terminal height - margins/borders/footer)
                        // Margin: 1 top/bottom (2) + Footer: 1 + Block borders: 2 = 5 overhead
                        let term_height = terminal.size()?.height;
                        let view_height = term_height.saturating_sub(5);
                        let total_lines = u16::try_from(
                            app.sessions
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
                                app.mode = AppMode::Reply {
                                    session_index: session_idx,
                                    input: String::new(),
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
                                let current = new_scroll
                                    .unwrap_or_else(|| total_lines.saturating_sub(view_height));
                                new_scroll = Some(current.saturating_sub(1));
                            }
                            KeyCode::Char('g') => {
                                new_scroll = Some(0);
                            }
                            KeyCode::Char('G') => {
                                new_scroll = None;
                            }
                            KeyCode::Char('d')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                if let Some(current) = new_scroll {
                                    let next = current.saturating_add(view_height / 2);
                                    if next >= total_lines.saturating_sub(view_height) {
                                        new_scroll = None;
                                    } else {
                                        new_scroll = Some(next);
                                    }
                                }
                            }
                            KeyCode::Char('u')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let current = new_scroll
                                    .unwrap_or_else(|| total_lines.saturating_sub(view_height));
                                new_scroll = Some(current.saturating_sub(view_height / 2));
                            }
                            _ => {}
                        }

                        // Update state if changed (and not switching mode)
                        if let AppMode::View { scroll_offset, .. } = &mut app.mode {
                            *scroll_offset = new_scroll;
                        }
                    }
                    AppMode::Reply {
                        session_index,
                        input,
                        scroll_offset,
                    } => {
                        let session_index = *session_index;
                        let scroll_snapshot = *scroll_offset;
                        match key.code {
                            KeyCode::Enter => {
                                let prompt = input.clone();
                                app.mode = AppMode::View {
                                    session_index,
                                    scroll_offset: None, // Reset scroll on new message
                                };
                                if !prompt.is_empty() {
                                    app.reply(session_index, &prompt);
                                }
                            }
                            KeyCode::Esc => {
                                app.mode = AppMode::View {
                                    session_index,
                                    scroll_offset: scroll_snapshot,
                                };
                            }
                            KeyCode::Char(c) => {
                                input.push(c);
                            }
                            KeyCode::Backspace => {
                                input.pop();
                            }
                            _ => {}
                        }
                    }
                    AppMode::Prompt { input } => match key.code {
                        KeyCode::Enter => {
                            let prompt = input.clone();
                            app.mode = AppMode::List;
                            if !prompt.is_empty() {
                                if let Err(e) = app.add_session(prompt) {
                                    #[allow(clippy::print_stderr)]
                                    {
                                        eprintln!("Error creating session: {e}");
                                    }
                                    // TODO: Add proper error display in TUI
                                }
                            }
                        }
                        KeyCode::Esc => {
                            app.mode = AppMode::List;
                        }
                        KeyCode::Char(c) => {
                            input.push(c);
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        _ => {}
                    },
                }
            }
        }

        if last_tick.elapsed() >= Duration::from_secs(1) {
            last_tick = Instant::now();
        }
    }

    // restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen,)?;
    terminal.show_cursor()?;

    Ok(())
}
