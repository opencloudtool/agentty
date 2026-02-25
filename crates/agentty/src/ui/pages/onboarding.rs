use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::Page;
use crate::ui::state::help_action;

const DESCRIPTION: &str = "Agentic Development Environment (ADE) in your terminal";
const START_BUTTON: &str = "[ Press Enter to Start ]";
const LOGO_LINES: [&str; 5] = [
    "    _    ____ _____ _   _ _____ _____ __   __",
    "   / \\  / ___| ____| \\ | |_   _|_   _|\\ \\ / /",
    "  / _ \\| |  _|  _| |  \\| | | |   | |   \\ V / ",
    " / ___ \\ |_| | |___| |\\  | | |   | |    | |  ",
    "/_/   \\_\\____|_____|_| \\_| |_|   |_|    |_|  ",
];

/// Full-screen onboarding page shown when there are no sessions.
pub struct OnboardingPage {
    current_version: String,
    latest_available_version: Option<String>,
}

impl OnboardingPage {
    /// Creates the onboarding page renderer with version metadata.
    pub fn new(current_version: String, latest_available_version: Option<String>) -> Self {
        Self {
            current_version,
            latest_available_version,
        }
    }

    fn current_version_message(&self) -> String {
        self.current_version.clone()
    }

    fn update_notice_message(&self) -> Option<String> {
        self.latest_available_version
            .as_ref()
            .map(|latest_available_version| {
                format!(
                    "{latest_available_version} version available update with npm i -g \
                     agentty@latest"
                )
            })
    }

    fn content_width(
        logo_width: u16,
        current_version_message: &str,
        update_notice_message: Option<&str>,
    ) -> u16 {
        let mut content_width = logo_width
            .max(Self::saturating_u16(current_version_message.len()))
            .max(Self::saturating_u16(DESCRIPTION.len()))
            .max(Self::saturating_u16(START_BUTTON.len()));
        if let Some(update_notice_message) = update_notice_message {
            content_width = content_width.max(Self::saturating_u16(update_notice_message.len()));
        }

        content_width
    }

    fn content_height(update_notice_message: Option<&str>) -> u16 {
        let additional_line_count = usize::from(update_notice_message.is_some());

        Self::saturating_u16(LOGO_LINES.len() + 6 + additional_line_count)
    }

    fn saturating_u16(value: usize) -> u16 {
        u16::try_from(value).unwrap_or(u16::MAX)
    }
}

impl Page for OnboardingPage {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let current_version_message = self.current_version_message();
        let update_notice_message = self.update_notice_message();
        let logo_width =
            Self::saturating_u16(LOGO_LINES.iter().map(|line| line.len()).max().unwrap_or(0));
        let content_width = Self::content_width(
            logo_width,
            &current_version_message,
            update_notice_message.as_deref(),
        );
        let content_height = Self::content_height(update_notice_message.as_deref());

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
        let mut lines = Vec::with_capacity(usize::from(content_height));
        lines.extend(logo_lines);
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            current_version_message,
            Style::default().fg(Color::Gray),
        )));
        if let Some(update_notice_message) = update_notice_message {
            lines.push(Line::from(Span::styled(
                update_notice_message,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
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

        let help_message = Paragraph::new(help_action::footer_text(
            &help_action::onboarding_footer_actions(),
        ))
        .style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, help_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_onboarding_render_shows_current_version() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut onboarding = OnboardingPage::new("v0.1.12".to_string(), None);

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                crate::ui::Page::render(&mut onboarding, frame, area);
            })
            .expect("failed to draw onboarding");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("v0.1.12"));
        assert!(!text.contains("version available update"));
    }

    #[test]
    fn test_onboarding_render_shows_update_notice_when_available() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut onboarding =
            OnboardingPage::new("v0.1.12".to_string(), Some("v0.1.13".to_string()));

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                crate::ui::Page::render(&mut onboarding, frame, area);
            })
            .expect("failed to draw onboarding");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("v0.1.12"));
        assert!(text.contains("v0.1.13 version available update with npm i -g agentty@latest"));
    }
}
