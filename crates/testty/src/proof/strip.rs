//! Screenshot strip proof backend.
//!
//! [`ScreenshotStripBackend`] renders each captured frame using the native
//! renderer and composes them vertically into a single tall PNG image with
//! step labels between each frame.

use std::path::Path;

use image::{Rgba, RgbaImage};

use super::backend::ProofBackend;
use super::report::{ProofCapture, ProofError, ProofReport};
use crate::frame::TerminalFrame;
use crate::renderer;

/// Height of the label header between frames in pixels.
const LABEL_HEIGHT: u32 = 32;

/// Background color for label headers.
const LABEL_BG: Rgba<u8> = Rgba([50, 50, 50, 255]);

/// Text color for label headers.
const LABEL_FG: Rgba<u8> = Rgba([220, 220, 220, 255]);

/// Padding between frames in pixels.
const FRAME_PADDING: u32 = 4;

/// Renders a proof report as a vertical PNG strip of screenshots.
///
/// Each capture is rendered using the native bitmap font renderer,
/// with step labels drawn between frames for context.
pub struct ScreenshotStripBackend;

impl ProofBackend for ScreenshotStripBackend {
    /// Render the proof report as a vertical strip PNG.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError::Io`] if writing the image file fails.
    fn render(&self, report: &ProofReport, output: &Path) -> Result<(), ProofError> {
        let rendered_frames = render_all_frames(report);
        let strip = compose_strip(&rendered_frames, report);
        strip
            .save(output)
            .map_err(|err| ProofError::Format(err.to_string()))?;

        Ok(())
    }
}

/// Render all captured frames to images using the native renderer.
fn render_all_frames(report: &ProofReport) -> Vec<(RgbaImage, &ProofCapture)> {
    report
        .captures
        .iter()
        .map(|capture| {
            let frame = TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes);
            let image = renderer::render_to_image(&frame);

            (image, capture)
        })
        .collect()
}

/// Compose rendered frames into a single vertical strip with labels.
fn compose_strip(frames: &[(RgbaImage, &ProofCapture)], report: &ProofReport) -> RgbaImage {
    if frames.is_empty() {
        return RgbaImage::new(1, 1);
    }

    // Calculate total dimensions.
    let max_width = frames
        .iter()
        .map(|(image, _)| image.width())
        .max()
        .unwrap_or(1);

    let total_height: u32 = frames
        .iter()
        .enumerate()
        .map(|(index, (image, _))| {
            let label_h = LABEL_HEIGHT;
            let padding = if index + 1 < frames.len() {
                FRAME_PADDING
            } else {
                0
            };

            label_h + image.height() + padding
        })
        .sum();

    let mut strip = RgbaImage::from_pixel(max_width, total_height, Rgba([30, 30, 30, 255]));
    let mut current_y = 0u32;

    for (index, (frame_image, capture)) in frames.iter().enumerate() {
        // Draw label header.
        draw_label_header(
            &mut strip,
            current_y,
            max_width,
            index + 1,
            &capture.label,
            &capture.description,
        );
        current_y += LABEL_HEIGHT;

        // Copy frame image.
        copy_image_onto(&mut strip, frame_image, 0, current_y);
        current_y += frame_image.height();

        // Add padding between frames.
        if index + 1 < report.captures.len() {
            current_y += FRAME_PADDING;
        }
    }

    strip
}

/// Draw a label header with step number, label, and description.
fn draw_label_header(
    image: &mut RgbaImage,
    start_y: u32,
    width: u32,
    step_number: usize,
    label: &str,
    description: &str,
) {
    // Fill background.
    for pixel_y in start_y..start_y + LABEL_HEIGHT {
        for pixel_x in 0..width {
            if pixel_x < image.width() && pixel_y < image.height() {
                image.put_pixel(pixel_x, pixel_y, LABEL_BG);
            }
        }
    }

    // Render label text using the bitmap font.
    let text = format!("Step {step_number}: [{label}] {description}");
    draw_text_line(image, 4, start_y + 8, &text, LABEL_FG);
}

