//! GIF proof backend for animated proof output.
//!
//! [`GifBackend`] renders each captured frame using the native renderer
//! and encodes them as an animated GIF with configurable inter-frame
//! delays, suitable for PR comments and documentation.

use std::fs::File;
use std::path::Path;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Frame, RgbaImage};

use super::backend::ProofBackend;
use super::report::{ProofError, ProofReport};
use crate::frame::TerminalFrame;
use crate::renderer;

/// Default delay between frames in milliseconds.
const DEFAULT_FRAME_DELAY_MS: u32 = 1500;

/// Minimum allowed frame delay in milliseconds.
const MIN_FRAME_DELAY_MS: u32 = 200;

/// Maximum allowed frame delay in milliseconds.
const MAX_FRAME_DELAY_MS: u32 = 5000;

/// Renders a proof report as an animated GIF.
///
/// Each captured frame is rendered via the native bitmap font renderer
/// and encoded as a GIF frame with configurable timing.
pub struct GifBackend {
    /// Delay between frames in milliseconds.
    frame_delay_ms: u32,
}

impl GifBackend {
    /// Create a GIF backend with the default frame delay.
    pub fn new() -> Self {
        Self {
            frame_delay_ms: DEFAULT_FRAME_DELAY_MS,
        }
    }

    /// Create a GIF backend with a custom frame delay.
    ///
    /// The delay is clamped to the range
    /// [`MIN_FRAME_DELAY_MS`]–[`MAX_FRAME_DELAY_MS`].
    pub fn with_delay_ms(delay_ms: u32) -> Self {
        Self {
            frame_delay_ms: delay_ms.clamp(MIN_FRAME_DELAY_MS, MAX_FRAME_DELAY_MS),
        }
    }

    /// Return the configured frame delay in milliseconds.
    pub fn frame_delay_ms(&self) -> u32 {
        self.frame_delay_ms
    }
}

impl Default for GifBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ProofBackend for GifBackend {
    /// Render the proof report as an animated GIF.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or encoding fails.
    fn render(&self, report: &ProofReport, output: &Path) -> Result<(), ProofError> {
        if report.captures.is_empty() {
            return Err(ProofError::Format(
                "cannot create GIF from empty report".to_string(),
            ));
        }

        let file = File::create(output)?;
        let mut encoder = GifEncoder::new_with_speed(file, 10);
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|err| ProofError::Format(err.to_string()))?;

        // GIF delay is in units of 10ms.
        let delay_hundredths = self.frame_delay_ms / 10;

        for capture in &report.captures {
            let terminal_frame =
                TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes);
            let image = renderer::render_to_image(&terminal_frame);
            let gif_frame = build_gif_frame(image, delay_hundredths);
            encoder
                .encode_frame(gif_frame)
                .map_err(|err| ProofError::Format(err.to_string()))?;
        }

        Ok(())
    }
}

/// Build a GIF frame from an RGBA image with the specified delay.
fn build_gif_frame(image: RgbaImage, delay_hundredths: u32) -> Frame {
    let delay = image::Delay::from_saturating_duration(std::time::Duration::from_millis(
        u64::from(delay_hundredths) * 10,
    ));

    Frame::from_parts(image, 0, 0, delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_backend_default_delay() {
        // Arrange / Act
        let backend = GifBackend::new();

        // Assert
        assert_eq!(backend.frame_delay_ms(), DEFAULT_FRAME_DELAY_MS);
    }

    #[test]
    fn gif_backend_custom_delay_clamped() {
        // Arrange / Act / Assert — below minimum.
        let too_low = GifBackend::with_delay_ms(50);
        assert_eq!(too_low.frame_delay_ms(), MIN_FRAME_DELAY_MS);

        // Above maximum.
        let too_high = GifBackend::with_delay_ms(10000);
        assert_eq!(too_high.frame_delay_ms(), MAX_FRAME_DELAY_MS);

        // Within range.
        let normal = GifBackend::with_delay_ms(1000);
        assert_eq!(normal.frame_delay_ms(), 1000);
    }

    #[test]
    fn gif_backend_writes_valid_gif() {
        // Arrange
        let frame_a = TerminalFrame::new(20, 3, b"Frame 1");
        let frame_b = TerminalFrame::new(20, 3, b"Frame 2");
        let mut report = ProofReport::new("gif_test");
        report.add_capture("first", "First frame", &frame_a);
        report.add_capture("second", "Second frame", &frame_b);

        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("proof.gif");

        // Act
        let backend = GifBackend::new();
        backend
            .render(&report, &output_path)
            .expect("render should succeed");

        // Assert — file exists and has non-trivial size.
        assert!(output_path.exists());
        let metadata = std::fs::metadata(&output_path).expect("failed to read metadata");
        assert!(metadata.len() > 100, "GIF should have meaningful content");
    }

    #[test]
    fn gif_backend_errors_on_empty_report() {
        // Arrange
        let report = ProofReport::new("empty");
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("empty.gif");

        // Act
        let backend = GifBackend::new();
        let result = backend.render(&report, &output_path);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn gif_backend_single_frame_succeeds() {
        // Arrange
        let frame = TerminalFrame::new(10, 2, b"Single");
        let mut report = ProofReport::new("single");
        report.add_capture("only", "Only frame", &frame);

        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("single.gif");

        // Act
        let backend = GifBackend::with_delay_ms(500);
        let result = backend.render(&report, &output_path);

        // Assert
        assert!(result.is_ok());
        assert!(output_path.exists());
    }
}
