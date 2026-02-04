use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use ag_cli::app::App;
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
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(100);

    loop {
        terminal.draw(|f| ui::render(f, &app.mode, &app.agents, &mut app.table_state))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match &mut app.mode {
                    AppMode::List => match key.code {
                        KeyCode::Char('q') => break,
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
                                if i < app.agents.len() {
                                    app.mode = AppMode::View { agent_index: i };
                                }
                            }
                        }
                        KeyCode::Char('d') => {
                            app.delete_selected_agent();
                        }
                        KeyCode::Char('o') => {
                            if let Some(agent) = app.selected_agent() {
                                let folder = agent.folder.clone();
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
                    AppMode::View { .. } => {
                        if key.code == KeyCode::Char('q') {
                            app.mode = AppMode::List;
                        }
                    }
                    AppMode::Prompt { input } => match key.code {
                        KeyCode::Enter => {
                            let prompt = input.clone();
                            app.mode = AppMode::List;
                            if !prompt.is_empty() {
                                app.add_agent(prompt);
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
