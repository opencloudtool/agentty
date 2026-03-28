use ratatui::style::Color;

use super::icon::Icon;
use crate::domain::session::Status;

/// Shared semantic color tokens for the terminal UI.
pub mod palette {
    use ratatui::style::Color;

    /// Primary accent color used for focused UI elements and titles.
    pub const ACCENT: Color = Color::Cyan;
    /// Brighter accent used for secondary emphasis.
    pub const ACCENT_SOFT: Color = Color::LightCyan;
    /// Subtle border and separator color.
    pub const BORDER: Color = Color::DarkGray;
    /// Error/danger color for destructive or failed states.
    pub const DANGER: Color = Color::Red;
    /// Softer danger tone used for graded severity scales.
    pub const DANGER_SOFT: Color = Color::LightRed;
    /// Informational color used for neutral-highlight states.
    pub const INFO: Color = Color::LightBlue;
    /// Color used for question-status emphasis.
    pub const QUESTION: Color = Color::LightMagenta;
    /// Base surface color for bars and selected rows.
    pub const SURFACE: Color = Color::DarkGray;
    /// Subtle danger-tinted surface used behind removed diff lines.
    pub const SURFACE_DANGER: Color = Color::Rgb(48, 24, 24);
    /// Elevated surface color for table headers.
    pub const SURFACE_ELEVATED: Color = Color::Gray;
    /// Subtle success-tinted surface used behind added diff lines.
    pub const SURFACE_SUCCESS: Color = Color::Rgb(18, 44, 26);
    /// Dark surface used to dim background content behind modal overlays.
    pub const SURFACE_OVERLAY: Color = Color::Black;
    /// Primary readable text color.
    pub const TEXT: Color = Color::White;
    /// Muted text color for secondary copy.
    pub const TEXT_MUTED: Color = Color::Gray;
    /// Extra-muted text color for placeholders and hints.
    pub const TEXT_SUBTLE: Color = Color::DarkGray;
    /// Success color for positive states.
    pub const SUCCESS: Color = Color::Green;
    /// Softer success tone used for graded severity scales.
    pub const SUCCESS_SOFT: Color = Color::LightGreen;
    /// Warning color for in-progress and caution states.
    pub const WARNING: Color = Color::Yellow;
    /// Softer warning tone used for graded severity scales.
    pub const WARNING_SOFT: Color = Color::LightYellow;
}

/// Returns the terminal color used for one session status label.
pub fn status_color(status: Status) -> Color {
    match status {
        Status::New => palette::TEXT_SUBTLE,
        Status::InProgress => palette::WARNING,
        Status::Review => palette::INFO,
        Status::Question => palette::QUESTION,
        Status::Queued => palette::ACCENT_SOFT,
        Status::Rebasing | Status::Merging => palette::ACCENT,
        Status::Done => palette::SUCCESS,
        Status::Canceled => palette::DANGER,
    }
}

/// Returns the icon used for one session status indicator.
pub fn status_icon(status: Status) -> Icon {
    match status {
        Status::New | Status::Review | Status::Question | Status::Queued => Icon::Pending,
        Status::InProgress | Status::Rebasing | Status::Merging => Icon::current_spinner(),
        Status::Done => Icon::Check,
        Status::Canceled => Icon::Cross,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_color_returns_success_for_done() {
        // Arrange / Act
        let color = status_color(Status::Done);

        // Assert
        assert_eq!(color, palette::SUCCESS);
    }

    #[test]
    fn status_color_returns_danger_for_canceled() {
        // Arrange / Act
        let color = status_color(Status::Canceled);

        // Assert
        assert_eq!(color, palette::DANGER);
    }

    #[test]
    fn status_color_returns_warning_for_in_progress() {
        // Arrange / Act
        let color = status_color(Status::InProgress);

        // Assert
        assert_eq!(color, palette::WARNING);
    }

    #[test]
    fn status_color_returns_info_for_review() {
        // Arrange / Act
        let color = status_color(Status::Review);

        // Assert
        assert_eq!(color, palette::INFO);
    }

    #[test]
    fn status_color_returns_accent_for_merging() {
        // Arrange / Act
        let color = status_color(Status::Merging);

        // Assert
        assert_eq!(color, palette::ACCENT);
    }

    #[test]
    fn status_icon_returns_check_for_done() {
        // Arrange / Act
        let icon = status_icon(Status::Done);

        // Assert
        assert!(matches!(icon, Icon::Check));
    }

    #[test]
    fn status_icon_returns_cross_for_canceled() {
        // Arrange / Act
        let icon = status_icon(Status::Canceled);

        // Assert
        assert!(matches!(icon, Icon::Cross));
    }

    #[test]
    fn status_icon_returns_pending_for_new() {
        // Arrange / Act
        let icon = status_icon(Status::New);

        // Assert
        assert!(matches!(icon, Icon::Pending));
    }
}
