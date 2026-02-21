use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::domain::session::Session;
use crate::model::DailyActivity;
use crate::ui::Page;
use crate::ui::pages::session_list::{model_column_width, project_column_width};
use crate::ui::util::{
    build_activity_heatmap_grid, current_day_key_utc, format_token_count, heatmap_intensity_level,
    heatmap_max_count,
};

const DAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const HEATMAP_SECTION_HEIGHT: u16 = 10;

/// Stats dashboard showing activity heatmap and per-session token statistics.
pub struct StatsPage<'a> {
    sessions: &'a [Session],
    stats_activity: &'a [DailyActivity],
}

impl<'a> StatsPage<'a> {
    /// Creates a stats page renderer.
    pub fn new(sessions: &'a [Session], stats_activity: &'a [DailyActivity]) -> Self {
        Self {
            sessions,
            stats_activity,
        }
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
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(HEATMAP_SECTION_HEIGHT),
                Constraint::Min(0),
            ])
            .split(main_area);

        self.render_heatmap(f, main_chunks[0]);
        self.render_table(f, main_chunks[1]);
        self.render_footer(f, footer_area);
    }
}

impl StatsPage<'_> {
    fn render_heatmap(&self, f: &mut Frame, area: Rect) {
        let heatmap = Paragraph::new(self.build_heatmap_lines()).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Activity Heatmap (Last 53 Weeks)"),
        );

        f.render_widget(heatmap, area);
    }

    fn render_table(&self, f: &mut Frame, area: Rect) {
        let selected_style = Style::default().bg(Color::DarkGray);
        let normal_style = Style::default().bg(Color::Gray).fg(Color::Black);
        let header_cells = ["Session", "Project", "Model", "Input", "Output"]
            .iter()
            .map(|header| Cell::from(*header));
        let header = Row::new(header_cells)
            .style(normal_style)
            .height(1)
            .bottom_margin(1);

        let rows = self.sessions.iter().map(|session| {
            let cells = vec![
                Cell::from(session.display_title().to_string()),
                Cell::from(session.project_name.clone()),
                Cell::from(session.model.as_str()),
                Cell::from(format_token_count(session.stats.input_tokens)),
                Cell::from(format_token_count(session.stats.output_tokens)),
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
        .block(Block::default().borders(Borders::ALL).title("Token Stats"))
        .row_highlight_style(selected_style)
        .highlight_symbol("   ");

        f.render_widget(table, area);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let footer_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Min(0)])
            .split(area);

        let help = Paragraph::new("q: quit").style(Style::default().fg(Color::Gray));
        f.render_widget(help, footer_chunks[0]);

        let total_input: u64 = self
            .sessions
            .iter()
            .map(|session| session.stats.input_tokens)
            .sum();
        let total_output: u64 = self
            .sessions
            .iter()
            .map(|session| session.stats.output_tokens)
            .sum();

        let input_display = format_token_count(total_input);
        let output_display = format_token_count(total_output);
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

    fn build_heatmap_lines(&self) -> Vec<Line<'static>> {
        let end_day_key = current_day_key_utc();
        let grid = build_activity_heatmap_grid(self.stats_activity, end_day_key);
        let max_count = heatmap_max_count(&grid);
        let mut lines: Vec<Line<'static>> = Vec::new();

        for (day_index, day_label) in DAY_LABELS.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!("{day_label} "),
                Style::default().fg(Color::Gray),
            )];

            for cell_count in &grid[day_index] {
                let intensity = heatmap_intensity_level(*cell_count, max_count);
                spans.push(Span::styled(
                    "  ",
                    Style::default().bg(Self::heatmap_color(intensity)),
                ));
            }

            lines.push(Line::from(spans));
        }

        let mut legend = vec![Span::styled("Less ", Style::default().fg(Color::Gray))];
        for intensity in 0_u8..=4 {
            legend.push(Span::styled(
                "  ",
                Style::default().bg(Self::heatmap_color(intensity)),
            ));
            legend.push(Span::raw(" "));
        }
        legend.push(Span::styled("More", Style::default().fg(Color::Gray)));
        legend.push(Span::raw(format!(" | Max/day: {max_count}")));
        lines.push(Line::from(legend));

        lines
    }

    fn heatmap_color(intensity: u8) -> Color {
        match intensity {
            1 => Color::Rgb(14, 68, 41),
            2 => Color::Rgb(0, 109, 50),
            3 => Color::Rgb(38, 166, 65),
            4 => Color::Rgb(57, 211, 83),
            _ => Color::Rgb(33, 38, 45),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::model::{PermissionMode, SessionSize, SessionStats, Status};

    fn session_fixture() -> Session {
        Session {
            base_branch: "main".to_string(),
            folder: PathBuf::new(),
            id: "session-id".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            permission_mode: PermissionMode::default(),
            project_name: "project".to_string(),
            prompt: String::new(),
            size: SessionSize::Xs,
            stats: SessionStats {
                input_tokens: 1500,
                output_tokens: 700,
            },
            status: Status::Review,
            summary: None,
            title: Some("Stats Session".to_string()),
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_render_shows_activity_heatmap_legend() {
        // Arrange
        let sessions = vec![session_fixture()];
        let activity = vec![DailyActivity {
            day_key: current_day_key_utc(),
            session_count: 3,
        }];
        let mut page = StatsPage::new(&sessions, &activity);
        let backend = ratatui::backend::TestBackend::new(160, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                crate::ui::Page::render(&mut page, frame, area);
            })
            .expect("failed to draw stats page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Activity Heatmap"));
        assert!(text.contains("Less"));
        assert!(text.contains("More"));
    }

    #[test]
    fn test_render_preserves_table_rows_and_footer_summary() {
        // Arrange
        let sessions = vec![session_fixture()];
        let activity: Vec<DailyActivity> = Vec::new();
        let mut page = StatsPage::new(&sessions, &activity);
        let backend = ratatui::backend::TestBackend::new(160, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                crate::ui::Page::render(&mut page, frame, area);
            })
            .expect("failed to draw stats page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Token Stats"));
        assert!(text.contains("Stats Session"));
        assert!(text.contains("Sessions: 1"));
        assert!(text.contains("Input: 1.5k"));
        assert!(text.contains("Output: 700"));
    }
}
