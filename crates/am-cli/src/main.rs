use std::{
    io,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    InProgress,
    Done,
}

struct Agent {
    name: String,
    status: Status,
}

impl Status {
    fn icon(self) -> &'static str {
        match self {
            Status::InProgress => "⏳",
            Status::Done => "✅",
        }
    }

    fn color(self) -> Color {
        match self {
            Status::InProgress => Color::Yellow,
            Status::Done => Color::Green,
        }
    }

    fn toggle(&mut self) {
        *self = match self {
            Status::InProgress => Status::Done,
            Status::Done => Status::InProgress,
        };
    }
}

fn main() -> io::Result<()> {
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut agents = vec![
        Agent {
            name: "Search Agent".to_string(),
            status: Status::InProgress,
        },
        Agent {
            name: "Writing Agent".to_string(),
            status: Status::Done,
        },
        Agent {
            name: "Research Agent".to_string(),
            status: Status::InProgress,
        },
    ];

    let mut table_state = TableState::default();
    table_state.select(Some(0));

    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(100);

    loop {
        terminal.draw(|f| {
            let rects = Layout::default()
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .margin(2)
                .split(f.area());

            let selected_style = Style::default().bg(Color::DarkGray);
            let normal_style = Style::default().bg(Color::Blue);
            let header_cells = ["Agent Name", "Status"]
                .iter()
                .map(|h| Cell::from(*h).style(Style::default().fg(Color::Cyan)));
            let header = Row::new(header_cells)
                .style(normal_style)
                .height(1)
                .bottom_margin(1);
            let rows = agents.iter().map(|agent| {
                let cells = vec![
                    Cell::from(agent.name.as_str()),
                    Cell::from(agent.status.icon())
                        .style(Style::default().fg(agent.status.color())),
                ];
                Row::new(cells).height(1)
            });
            let t = Table::new(
                rows,
                [Constraint::Percentage(50), Constraint::Percentage(50)],
            )
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("Agents"))
            .row_highlight_style(selected_style)
            .highlight_symbol(">> ");

            f.render_stateful_widget(t, rects[0], &mut table_state);

            let help_message =
                ratatui::widgets::Paragraph::new("Press 'j'/'k' to navigate, 'q' to quit")
                    .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, rects[1]);
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('j') | KeyCode::Down => {
                        let i = match table_state.selected() {
                            Some(i) => {
                                if i >= agents.len() - 1 {
                                    0
                                } else {
                                    i + 1
                                }
                            }
                            None => 0,
                        };
                        table_state.select(Some(i));
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        let i = match table_state.selected() {
                            Some(i) => {
                                if i == 0 {
                                    agents.len() - 1
                                } else {
                                    i - 1
                                }
                            }
                            None => 0,
                        };
                        table_state.select(Some(i));
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= Duration::from_secs(1) {
            for agent in &mut agents {
                agent.status.toggle();
            }
            last_tick = Instant::now();
        }
    }

    // restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen,)?;
    terminal.show_cursor()?;

    Ok(())
}
