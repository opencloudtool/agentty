use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::domain::session::{
    AllTimeModelUsage, CodexUsageLimitWindow, CodexUsageLimits, DailyActivity, Session,
};
use crate::ui::Page;
use crate::ui::pages::session_list::{model_column_width, project_column_width};
use crate::ui::state::help_action;
use crate::ui::util::{
    build_activity_heatmap_grid, build_visible_heatmap_month_row, current_day_key_local,
    format_duration_compact, format_token_count, heatmap_intensity_level, heatmap_max_count,
    visible_heatmap_week_count,
};

const DAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const HEATMAP_CELL_WIDTH: usize = 2;
const HEATMAP_DAY_LABEL_WIDTH: usize = 4;
const HEATMAP_SECTION_HEIGHT: u16 = 11;
const MIN_HEATMAP_PANEL_WIDTH: u16 = 20;
const MIN_SUMMARY_SECTION_WIDTH: u16 = 26;
const SUMMARY_SECTION_WIDTH: u16 = 44;
const USAGE_BAR_WIDTH: usize = 20;
const USAGE_SECTION_HEIGHT: u16 = 5;

/// Stats dashboard showing activity heatmap, all-time summaries, and
/// per-session token statistics.
pub struct StatsPage<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    longest_session_duration_seconds: u64,
    sessions: &'a [Session],
    stats_activity: &'a [DailyActivity],
}

impl<'a> StatsPage<'a> {
    /// Creates a stats page renderer from live sessions and persisted
    /// historical aggregates.
    pub fn new(
        sessions: &'a [Session],
        stats_activity: &'a [DailyActivity],
        all_time_model_usage: &'a [AllTimeModelUsage],
        longest_session_duration_seconds: u64,
        codex_usage_limits: Option<CodexUsageLimits>,
    ) -> Self {
        Self {
            all_time_model_usage,
            codex_usage_limits,
            longest_session_duration_seconds,
            sessions,
            stats_activity,
        }
    }
}

impl Page for StatsPage<'_> {
    /// Renders the dashboard with a responsive top row that hides the summary
    /// panel when the terminal is too narrow.
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
                Constraint::Length(USAGE_SECTION_HEIGHT),
                Constraint::Min(0),
            ])
            .split(main_area);

        let summary_width = Self::summary_section_width(main_chunks[0].width);
        if summary_width == 0 {
            self.render_heatmap(f, main_chunks[0]);
        } else {
            let top_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(0), Constraint::Length(summary_width)])
                .split(main_chunks[0]);

            self.render_heatmap(f, top_chunks[0]);
            self.render_summary(f, top_chunks[1]);
        }

        self.render_usage(f, main_chunks[1]);
        self.render_table(f, main_chunks[2]);
        self.render_footer(f, footer_area);
    }
}

