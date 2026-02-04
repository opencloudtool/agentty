use std::io;
use std::time::{Duration, Instant};

use am_cli::app::App;
use am_cli::model::{Agent, AppMode, Status};
use am_cli::ui;
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
                        _ => {}
                    },
                    AppMode::Prompt { input } => match key.code {
                        KeyCode::Enter => {
                            if !input.is_empty() {
                                app.agents.push(Agent {
                                    name: format!("Agent {}", app.agents.len() + 1),
                                    prompt: input.clone(),
                                    status: Status::InProgress,
                                });
                                if app.table_state.selected().is_none() {
                                    app.table_state.select(Some(0));
                                }
                            }
                            app.mode = AppMode::List;
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
            app.toggle_all();
            last_tick = Instant::now();
        }
    }

    // restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen,)?;
    terminal.show_cursor()?;

    Ok(())
}
