use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::app::Tab;
use crate::domain::project::ProjectListItem;
use crate::ui::{Component, style};

/// Header tabs rendered at the top of list mode pages.
pub struct Tabs<'a> {
    active_project_id: i64,
    current_tab: Tab,
    projects: &'a [ProjectListItem],
}

impl Tabs<'_> {
    /// Creates a tabs component with the provided active tab and project
    /// context used to label the project-scoped tab group.
    pub fn new(current_tab: Tab, active_project_id: i64, projects: &[ProjectListItem]) -> Tabs<'_> {
        Tabs {
            active_project_id,
            current_tab,
            projects,
        }
    }
}

impl Component for Tabs<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let line = Line::from(tab_spans(
            self.current_tab,
            self.active_project_id,
            self.projects,
        ));
        let paragraph = Paragraph::new(line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(style::palette::BORDER))
                .padding(Padding::top(1)),
        );
        f.render_widget(paragraph, area);
    }
}

/// Returns styled tab spans with separators and a shared project-scope label.
fn tab_spans(
    current_tab: Tab,
    active_project_id: i64,
    projects: &[ProjectListItem],
) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(9);

    spans.push(tab_span(Tab::Projects, current_tab));
    spans.push(tab_separator_span());
    spans.push(project_context_span(active_project_id, projects));

    for tab in Tab::PROJECT_SCOPED {
        spans.push(tab_separator_span());
        spans.push(tab_span(tab, current_tab));
    }

    spans
}

/// Returns one styled separator span between tabs.
fn tab_separator_span() -> Span<'static> {
    Span::styled("|", Style::default().fg(style::palette::BORDER))
}

/// Returns one styled tab span with active/inactive affordance treatment.
fn tab_span(tab: Tab, current_tab: Tab) -> Span<'static> {
    let label = format!(" {} ", tab_label(tab));

    if tab == current_tab {
        return Span::styled(
            label,
            Style::default()
                .fg(style::palette::WARNING)
                .add_modifier(Modifier::BOLD),
        );
    }

    Span::styled(label, Style::default().fg(style::palette::TEXT_MUTED))
}

/// Returns a styled span describing the active project for project-scoped tabs.
fn project_context_span(active_project_id: i64, projects: &[ProjectListItem]) -> Span<'static> {
    let (project_name, project_scope_style) = active_project_name(active_project_id, projects)
        .map_or_else(
            || {
                (
                    "None".to_string(),
                    Style::default().fg(style::palette::TEXT_SUBTLE),
                )
            },
            |project_name| {
                (
                    project_name,
                    Style::default()
                        .fg(style::palette::ACCENT_SOFT)
                        .add_modifier(Modifier::BOLD),
                )
            },
        );
    let project_scope = format!(" Project: {project_name} ");

    Span::styled(project_scope, project_scope_style)
}

/// Returns the display label for a top-level tab.
fn tab_label(tab: Tab) -> &'static str {
    tab.title()
}

/// Returns the active project name shown before project-scoped tabs.
fn active_project_name(active_project_id: i64, projects: &[ProjectListItem]) -> Option<String> {
    projects
        .iter()
        .find(|project_item| project_item.project.id == active_project_id)
        .map(|project_item| project_item.project.display_label())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::project::Project;

    #[test]
    fn test_tab_spans_use_equal_spacing_between_labels() {
        // Arrange
        let current_tab = Tab::Projects;

        // Act
        let spans = tab_spans(current_tab, 0, &[]);
        let rendered_tabs: String = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        // Assert
        assert_eq!(
            rendered_tabs,
            " Projects | Project: None | Sessions | Stats | Settings "
        );
    }

    #[test]
    fn test_tab_spans_highlight_the_active_tab() {
        // Arrange
        let current_tab = Tab::Stats;

        // Act
        let spans = tab_spans(current_tab, 0, &[]);

        // Assert
        assert_eq!(spans[0].style.fg, Some(style::palette::TEXT_MUTED));
        assert_eq!(spans[2].style.fg, Some(style::palette::TEXT_SUBTLE));
        assert_eq!(spans[4].style.fg, Some(style::palette::TEXT_MUTED));
        assert_eq!(spans[6].style.fg, Some(style::palette::WARNING));
        assert_eq!(spans[6].style.bg, None);
        assert_eq!(spans[8].style.fg, Some(style::palette::TEXT_MUTED));
        assert!(spans[6].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_tab_spans_include_selected_project_name_in_project_scope_label() {
        // Arrange
        let current_tab = Tab::Sessions;
        let projects = vec![
            project_list_item(7, Some("Primary"), "/tmp/primary"),
            project_list_item(8, Some("Secondary"), "/tmp/secondary"),
        ];

        // Act
        let spans = tab_spans(current_tab, 7, &projects);
        let rendered_tabs: String = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        // Assert
        assert_eq!(
            rendered_tabs,
            " Projects | Project: Primary | Sessions | Stats | Settings "
        );
        assert_eq!(spans[2].style.fg, Some(style::palette::ACCENT_SOFT));
        assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[4].style.fg, Some(style::palette::WARNING));
        assert_eq!(spans[4].style.bg, None);
    }

    #[test]
    fn test_tab_spans_render_divider_spans_with_border_color() {
        // Arrange
        let current_tab = Tab::Projects;

        // Act
        let spans = tab_spans(current_tab, 0, &[]);

        // Assert
        assert_eq!(spans[1].content.as_ref(), "|");
        assert_eq!(spans[3].content.as_ref(), "|");
        assert_eq!(spans[5].content.as_ref(), "|");
        assert_eq!(spans[7].content.as_ref(), "|");
        assert_eq!(spans[1].style.fg, Some(style::palette::BORDER));
        assert_eq!(spans[3].style.fg, Some(style::palette::BORDER));
        assert_eq!(spans[5].style.fg, Some(style::palette::BORDER));
        assert_eq!(spans[7].style.fg, Some(style::palette::BORDER));
    }

    #[test]
    fn test_tab_spans_dim_project_scope_when_no_project_is_selected() {
        // Arrange
        let current_tab = Tab::Stats;

        // Act
        let spans = tab_spans(current_tab, 0, &[]);

        // Assert
        assert_eq!(spans[2].content.as_ref(), " Project: None ");
        assert_eq!(spans[2].style.fg, Some(style::palette::TEXT_SUBTLE));
    }

    /// Creates a `ProjectListItem` for tab-label rendering tests.
    fn project_list_item(id: i64, display_name: Option<&str>, path: &str) -> ProjectListItem {
        ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: None,
            project: Project {
                created_at: 0,
                display_name: display_name.map(std::string::ToString::to_string),
                git_branch: None,
                id,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from(path),
                updated_at: 0,
            },
            session_count: 0,
        }
    }
}
