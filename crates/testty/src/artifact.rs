//! Artifact storage for test captures and failure diagnostics.
//!
//! Manages the directory layout for screenshots, semantic frame dumps,
//! overlays, and calibration data produced during test execution. Each
//! capture is stored with its active calibration so matched regions can
//! be mapped back onto the screenshot.

use std::fs;
use std::path::{Path, PathBuf};

use crate::calibration::Calibration;
use crate::frame::TerminalFrame;

/// A capture artifact combining a screenshot path with its calibration
/// and semantic frame data.
#[derive(Debug)]
#[must_use]
pub struct CaptureArtifact {
    /// Path to the screenshot PNG file (if produced by VHS).
    pub screenshot_path: Option<PathBuf>,
    /// The calibration used for this capture.
    pub calibration: Option<Calibration>,
    /// The semantic terminal frame text dump.
    pub frame_text: String,
    /// Number of terminal columns.
    pub cols: u16,
    /// Number of terminal rows.
    pub rows: u16,
}

impl CaptureArtifact {
    /// Create a capture artifact from a terminal frame.
    pub fn from_frame(frame: &TerminalFrame) -> Self {
        Self {
            screenshot_path: None,
            calibration: None,
            frame_text: frame.all_text(),
            cols: frame.cols(),
            rows: frame.rows(),
        }
    }

    /// Attach a screenshot path to this artifact.
    pub fn with_screenshot(mut self, path: PathBuf) -> Self {
        self.screenshot_path = Some(path);

        self
    }

    /// Attach calibration data to this artifact.
    pub fn with_calibration(mut self, calibration: Calibration) -> Self {
        self.calibration = Some(calibration);

        self
    }
}

/// Manages an artifact output directory for one test run.
///
/// Creates and owns a directory where screenshots, overlays, frame dumps,
/// and calibration data are stored during test execution.
pub struct ArtifactDir {
    /// Root path for this test's artifacts.
    root: PathBuf,
}

impl ArtifactDir {
    /// Create a new artifact directory at the given path.
    ///
    /// Creates the directory if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let root = root.into();
        fs::create_dir_all(&root)?;

        Ok(Self { root })
    }

    /// Return the root path of the artifact directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the path where a named screenshot should be stored.
    pub fn screenshot_path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.png"))
    }

    /// Return the path where a named overlay should be stored.
    pub fn overlay_path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}_overlay.png"))
    }

    /// Return the path where a semantic frame dump should be stored.
    pub fn frame_dump_path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}_frame.txt"))
    }

    /// Return the path for a VHS tape file.
    pub fn tape_path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.tape"))
    }

    /// Save a semantic frame dump to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the file fails.
    pub fn save_frame_dump(
        &self,
        name: &str,
        frame: &TerminalFrame,
    ) -> Result<PathBuf, std::io::Error> {
        let path = self.frame_dump_path(name);
        fs::write(&path, frame.all_text())?;

        Ok(path)
    }

    /// Save a capture artifact to disk (frame dump and optional screenshot
    /// copy).
    ///
    /// # Errors
    ///
    /// Returns an error if writing files fails.
    pub fn save_artifact(
        &self,
        name: &str,
        artifact: &CaptureArtifact,
    ) -> Result<(), std::io::Error> {
        let frame_path = self.frame_dump_path(name);
        fs::write(&frame_path, &artifact.frame_text)?;

        if let Some(screenshot) = &artifact.screenshot_path
            && screenshot.exists()
        {
            let dest = self.screenshot_path(name);
            fs::copy(screenshot, &dest)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_dir_creates_directory() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let artifact_path = temp.path().join("artifacts");

        // Act
        let dir = ArtifactDir::new(&artifact_path).expect("failed to create artifact dir");

        // Assert
        assert!(dir.root().exists());
    }

    #[test]
    fn screenshot_path_uses_name() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let dir =
            ArtifactDir::new(temp.path().join("artifacts")).expect("failed to create artifact dir");

        // Act
        let path = dir.screenshot_path("startup");

        // Assert
        assert!(path.ends_with("startup.png"));
    }

    #[test]
    fn save_frame_dump_writes_text() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let dir =
            ArtifactDir::new(temp.path().join("artifacts")).expect("failed to create artifact dir");
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act
        let path = dir
            .save_frame_dump("test", &frame)
            .expect("failed to save frame dump");

        // Assert
        let content = fs::read_to_string(path).expect("failed to read frame dump");
        assert!(content.contains("Hello World"));
    }

    #[test]
    fn capture_artifact_from_frame_captures_text() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Test Content");

        // Act
        let artifact = CaptureArtifact::from_frame(&frame);

        // Assert
        assert!(artifact.frame_text.contains("Test Content"));
        assert_eq!(artifact.cols, 80);
        assert_eq!(artifact.rows, 24);
        assert!(artifact.screenshot_path.is_none());
    }
}