impl StatsPage<'_> {
    /// Returns summary-panel width for the current terminal width.
    ///
    /// The summary is hidden when preserving a minimum heatmap width would
    /// leave less than the summary minimum.
    fn summary_section_width(total_width: u16) -> u16 {
        let max_summary_width = total_width.saturating_sub(MIN_HEATMAP_PANEL_WIDTH);
        if max_summary_width < MIN_SUMMARY_SECTION_WIDTH {
            return 0;
        }

        SUMMARY_SECTION_WIDTH.min(max_summary_width)
    }

    /// Renders the activity heatmap with a width-aware week count.
    fn render_heatmap(&self, f: &mut Frame, area: Rect) {
        let heatmap = Paragraph::new(self.build_heatmap_lines(area.width)).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Activity Heatmap (Last 53 Weeks)"),
        );

        f.render_widget(heatmap, area);
    }

    /// Renders aggregate session metrics beside the heatmap.
    fn render_summary(&self, f: &mut Frame, area: Rect) {
        let summary = Paragraph::new(self.build_summary_lines()).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Session Stats"),
        );

        f.render_widget(summary, area);
    }

    /// Renders account-level Codex usage windows and reset countdowns.
    fn render_usage(&self, f: &mut Frame, area: Rect) {
        let usage = Paragraph::new(self.build_usage_lines())
            .block(Block::default().borders(Borders::ALL).title("Usage"));

        f.render_widget(usage, area);
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

        let help = Paragraph::new(help_action::footer_text(
            &help_action::stats_footer_actions(),
        ))
        .style(Style::default().fg(Color::Gray));
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

    /// Builds per-window Codex usage lines for the usage panel.
    fn build_usage_lines(&self) -> Vec<Line<'static>> {
        let Some(codex_usage_limits) = self.codex_usage_limits else {
            return vec![
                Line::from("Codex usage unavailable."),
                Line::from(Span::styled(
                    "Run `codex login` and refresh.",
                    Style::default().fg(Color::Gray),
                )),
            ];
        };
        let now = Self::current_unix_timestamp();
        let mut usage_lines: Vec<Line<'static>> = Vec::new();

        if let Some(primary_window) = codex_usage_limits.primary {
            usage_lines.push(Self::usage_line(primary_window, now));
        }
        if let Some(secondary_window) = codex_usage_limits.secondary {
            usage_lines.push(Self::usage_line(secondary_window, now));
        }
        if usage_lines.is_empty() {
            return vec![
                Line::from("Codex usage unavailable."),
                Line::from(Span::styled(
                    "Refresh stats to retry loading limits.",
                    Style::default().fg(Color::Gray),
                )),
            ];
        }

        usage_lines
    }

    /// Builds heatmap lines and trims visible week columns for narrow widths.
    fn build_heatmap_lines(&self, available_width: u16) -> Vec<Line<'static>> {
        let content_width = usize::from(available_width.saturating_sub(2));
        let end_day_key = current_day_key_local();
        let activity = self.build_local_activity();
        let grid = build_activity_heatmap_grid(&activity, end_day_key);
        let max_count = heatmap_max_count(&grid);
        let visible_week_count =
            visible_heatmap_week_count(content_width, HEATMAP_DAY_LABEL_WIDTH, HEATMAP_CELL_WIDTH);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let month_row = build_visible_heatmap_month_row(
            end_day_key,
            HEATMAP_DAY_LABEL_WIDTH,
            HEATMAP_CELL_WIDTH,
            visible_week_count,
        );
        lines.push(Line::from(Span::styled(
            month_row,
            Style::default().fg(Color::Gray),
        )));

        for (day_index, day_label) in DAY_LABELS.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!("{day_label} "),
                Style::default().fg(Color::Gray),
            )];

            let first_visible_week = grid[day_index].len().saturating_sub(visible_week_count);
            for cell_count in &grid[day_index][first_visible_week..] {
                let intensity = heatmap_intensity_level(*cell_count, max_count);
                spans.push(Span::styled(
                    "  ",
                    Style::default().bg(Self::heatmap_color(intensity)),
                ));
            }

            lines.push(Line::from(spans));
        }

        if content_width < 24 {
            lines.push(Line::from(Span::styled(
                format!("Max/day: {max_count}"),
                Style::default().fg(Color::Gray),
            )));

            return lines;
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
        if content_width >= 36 {
            legend.push(Span::raw(format!(" | Max/day: {max_count}")));
        }
        lines.push(Line::from(legend));

        lines
    }

    /// Returns persisted local-day activity aggregates for heatmap rendering.
    fn build_local_activity(&self) -> Vec<DailyActivity> {
        self.stats_activity.to_vec()
    }

    /// Builds summary lines for favorite model, longest `agentty` session
    /// duration, and all-time per-model combined token totals.
    fn build_summary_lines(&self) -> Vec<Line<'static>> {
        let favorite_model = Self::favorite_model_name(self.all_time_model_usage)
            .unwrap_or_else(|| "n/a".to_string());
        let longest_session = self.longest_session_summary();
        let longest_label = longest_session.unwrap_or_else(|| "n/a".to_string());
        let mut lines = vec![
            Line::from(format!("Favorite model: {favorite_model}")),
            Line::from(format!("Longest Agentty session: {longest_label}")),
            Line::from(""),
            Line::from(Span::styled(
                "Model stats (All time)",
                Style::default().fg(Color::Gray),
            )),
        ];

        if self.all_time_model_usage.is_empty() {
            lines.push(Line::from("No sessions yet"));

            return lines;
        }

        for model_usage in self.all_time_model_usage {
            let combined_tokens = format_token_count(
                model_usage
                    .input_tokens
                    .saturating_add(model_usage.output_tokens),
            );
            lines.push(Line::from(format!(
                "{}: {combined_tokens}",
                model_usage.model.as_str()
            )));
        }

        lines
    }

    /// Returns the model with the highest session count.
    fn favorite_model_name(all_time_model_usage: &[AllTimeModelUsage]) -> Option<String> {
        let mut favorite: Option<&AllTimeModelUsage> = None;

        for summary in all_time_model_usage {
            favorite = match favorite {
                None => Some(summary),
                Some(favorite_summary)
                    if summary.session_count > favorite_summary.session_count =>
                {
                    Some(summary)
                }
                Some(favorite_summary)
                    if summary.session_count == favorite_summary.session_count
                        && summary.model.as_str() < favorite_summary.model.as_str() =>
                {
                    Some(summary)
                }
                _ => favorite,
            };
        }

        favorite.map(|summary| format!("{} ({})", summary.model.as_str(), summary.session_count))
    }

    /// Returns a compact duration label for the longest `agentty` session
    /// across both live and persisted historical sessions.
    fn longest_session_summary(&self) -> Option<String> {
        let longest_duration = self
            .live_longest_session_duration_seconds()
            .max(self.longest_session_duration_seconds);
        if longest_duration == 0 {
            return None;
        }

        let duration_seconds = i64::try_from(longest_duration).unwrap_or(i64::MAX);

        Some(format_duration_compact(duration_seconds))
    }

    /// Returns the longest live session duration from currently loaded
    /// sessions.
    fn live_longest_session_duration_seconds(&self) -> u64 {
        let mut longest_duration = 0_u64;

        for session in self.sessions {
            let duration_seconds = session.updated_at.saturating_sub(session.created_at).max(0);
            let duration_seconds = u64::try_from(duration_seconds).unwrap_or_default();
            if duration_seconds <= longest_duration {
                continue;
            }

            longest_duration = duration_seconds;
        }

        longest_duration
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

    /// Builds a single usage row with progress bar and reset countdown.
    fn usage_line(window: CodexUsageLimitWindow, now: i64) -> Line<'static> {
        let percent_left = 100_u8.saturating_sub(window.used_percent);
        let label = Self::usage_window_label(window.window_minutes);
        let progress = Self::usage_progress_bar(percent_left);
        let reset_display = Self::format_reset_eta(window.resets_at, now);

        Line::from(format!(
            "{label:<12} {progress} {percent_left:>3}% left ({reset_display})"
        ))
    }

    /// Converts a usage window duration to a short label.
    fn usage_window_label(window_minutes: Option<u32>) -> String {
        match window_minutes {
            Some(300) => "5h limit".to_string(),
            Some(10_080) => "Weekly limit".to_string(),
            Some(window_minutes) => format!("{window_minutes}m limit"),
            None => "Limit".to_string(),
        }
    }

    /// Renders a fixed-width ASCII progress bar for percentage-left values.
    fn usage_progress_bar(percent_left: u8) -> String {
        let filled_cells = usize::from(percent_left).saturating_mul(USAGE_BAR_WIDTH) / 100;
        let empty_cells = USAGE_BAR_WIDTH.saturating_sub(filled_cells);

        format!("[{}{}]", "#".repeat(filled_cells), ".".repeat(empty_cells))
    }

    /// Returns a compact reset countdown for a Unix timestamp.
    fn format_reset_eta(resets_at: Option<i64>, now: i64) -> String {
        let Some(resets_at) = resets_at else {
            return "reset unavailable".to_string();
        };
        if resets_at <= now {
            return "resets now".to_string();
        }

        let remaining_seconds = resets_at.saturating_sub(now);
        let remaining_days = remaining_seconds / 86_400;
        let remaining_hours = (remaining_seconds % 86_400) / 3_600;
        let remaining_minutes = (remaining_seconds % 3_600) / 60;

        if remaining_days > 0 {
            return format!("resets in {remaining_days}d {remaining_hours}h");
        }

        if remaining_hours > 0 {
            return format!("resets in {remaining_hours}h {remaining_minutes}m");
        }

        format!("resets in {}m", remaining_minutes.max(1))
    }

    /// Returns the current Unix timestamp in seconds.
    fn current_unix_timestamp() -> i64 {
        let now_duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        i64::try_from(now_duration.as_secs()).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::session::{AllTimeModelUsage, SessionSize, SessionStats, Status};

    fn session_fixture() -> Session {
        session_fixture_with(
            "session-id",
            "Stats Session",
            AgentModel::Gemini3FlashPreview,
            1_500,
            700,
            0,
            1_800,
        )
    }

    fn session_fixture_with(
        session_id: &str,
        title: &str,
        model: AgentModel,
        input_tokens: u64,
        output_tokens: u64,
        created_at: i64,
        updated_at: i64,
    ) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at,
            folder: PathBuf::new(),
            id: session_id.to_string(),
            model,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: String::new(),
            size: SessionSize::Xs,
            stats: SessionStats {
                input_tokens,
                output_tokens,
            },
            status: Status::Review,
            summary: None,
            title: Some(title.to_string()),
            updated_at,
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn all_time_usage_fixture() -> Vec<AllTimeModelUsage> {
        vec![
            AllTimeModelUsage {
                input_tokens: 3_000,
                model: AgentModel::Gpt53Codex.as_str().to_string(),
                output_tokens: 1_200,
                session_count: 2,
            },
            AllTimeModelUsage {
                input_tokens: 300,
                model: AgentModel::ClaudeOpus46.as_str().to_string(),
                output_tokens: 200,
                session_count: 1,
            },
        ]
    }

    #[test]
    fn test_render_shows_activity_heatmap_legend() {
        // Arrange
        let sessions = vec![session_fixture()];
        let activity = vec![DailyActivity {
            day_key: current_day_key_local(),
            session_count: 3,
        }];
        let all_time_usage: Vec<AllTimeModelUsage> = Vec::new();
        let mut page = StatsPage::new(&sessions, &activity, &all_time_usage, 0, None);
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
    fn test_build_heatmap_lines_uses_persisted_activity_for_max_count() {
        // Arrange
        let now_seconds = StatsPage::current_unix_timestamp();
        let sessions = vec![session_fixture_with(
            "session-1",
            "Active Session",
            AgentModel::Gpt53Codex,
            10,
            10,
            now_seconds,
            now_seconds,
        )];
        let activity = vec![DailyActivity {
            day_key: current_day_key_local(),
            session_count: 50,
        }];
        let all_time_usage: Vec<AllTimeModelUsage> = Vec::new();
        let page = StatsPage::new(&sessions, &activity, &all_time_usage, 0, None);

        // Act
        let heatmap_lines = page.build_heatmap_lines(160);
        let rendered_text = heatmap_lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(rendered_text.contains("Max/day: 50"));
    }

    #[test]
    fn test_build_heatmap_lines_trims_visible_weeks_on_narrow_width() {
        // Arrange
        let sessions = vec![session_fixture()];
        let activity = vec![DailyActivity {
            day_key: current_day_key_local(),
            session_count: 1,
        }];
        let all_time_usage: Vec<AllTimeModelUsage> = Vec::new();
        let page = StatsPage::new(&sessions, &activity, &all_time_usage, 0, None);

        // Act
        let heatmap_lines = page.build_heatmap_lines(28);
        let monday_row = &heatmap_lines[1];

        // Assert
        assert_eq!(monday_row.spans.len(), 12);
    }

    #[test]
    fn test_render_shows_session_summary_panel_metrics() {
        // Arrange
        let sessions = vec![
            session_fixture_with(
                "session-1",
                "Longest Session",
                AgentModel::Gpt53Codex,
                1_000,
                500,
                0,
                7_200,
            ),
            session_fixture_with(
                "session-2",
                "Second Session",
                AgentModel::Gpt53Codex,
                2_000,
                700,
                10,
                20,
            ),
            session_fixture_with(
                "session-3",
                "Claude Session",
                AgentModel::ClaudeOpus46,
                300,
                200,
                30,
                90,
            ),
        ];
        let activity: Vec<DailyActivity> = Vec::new();
        let all_time_usage = all_time_usage_fixture();
        let mut page = StatsPage::new(&sessions, &activity, &all_time_usage, 0, None);
        let backend = ratatui::backend::TestBackend::new(220, 30);
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
        assert!(text.contains("Session Stats"));
        assert!(text.contains("Favorite model: gpt-5.3-codex (2)"));
        assert!(text.contains("Longest Agentty session: 2h 0m"));
        assert!(text.contains("Model stats (All time)"));
        assert!(text.contains("gpt-5.3-codex: 4.2k"));
        assert!(text.contains("claude-opus-4-6: 500"));
    }

    #[test]
    fn test_render_hides_summary_panel_on_narrow_terminal() {
        // Arrange
        let sessions = vec![session_fixture()];
        let activity: Vec<DailyActivity> = Vec::new();
        let all_time_usage = all_time_usage_fixture();
        let mut page = StatsPage::new(&sessions, &activity, &all_time_usage, 0, None);
        let backend = ratatui::backend::TestBackend::new(40, 30);
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
        assert!(!text.contains("Session Stats"));
    }

    #[test]
    fn test_render_preserves_table_rows_and_footer_summary() {
        // Arrange
        let sessions = vec![session_fixture()];
        let activity: Vec<DailyActivity> = Vec::new();
        let all_time_usage = all_time_usage_fixture();
        let usage_limits = Some(CodexUsageLimits {
            primary: Some(CodexUsageLimitWindow {
                resets_at: Some(i64::MAX),
                used_percent: 26,
                window_minutes: Some(300),
            }),
            secondary: Some(CodexUsageLimitWindow {
                resets_at: Some(i64::MAX),
                used_percent: 24,
                window_minutes: Some(10_080),
            }),
        });
        let mut page = StatsPage::new(&sessions, &activity, &all_time_usage, 0, usage_limits);
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
        assert!(text.contains("Usage"));
        assert!(text.contains("5h limit"));
        assert!(text.contains("Weekly limit"));
        assert!(text.contains("Stats Session"));
        assert!(text.contains("Sessions: 1"));
        assert!(text.contains("Input: 1.5k"));
        assert!(text.contains("Output: 700"));
    }

    #[test]
    fn test_render_shows_single_usage_window_when_secondary_is_missing() {
        // Arrange
        let sessions = vec![session_fixture()];
        let activity: Vec<DailyActivity> = Vec::new();
        let all_time_usage = all_time_usage_fixture();
        let usage_limits = Some(CodexUsageLimits {
            primary: Some(CodexUsageLimitWindow {
                resets_at: None,
                used_percent: 26,
                window_minutes: Some(300),
            }),
            secondary: None,
        });
        let mut page = StatsPage::new(&sessions, &activity, &all_time_usage, 0, usage_limits);
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
        assert!(text.contains("5h limit"));
        assert!(text.contains("reset unavailable"));
    }

    #[test]
    fn test_render_uses_persisted_longest_session_when_live_list_is_empty() {
        // Arrange
        let sessions: Vec<Session> = Vec::new();
        let activity: Vec<DailyActivity> = Vec::new();
        let all_time_usage = all_time_usage_fixture();
        let mut page = StatsPage::new(&sessions, &activity, &all_time_usage, 18_000, None);
        let backend = ratatui::backend::TestBackend::new(220, 30);
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
        assert!(text.contains("Longest Agentty session: 5h 0m"));
        assert!(text.contains("gpt-5.3-codex: 4.2k"));
    }
}
