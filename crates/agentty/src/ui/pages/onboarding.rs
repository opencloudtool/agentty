use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::Page;

const DESCRIPTION: &str = "Manage coding agents in your terminal";
const HELP_HINT: &str = "q: quit";
const START_BUTTON: &str = "[ Press Enter to Start ]";
const LOGO_LINES: [&str; 5] = [
    "    _    ____ _____ _   _ _____ _____ __   __",
    "   / \\  / ___| ____| \\ | |_   _|_   _|\\ \\ / /",
    "  / _ \\| |  _|  _| |  \\| | | |   | |   \\ V / ",
    " / ___ \\ |_| | |___| |\\  | | |   | |    | |  ",
    "/_/   \\_\\____|_____|_| \\_| |_|   |_|    |_|  ",
];

/// Full-screen onboarding page shown when there are no sessions.
pub struct OnboardingPage;

impl Page for OnboardingPage {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        let logo_width =
            Self::saturating_u16(LOGO_LINES.iter().map(|line| line.len()).max().unwrap_or(0));
        let content_width = logo_width
            .max(Self::saturating_u16(version.len()))
            .max(Self::saturating_u16(DESCRIPTION.len()))
            .max(Self::saturating_u16(START_BUTTON.len()));
        let content_height = Self::saturating_u16(LOGO_LINES.len() + 6);

        let page_chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);
        let main_area = page_chunks[0];
        let help_area = page_chunks[1];

        let vertical_chunks = Layout::default()
            .constraints([
                Constraint::Min(0),
                Constraint::Length(content_height),
                Constraint::Min(0),
            ])
            .split(main_area);
        let horizontal_chunks = Layout::default()
            .constraints([
                Constraint::Min(0),
                Constraint::Length(content_width),
                Constraint::Min(0),
            ])
            .split(vertical_chunks[1]);

        let logo_lines: Vec<Line<'_>> = LOGO_LINES
            .iter()
            .map(|line| {
                Line::from(Span::styled(
                    *line,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            })
            .collect();
        let mut lines = Vec::with_capacity(LOGO_LINES.len() + 6);
        lines.extend(logo_lines);
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            version,
            Style::default().fg(Color::Gray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(DESCRIPTION));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            START_BUTTON,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        let onboarding = Paragraph::new(lines).alignment(Alignment::Center);
        f.render_widget(onboarding, horizontal_chunks[1]);

        let help_message = Paragraph::new(HELP_HINT).style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, help_area);
    }
}

impl OnboardingPage {
    fn saturating_u16(value: usize) -> u16 {
        u16::try_from(value).unwrap_or(u16::MAX)
    }
}