/// Draw a line of text using the bitmap font from the renderer module.
///
/// Each character is 8 pixels wide and rendered from the embedded font.
fn draw_text_line(image: &mut RgbaImage, start_x: u32, start_y: u32, text: &str, color: Rgba<u8>) {
    let mut cursor_x = start_x;

    for character in text.chars() {
        if cursor_x + 8 > image.width() {
            break;
        }

        draw_bitmap_char(image, cursor_x, start_y, character, color);
        cursor_x += 8;
    }
}

/// Draw a single character from the embedded ASCII bitmap font.
fn draw_bitmap_char(
    image: &mut RgbaImage,
    pixel_x: u32,
    pixel_y: u32,
    character: char,
    color: Rgba<u8>,
) {
    let code = character as u32;

    // Only render ASCII printable characters.
    if !(0x20..=0x7E).contains(&code) {
        return;
    }

    let glyph_index = (code - 0x20) as usize;
    let glyph = &renderer::ASCII_GLYPHS[glyph_index];

    for (glyph_row, &byte) in glyph.iter().enumerate() {
        let row_offset = u32::try_from(glyph_row).unwrap_or(0);
        for bit in 0..8u32 {
            if byte & (0x80 >> bit) != 0 {
                let target_x = pixel_x + bit;
                let target_y = pixel_y + row_offset;
                if target_x < image.width() && target_y < image.height() {
                    image.put_pixel(target_x, target_y, color);
                }
            }
        }
    }
}

/// Copy a source image onto a destination at the given offset.
fn copy_image_onto(destination: &mut RgbaImage, source: &RgbaImage, offset_x: u32, offset_y: u32) {
    for source_y in 0..source.height() {
        for source_x in 0..source.width() {
            let target_x = offset_x + source_x;
            let target_y = offset_y + source_y;
            if target_x < destination.width() && target_y < destination.height() {
                destination.put_pixel(target_x, target_y, *source.get_pixel(source_x, source_y));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_of_two_captures_is_taller_than_single_frame() {
        // Arrange
        let frame_a = TerminalFrame::new(40, 5, b"First");
        let frame_b = TerminalFrame::new(40, 5, b"Second");
        let mut report = ProofReport::new("strip_test");
        report.add_capture("first", "First capture", &frame_a);
        report.add_capture("second", "Second capture", &frame_b);

        // Act
        let rendered_frames = render_all_frames(&report);
        let strip = compose_strip(&rendered_frames, &report);

        // Assert — strip should be taller than a single frame (5*16 = 80px).
        let single_frame_height = 5 * 16;
        assert!(strip.height() > single_frame_height);
    }

    #[test]
    fn strip_backend_writes_valid_png() {
        // Arrange
        let frame = TerminalFrame::new(20, 3, b"Hello strip");
        let mut report = ProofReport::new("png_test");
        report.add_capture("snap", "Snapshot", &frame);

        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("strip.png");

        // Act
        let backend = ScreenshotStripBackend;
        backend
            .render(&report, &output_path)
            .expect("render should succeed");

        // Assert — file exists and is a valid PNG.
        assert!(output_path.exists());
        let loaded = image::open(&output_path).expect("should open as image");
        assert!(loaded.width() > 0);
        assert!(loaded.height() > 0);
    }

    #[test]
    fn empty_report_produces_minimal_strip() {
        // Arrange
        let report = ProofReport::new("empty");

        // Act
        let rendered_frames = render_all_frames(&report);
        let strip = compose_strip(&rendered_frames, &report);

        // Assert — minimal 1x1 image for empty reports.
        assert_eq!(strip.width(), 1);
        assert_eq!(strip.height(), 1);
    }

    #[test]
    fn strip_width_matches_widest_frame() {
        // Arrange — two frames with different widths.
        let narrow = TerminalFrame::new(20, 3, b"Narrow");
        let wide = TerminalFrame::new(40, 3, b"Wide");
        let mut report = ProofReport::new("width_test");
        report.add_capture("narrow", "Narrow frame", &narrow);
        report.add_capture("wide", "Wide frame", &wide);

        // Act
        let rendered_frames = render_all_frames(&report);
        let strip = compose_strip(&rendered_frames, &report);

        // Assert — strip width should be 40 * 8 = 320 (widest frame).
        assert_eq!(strip.width(), 320);
    }
}
