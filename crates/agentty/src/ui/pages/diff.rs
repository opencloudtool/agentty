use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::model::Session;
use crate::ui::Page;
use crate::ui::util::{
    DiffLine, DiffLineKind, max_diff_line_number, parse_diff_lines, wrap_diff_content,
};

const BORDER_HORIZONTAL_WIDTH: u16 = 2;
const FOOTER_HEIGHT: u16 = 1;
const GUTTER_EXTRA_WIDTH: usize = 2;
const LINE_NUMBER_COLUMN_COUNT: usize = 2;
const LAYOUT_MARGIN: u16 = 1;
const MIN_GUTTER_WIDTH: usize = 1;
const SCROLL_X_OFFSET: u16 = 0;
const SIGN_COLUMN_WIDTH: usize = 1;
const WRAPPED_CHUNK_START_INDEX: usize = 0;

/// Renders the current session's git diff in a scrollable page.
pub struct DiffPage<'a> {
    pub diff: String,
    pub scroll_offset: u16,
    pub session: &'a Session,
}

impl<'a> DiffPage<'a> {
    /// Creates a diff page for the given session and scroll position.
    pub fn new(session: &'a Session, diff: String, scroll_offset: u16) -> Self {
        Self {
            diff,
            scroll_offset,
            session,
        }
    }

    fn render_file_list(f: &mut Frame, area: Rect, parsed: &[DiffLine]) {
        let mut file_list_lines = Vec::new();
        for line in parsed {
            if line.kind == DiffLineKind::FileHeader && line.content.starts_with("diff --git") {
                let text = if let Some(stripped) = line.content.strip_prefix("diff --git a/") {
                    if let Some((old, new)) = stripped.split_once(" b/") {
                        if old == new {
                            old.to_string()
                        } else {
                            format!("{old} -> {new}")
                        }
                    } else {
                        stripped.to_string()
                    }
                } else {
                    line.content.replace("diff --git ", "")
                };
                file_list_lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::Cyan),
                )));
            }
        }

        if file_list_lines.is_empty() {
            file_list_lines.push(Line::from(Span::styled(
                "No files",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let file_list_paragraph = Paragraph::new(file_list_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Files ", Style::default().fg(Color::Cyan))),
        );
        f.render_widget(file_list_paragraph, area);
    }

    fn render_diff_content(&self, f: &mut Frame, area: Rect, parsed: &[DiffLine]) {
        let title = format!(" Diff — {} ", self.session.display_title());

        let max_num = max_diff_line_number(parsed);
        let gutter_width = if max_num == 0 {
            MIN_GUTTER_WIDTH
        } else {
            max_num.ilog10() as usize + MIN_GUTTER_WIDTH
        };

        // gutter: "old│new " = gutter_width * 2 + 2 (separator + trailing space)
        // sign column: 1 char
        let prefix_width =
            gutter_width * LINE_NUMBER_COLUMN_COUNT + GUTTER_EXTRA_WIDTH + SIGN_COLUMN_WIDTH;
        let inner_width = area.width.saturating_sub(BORDER_HORIZONTAL_WIDTH) as usize;

        let gutter_style = Style::default().fg(Color::DarkGray);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(parsed.len());

        for diff_line in parsed {
            let (sign, content_style) = match diff_line.kind {
                DiffLineKind::FileHeader => {
                    if diff_line.content.starts_with("diff ") && !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(Color::Yellow),
                    )));

                    continue;
                }
                DiffLineKind::HunkHeader => {
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(Color::Cyan),
                    )));

                    continue;
                }
                DiffLineKind::Addition => ("+", Style::default().fg(Color::Green)),
                DiffLineKind::Deletion => ("-", Style::default().fg(Color::Red)),
                DiffLineKind::Context => (" ", Style::default().fg(Color::Gray)),
            };

            let old_str = match diff_line.old_line {
                Some(num) => format!("{num:>gutter_width$}"),
                None => " ".repeat(gutter_width),
            };
            let new_str = match diff_line.new_line {
                Some(num) => format!("{num:>gutter_width$}"),
                None => " ".repeat(gutter_width),
            };

            let gutter_text = format!("{old_str}│{new_str} ");
            let content_available = inner_width.saturating_sub(prefix_width);
            let chunks = wrap_diff_content(diff_line.content, content_available);

            for (idx, chunk) in chunks.iter().enumerate() {
                if idx == WRAPPED_CHUNK_START_INDEX {
                    lines.push(Line::from(vec![
                        Span::styled(gutter_text.clone(), gutter_style),
                        Span::styled(sign, content_style),
                        Span::styled(*chunk, content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(" ".repeat(prefix_width), gutter_style),
                        Span::styled(*chunk, content_style),
                    ]));
                }
            }
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(title, Style::default().fg(Color::Yellow))),
            )
            .scroll((self.scroll_offset, SCROLL_X_OFFSET));

        f.render_widget(paragraph, area);
    }
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(FOOTER_HEIGHT)])
            .margin(LAYOUT_MARGIN)
            .split(area);

        let content_area = chunks[0];
        let footer_area = chunks[1];

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(content_area);

        let file_list_area = content_layout[0];
        let diff_area = content_layout[1];

        let parsed = parse_diff_lines(&self.diff);

        Self::render_file_list(f, file_list_area, &parsed);
        self.render_diff_content(f, diff_area, &parsed);

        let help_message = Paragraph::new("q: back | j/k: scroll | ?: help")
            .style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}
