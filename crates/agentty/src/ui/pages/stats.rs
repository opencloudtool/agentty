use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::model::Session;
use crate::ui::Page;
use crate::ui::pages::session_list::{model_column_width, project_column_width};
use crate::ui::util::format_optional_tokens;

/// Stats dashboard showing per-session token statistics.
pub struct StatsPage<'a> {
    sessions: &'a [Session],
}

impl<'a> StatsPage<'a> {
    pub fn new(sessions: &'a [Session]) -> Self {
        Self { sessions }
    }
}

impl Page for StatsPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        let selected_style = Style::default().bg(Color::DarkGray);
        let normal_style = Style::default().bg(Color::Gray).fg(Color::Black);
        let header_cells = ["Session", "Project", "Model", "Input", "Output"]
            .iter()
            .map(|h| Cell::from(*h));
        let header = Row::new(header_cells)
            .style(normal_style)
            .height(1)
            .bottom_margin(1);

        let rows = self.sessions.iter().map(|session| {
            let cells = vec![
                Cell::from(session.display_title().to_string()),
                Cell::from(session.project_name.clone()),
                Cell::from(session.model.as_str()),
                Cell::from(format_optional_tokens(session.stats.input_tokens)),
                Cell::from(format_optional_tokens(session.stats.output_tokens)),
            ];

            Row::new(cells).height(1)
        });

        let table = Table::new(
            rows,
            [
                Constraint::Min(0),
                project_column_width(self.sessions),
                model_column_width(self.sessions),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
        )
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Stats"))
        .row_highlight_style(selected_style)
        .highlight_symbol("   ");

        f.render_widget(table, main_area);

        self.render_footer(f, footer_area);
    }
}

impl StatsPage<'_> {
    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let footer_chunks = Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Min(0)])
            .split(area);

        let help = Paragraph::new("q: quit").style(Style::default().fg(Color::Gray));
        f.render_widget(help, footer_chunks[0]);

        let total_input: i64 = self
            .sessions
            .iter()
            .filter_map(|session| session.stats.input_tokens)
            .sum();
        let total_output: i64 = self
            .sessions
            .iter()
            .filter_map(|session| session.stats.output_tokens)
            .sum();
        let has_tokens = self
            .sessions
            .iter()
            .any(|session| session.stats.input_tokens.is_some());

        let input_display = if has_tokens {
            format_optional_tokens(Some(total_input))
        } else {
            "-".to_string()
        };
        let output_display = if has_tokens {
            format_optional_tokens(Some(total_output))
        } else {
            "-".to_string()
        };

        let summary = format!(
            "Sessions: {} | Input: {} | Output: {}",
            self.sessions.len(),
            input_display,
            output_display
        );
        let stats = Paragraph::new(summary)
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Right);
        f.render_widget(stats, footer_chunks[1]);
    }
}
