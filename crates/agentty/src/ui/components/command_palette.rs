use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::agent::AgentKind;
use crate::model::{PaletteCommand, PaletteFocus};
use crate::ui::Component;

pub struct CommandPaletteInput<'a> {
    focus: PaletteFocus,
    input: &'a str,
    selected_index: usize,
}

impl<'a> CommandPaletteInput<'a> {
    pub fn new(input: &'a str, selected_index: usize, focus: PaletteFocus) -> Self {
        Self {
            focus,
            input,
            selected_index,
        }
    }
}

impl Component for CommandPaletteInput<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let filtered = PaletteCommand::filter(self.input);
        let show_dropdown = !self.input.is_empty() && !filtered.is_empty();
        let dropdown_height = if show_dropdown {
            u16::try_from(filtered.len()).unwrap_or(0) + 2 // +2 for borders
        } else {
            0
        };

        let chunks = Layout::default()
            .constraints([
                Constraint::Min(0),
                Constraint::Length(dropdown_height),
                Constraint::Length(1),
            ])
            .split(area);

        let dropdown_area = chunks[1];
        let input_area = chunks[2];

        // Render dropdown
        if show_dropdown {
            let rows: Vec<Line> = filtered
                .iter()
                .enumerate()
                .map(|(index, cmd)| {
                    let is_selected =
                        self.focus == PaletteFocus::Dropdown && index == self.selected_index;
                    let prefix = if is_selected { ">> " } else { "   " };
                    let style = if is_selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    Line::from(Span::styled(format!("{prefix}{}", cmd.label()), style))
                })
                .collect();

            let dropdown = Paragraph::new(rows).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            f.render_widget(Clear, dropdown_area);
            f.render_widget(dropdown, dropdown_area);
        }

        // Render input line
        let input_line = Line::from(vec![
            Span::styled("> / ", Style::default().fg(Color::Cyan)),
            Span::raw(self.input),
        ]);
        let input_widget = Paragraph::new(input_line);
        f.render_widget(Clear, input_area);
        f.render_widget(input_widget, input_area);

        // Set cursor position
        let cursor_x = input_area
            .x
            .saturating_add(4) // "> / " prefix
            .saturating_add(u16::try_from(self.input.len()).unwrap_or(0));
        f.set_cursor_position((cursor_x, input_area.y));
    }
}

pub struct CommandOptionList {
    command: PaletteCommand,
    selected_index: usize,
}

impl CommandOptionList {
    pub fn new(command: PaletteCommand, selected_index: usize) -> Self {
        Self {
            command,
            selected_index,
        }
    }

    fn options(&self) -> Vec<String> {
        match self.command {
            PaletteCommand::Agents => AgentKind::ALL
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            PaletteCommand::Health => Vec::new(),
        }
    }
}

impl Component for CommandOptionList {
    fn render(&self, f: &mut Frame, area: Rect) {
        let options = self.options();
        let dropdown_height = u16::try_from(options.len()).unwrap_or(0) + 2; // +2 for borders

        let chunks = Layout::default()
            .constraints([
                Constraint::Min(0),
                Constraint::Length(dropdown_height),
                Constraint::Length(1),
            ])
            .split(area);

        let dropdown_area = chunks[1];
        let label_area = chunks[2];

        // Render options dropdown
        let rows: Vec<Line> = options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let is_selected = index == self.selected_index;
                let prefix = if is_selected { ">> " } else { "   " };
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(format!("{prefix}{option}"), style))
            })
            .collect();

        let dropdown = Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(Clear, dropdown_area);
        f.render_widget(dropdown, dropdown_area);

        // Render command label
        let label_line = Line::from(vec![
            Span::styled(
                format!("{} ", self.command.label()),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
        ]);
        let label_widget = Paragraph::new(label_line);
        f.render_widget(Clear, label_area);
        f.render_widget(label_widget, label_area);
    }
}
