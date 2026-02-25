use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::ProjectSwitcherItem;
use crate::ui::Component;

const EMPTY_RESULTS_TEXT: &str = "No projects available";
/// Maximum number of project rows rendered at once.
const MAX_VISIBLE_ITEMS: u16 = 8;

/// Overlay renderer for quick project switching.
pub struct ProjectSwitcherOverlay<'a> {
    projects: &'a [ProjectSwitcherItem],
    selected_index: usize,
}

impl<'a> ProjectSwitcherOverlay<'a> {
    /// Creates one project switcher overlay renderer.
    pub fn new(projects: &'a [ProjectSwitcherItem], selected_index: usize) -> Self {
        Self {
            projects,
            selected_index,
        }
    }
}

impl Component for ProjectSwitcherOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = centered_rect(72, 60, area);
        let layout = Layout::default()
            .constraints([
                Constraint::Length(MAX_VISIBLE_ITEMS + 2),
                Constraint::Length(1),
            ])
            .margin(1)
            .split(popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Switch Project");
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let list_text = switcher_list_lines(self.projects, self.selected_index);
        let list_widget = Paragraph::new(list_text).alignment(Alignment::Left);
        f.render_widget(list_widget, layout[0]);

        let footer_widget = Paragraph::new("Enter: switch | Esc: close | j/k: navigate")
            .style(Style::default().fg(Color::Gray));
        f.render_widget(footer_widget, layout[1]);
    }
}

/// Builds switcher list lines using a sliding window and highlights the
/// selected row.
///
/// When the selected item moves beyond `MAX_VISIBLE_ITEMS`, the rendered list
/// shifts down so the selected project remains visible.
fn switcher_list_lines(
    projects: &[ProjectSwitcherItem],
    selected_index: usize,
) -> Vec<Line<'static>> {
    if projects.is_empty() {
        return vec![Line::from(Span::styled(
            EMPTY_RESULTS_TEXT,
            Style::default().fg(Color::DarkGray),
        ))];
    }

    let clamped_selected_index = selected_index.min(projects.len().saturating_sub(1));
    let start_index = switcher_window_start_index(projects.len(), clamped_selected_index);

    projects
        .iter()
        .skip(start_index)
        .take(usize::from(MAX_VISIBLE_ITEMS))
        .enumerate()
        .map(|(index, project)| {
            let absolute_index = start_index + index;
            let marker = if absolute_index == clamped_selected_index {
                ">"
            } else {
                " "
            };
            let favorite = if project.is_favorite { "★ " } else { "" };
            let branch = project.git_branch.as_deref().unwrap_or("-");
            let line_text = format!(
                "{marker} {favorite}{}  [{}]  sessions:{}",
                project.title, branch, project.session_count
            );

            if absolute_index == clamped_selected_index {
                Line::from(Span::styled(
                    line_text,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(line_text, Style::default().fg(Color::White)))
            }
        })
        .collect()
}

/// Returns the first visible item index for a fixed-height sliding list window.
fn switcher_window_start_index(project_count: usize, selected_index: usize) -> usize {
    let max_visible_items = usize::from(MAX_VISIBLE_ITEMS);
    let selected_index = selected_index.min(project_count.saturating_sub(1));

    selected_index
        .saturating_add(1)
        .saturating_sub(max_visible_items)
}

/// Returns a centered popup rectangle using percent-based dimensions.
fn centered_rect(horizontal_percent: u16, vertical_percent: u16, area: Rect) -> Rect {
    let vertical_layout = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - vertical_percent) / 2),
            Constraint::Percentage(vertical_percent),
            Constraint::Percentage((100 - vertical_percent) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - horizontal_percent) / 2),
            Constraint::Percentage(horizontal_percent),
            Constraint::Percentage((100 - horizontal_percent) / 2),
        ])
        .split(vertical_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_switcher_list_lines_show_empty_hint_when_no_projects() {
        // Arrange
        let projects = Vec::new();

        // Act
        let lines = switcher_list_lines(&projects, 0);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), EMPTY_RESULTS_TEXT);
    }

    #[test]
    fn test_switcher_list_lines_mark_selected_row() {
        // Arrange
        let projects = vec![
            ProjectSwitcherItem {
                git_branch: Some("main".to_string()),
                id: 1,
                is_favorite: true,
                last_opened_at: None,
                path: PathBuf::from("/tmp/agentty"),
                session_count: 3,
                title: "agentty".to_string(),
            },
            ProjectSwitcherItem {
                git_branch: Some("develop".to_string()),
                id: 2,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from("/tmp/service"),
                session_count: 1,
                title: "service".to_string(),
            },
        ];

        // Act
        let lines = switcher_list_lines(&projects, 1);

        // Assert
        assert!(lines[0].spans[0].content.starts_with("  ★ agentty"));
        assert!(lines[1].spans[0].content.starts_with("> service"));
    }

    /// Keeps the selected project visible when selection moves past the first
    /// page.
    #[test]
    fn test_switcher_list_lines_keep_selected_row_visible_beyond_max_visible_items() {
        // Arrange
        let projects: Vec<ProjectSwitcherItem> = (0_i64..12)
            .map(|index| ProjectSwitcherItem {
                git_branch: Some("main".to_string()),
                id: index,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from(format!("/tmp/project-{index:02}")),
                session_count: index as u32,
                title: format!("project-{index:02}"),
            })
            .collect();

        // Act
        let lines = switcher_list_lines(&projects, 10);

        // Assert
        assert_eq!(lines.len(), usize::from(MAX_VISIBLE_ITEMS));
        assert!(lines[0].spans[0].content.starts_with("  project-03"));

        let selected_lines: Vec<&Line<'static>> = lines
            .iter()
            .filter(|line| line.spans[0].content.starts_with("> "))
            .collect();
        assert_eq!(selected_lines.len(), 1);
        assert!(selected_lines[0].spans[0].content.contains("project-10"));
    }
}
