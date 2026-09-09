//! Scoped semantic style settings for shared terminal text rendering.

use std::cell::Cell;

use ratatui::style::Color;

thread_local! {
    static ACTIVE_RENDER_SETTINGS: Cell<TextRenderSettings> =
        const { Cell::new(TextRenderSettings::DEFAULT) };
}

/// Semantic color tokens used by markdown, mermaid, and text renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextPalette {
    /// Primary accent color used for headings and prompt markers.
    pub accent: Color,
    /// Informational color used for secondary headings and file lookups.
    pub info: Color,
    /// Success color used for positive emphasis and stats headings.
    pub success: Color,
    /// Subtle dark surface color used behind prompt transcript blocks.
    pub surface: Color,
    /// Subtle blue-gray surface used behind clarification prompt blocks.
    pub surface_clarification: Color,
    /// Elevated surface color used for table headers.
    pub surface_elevated: Color,
    /// Dark surface used behind code blocks.
    pub surface_overlay: Color,
    /// Primary readable text color.
    pub text: Color,
    /// Muted text color used for secondary content and code blocks.
    pub text_muted: Color,
    /// Extra-muted text color used for separators and diagram structure.
    pub text_subtle: Color,
    /// Warning color used for inline code and caution emphasis.
    pub warning: Color,
    /// Softer warning tone used for clarification headings.
    pub warning_soft: Color,
}

impl TextPalette {
    /// Default standalone palette used when callers do not inject settings.
    pub const DEFAULT: Self = Self {
        accent: Color::Cyan,
        info: Color::LightBlue,
        success: Color::Green,
        surface: Color::Rgb(35, 42, 55),
        surface_clarification: Color::Rgb(28, 38, 48),
        surface_elevated: Color::Black,
        surface_overlay: Color::Black,
        text: Color::White,
        text_muted: Color::Gray,
        text_subtle: Color::DarkGray,
        warning: Color::Yellow,
        warning_soft: Color::LightYellow,
    };
}

impl Default for TextPalette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Rendering settings injected by host applications at the text-rendering
/// boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextRenderSettings {
    /// Stable discriminator for non-content style changes that should
    /// invalidate rendered-line caches.
    pub cache_version: u64,
    /// Semantic colors used by the renderer.
    pub palette: TextPalette,
}

impl TextRenderSettings {
    /// Default standalone settings used when callers do not inject settings.
    pub const DEFAULT: Self = Self {
        cache_version: 0,
        palette: TextPalette::DEFAULT,
    };
}

impl Default for TextRenderSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

struct RenderSettingsScope<'settings> {
    cell: &'settings Cell<TextRenderSettings>,
    previous_settings: TextRenderSettings,
}

impl Drop for RenderSettingsScope<'_> {
    fn drop(&mut self) {
        self.cell.set(self.previous_settings);
    }
}

pub(crate) mod palette {
    use ratatui::style::Color;

    use super::active_palette;

    pub(crate) fn accent() -> Color {
        active_palette().accent
    }

    pub(crate) fn info() -> Color {
        active_palette().info
    }

    pub(crate) fn surface() -> Color {
        active_palette().surface
    }

    pub(crate) fn surface_clarification() -> Color {
        active_palette().surface_clarification
    }

    pub(crate) fn surface_elevated() -> Color {
        active_palette().surface_elevated
    }

    pub(crate) fn surface_overlay() -> Color {
        active_palette().surface_overlay
    }

    pub(crate) fn success() -> Color {
        active_palette().success
    }

    pub(crate) fn text() -> Color {
        active_palette().text
    }

    pub(crate) fn text_muted() -> Color {
        active_palette().text_muted
    }

    pub(crate) fn text_subtle() -> Color {
        active_palette().text_subtle
    }

    pub(crate) fn warning() -> Color {
        active_palette().warning
    }

    pub(crate) fn warning_soft() -> Color {
        active_palette().warning_soft
    }
}

pub(crate) fn with_render_settings<Output>(
    settings: TextRenderSettings,
    render: impl FnOnce() -> Output,
) -> Output {
    ACTIVE_RENDER_SETTINGS.with(|cell| {
        let previous_settings = cell.replace(settings);
        let _scope = RenderSettingsScope {
            cell,
            previous_settings,
        };

        render()
    })
}

pub(crate) fn active_theme_cache_version() -> u64 {
    active_settings().cache_version
}

fn active_palette() -> TextPalette {
    active_settings().palette
}

fn active_settings() -> TextRenderSettings {
    ACTIVE_RENDER_SETTINGS.with(Cell::get)
}

#[cfg(test)]
#[path = "style_test.rs"]
mod tests;
