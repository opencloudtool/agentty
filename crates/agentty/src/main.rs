use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use agentty::agent::AgentKind;
use agentty::app::{AGENTTY_WORKSPACE, App, SESSION_DATA_DIR};
use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::model::{AppMode, PaletteCommand, PaletteFocus};
use agentty::ui;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

#[tokio::main]
async fn main() -> io::Result<()> {
    let base_path = PathBuf::from(AGENTTY_WORKSPACE);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_branch = agentty::git::detect_git_info(&working_dir);
    let lock_path = base_path.join("lock");
    let _lock = match agentty::lock::acquire_lock(&lock_path) {
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

    let db_path = base_path.join(DB_DIR).join(DB_FILE);
    let db = Database::open(&db_path).await.map_err(io::Error::other)?;

    let agent_kind = AgentKind::from_env();
    let backend = agent_kind.create_backend();
    let mut app = App::new(base_path, working_dir, git_branch, agent_kind, backend, db).await;
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(100);

    loop {
        let current_agent_kind = match &app.mode {
            AppMode::CommandOption {
                command: PaletteCommand::Agents,
                selected_index,
            } => AgentKind::ALL
                .get(*selected_index)
                .copied()
                .unwrap_or_else(|| app.agent_kind()),
            _ => app.agent_kind(),
        };
        let current_tab = app.current_tab;
        let current_working_dir = app.working_dir().clone();
        let current_git_branch = app.git_branch().map(std::string::ToString::to_string);
        let current_git_status = app.git_status_info();
        let health_checks = app.health_checks().clone();

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
                current_git_status,
                &health_checks,
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
                        KeyCode::Char('/') => {
                            app.mode = AppMode::CommandPalette {
                                input: String::new(),
                                selected_index: 0,
                                focus: PaletteFocus::Dropdown,
                            };
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
                            app.delete_selected_session().await;
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
                            KeyCode::Char('d')
                                if !key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                if let Some(session) = app.sessions.get(session_idx) {
                                    let output = Command::new("git")
                                        .arg("diff")
                                        .current_dir(&session.folder)
                                        .output()
                                        .ok();
                                    let diff = output
                                        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                                        .unwrap_or_else(|| "Failed to run git diff".to_string());
                                    app.mode = AppMode::Diff {
                                        session_index: session_idx,
                                        diff,
                                        scroll_offset: 0,
                                    };
                                }
                            }
                            KeyCode::Char('c') => {
                                if let Some(session) = app.sessions.get(session_idx) {
                                    let result_message = match app.commit_session(session_idx).await
                                    {
                                        Ok(msg) => format!("\n[Commit] {msg}\n"),
                                        Err(err) => format!("\n[Commit Error] {err}\n"),
                                    };
                                    if let Ok(mut buf) = session.output.lock() {
                                        buf.push_str(&result_message);
                                    }
                                    let _ = std::fs::OpenOptions::new()
                                        .append(true)
                                        .open(
                                            session
                                                .folder
                                                .join(SESSION_DATA_DIR)
                                                .join("output.txt"),
                                        )
                                        .and_then(|mut f| {
                                            use std::io::Write;
                                            write!(f, "{result_message}")
                                        });
                                }
                            }
                            KeyCode::Char('m') => {
                                if let Some(session) = app.sessions.get(session_idx) {
                                    let result_message = match app.merge_session(session_idx).await
                                    {
                                        Ok(msg) => format!("\n[Merge] {msg}\n"),
                                        Err(err) => format!("\n[Merge Error] {err}\n"),
                                    };
                                    if let Ok(mut buf) = session.output.lock() {
                                        buf.push_str(&result_message);
                                    }
                                    let _ = std::fs::OpenOptions::new()
                                        .append(true)
                                        .open(
                                            session
                                                .folder
                                                .join(SESSION_DATA_DIR)
                                                .join("output.txt"),
                                        )
                                        .and_then(|mut f| {
                                            use std::io::Write;
                                            write!(f, "{result_message}")
                                        });
                                }
                            }
                            KeyCode::Char('p') => {
                                if let Err(e) = app.create_pr_session(session_idx).await {
                                    // Log immediate errors (e.g. "Already processing") to output
                                    if let Some(session) = app.sessions.get(session_idx) {
                                        let err_msg = format!("\n[PR Error] {e}\n");
                                        if let Ok(mut buf) = session.output.lock() {
                                            buf.push_str(&err_msg);
                                        }
                                        let _ = std::fs::OpenOptions::new()
                                            .append(true)
                                            .open(
                                                session
                                                    .folder
                                                    .join(SESSION_DATA_DIR)
                                                    .join("output.txt"),
                                            )
                                            .and_then(|mut f| {
                                                use std::io::Write;
                                                write!(f, "{err_msg}")
                                            });
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
                            KeyCode::Char('c')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
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
                                if let Err(e) = app.add_session(prompt).await {
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
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
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
                    AppMode::Diff {
                        session_index,
                        diff: _,
                        scroll_offset,
                    } => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            app.mode = AppMode::View {
                                session_index: *session_index,
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
                            KeyCode::Char('c')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
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
                                        PaletteCommand::Health => {
                                            app.start_health_checks();
                                            app.mode = AppMode::Health;
                                        }
                                        PaletteCommand::Agents => {
                                            app.mode = AppMode::CommandOption {
                                                command,
                                                selected_index: 0,
                                            };
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
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            app.mode = AppMode::List;
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            let option_count = match command {
                                PaletteCommand::Agents => AgentKind::ALL.len(),
                                PaletteCommand::Health => 0,
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
                                PaletteCommand::Agents => {
                                    if let Some(&agent_kind) = AgentKind::ALL.get(*selected_index) {
                                        app.set_agent_kind(agent_kind);
                                    }
                                }
                                PaletteCommand::Health => {}
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
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            app.mode = AppMode::List;
                        }
                        KeyCode::Char('r') => {
                            app.start_health_checks();
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
