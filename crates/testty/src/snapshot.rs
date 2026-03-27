//! Paired snapshot workflow for baseline management and failure artifacts.
//!
//! Manages committed baseline snapshots (visual PNG plus semantic frame
//! sidecar) and produces structured diffs on failure. Supports an
//! environment-driven update mode so baselines are only rewritten when
//! the author explicitly requests it.

use std::path::{Path, PathBuf};
use std::{env, fs};

use image::GenericImageView;

/// Environment variable name that enables baseline update mode.
const UPDATE_ENV_VAR: &str = "TUI_TEST_UPDATE";

/// Default pixel color-distance threshold for image comparison.
const DEFAULT_PIXEL_THRESHOLD: f64 = 30.0;

/// Default percentage of pixels allowed to differ.
const DEFAULT_DIFF_PERCENT_THRESHOLD: f64 = 10.0;

/// Configuration for snapshot comparison behavior.
#[derive(Debug, Clone)]
#[must_use]
pub struct SnapshotConfig {
    /// Directory where reference baselines are stored.
    pub baseline_dir: PathBuf,
    /// Directory where failure artifacts are written.
    pub artifact_dir: PathBuf,
    /// Per-pixel color distance threshold.
    pub pixel_threshold: f64,
    /// Maximum percentage of differing pixels allowed.
    pub diff_percent_threshold: f64,
}

impl SnapshotConfig {
    /// Create a new snapshot config with the given baseline and artifact
    /// directories, using default tolerance thresholds.
    pub fn new(baseline_dir: impl Into<PathBuf>, artifact_dir: impl Into<PathBuf>) -> Self {
        Self {
            baseline_dir: baseline_dir.into(),
            artifact_dir: artifact_dir.into(),
            pixel_threshold: DEFAULT_PIXEL_THRESHOLD,
            diff_percent_threshold: DEFAULT_DIFF_PERCENT_THRESHOLD,
        }
    }

    /// Set custom tolerance thresholds.
    pub fn with_thresholds(mut self, pixel_threshold: f64, diff_percent_threshold: f64) -> Self {
        self.pixel_threshold = pixel_threshold;
        self.diff_percent_threshold = diff_percent_threshold;

        self
    }
}

/// Check whether update mode is active (via environment variable).
pub fn is_update_mode() -> bool {
    env::var(UPDATE_ENV_VAR).is_ok()
}

/// Compare an actual screenshot against its stored baseline.
///
/// In update mode, saves the actual as the new baseline. When not in
/// update mode a committed baseline must already exist — a missing
/// baseline is treated as an error so CI never silently passes without
/// a reference.
///
/// # Errors
///
/// Returns an error if the baseline is missing (outside update mode),
/// the images differ beyond tolerance, or if I/O fails.
pub fn assert_snapshot_matches(
    config: &SnapshotConfig,
    name: &str,
    actual_screenshot: &Path,
) -> Result<(), SnapshotError> {
    fs::create_dir_all(&config.baseline_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;
    fs::create_dir_all(&config.artifact_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;

    let baseline_path = config.baseline_dir.join(format!("{name}.png"));

    if is_update_mode() {
        fs::copy(actual_screenshot, &baseline_path)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Ok(());
    }

    if !baseline_path.exists() {
        return Err(SnapshotError::MissingBaseline {
            name: name.to_string(),
            baseline_path,
        });
    }

    let diff_percent =
        compare_screenshots(actual_screenshot, &baseline_path, config.pixel_threshold)?;

    if diff_percent > config.diff_percent_threshold {
        // Save the actual screenshot as a failure artifact.
        let actual_artifact = config.artifact_dir.join(format!("{name}_actual.png"));
        fs::copy(actual_screenshot, &actual_artifact)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Err(SnapshotError::Mismatch {
            name: name.to_string(),
            diff_percent,
            threshold: config.diff_percent_threshold,
            baseline_path,
            actual_path: actual_artifact,
        });
    }

    Ok(())
}

/// Compare a frame text dump against its stored semantic baseline.
///
/// In update mode, saves the actual dump as the new baseline. When not
/// in update mode a committed baseline must already exist — a missing
/// baseline is treated as an error so CI never silently passes without
/// a reference.
///
/// # Errors
///
/// Returns an error if the baseline is missing (outside update mode),
/// the text differs, or if I/O fails.
pub fn assert_frame_snapshot_matches(
    config: &SnapshotConfig,
    name: &str,
    actual_text: &str,
) -> Result<(), SnapshotError> {
    fs::create_dir_all(&config.baseline_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;
    fs::create_dir_all(&config.artifact_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;

    let baseline_path = config.baseline_dir.join(format!("{name}_frame.txt"));

    if is_update_mode() {
        fs::write(&baseline_path, actual_text)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Ok(());
    }

    if !baseline_path.exists() {
        return Err(SnapshotError::MissingBaseline {
            name: name.to_string(),
            baseline_path,
        });
    }

    let expected_text = fs::read_to_string(&baseline_path)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;

    if actual_text != expected_text {
        let actual_artifact = config.artifact_dir.join(format!("{name}_frame_actual.txt"));
        fs::write(&actual_artifact, actual_text)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Err(SnapshotError::FrameMismatch {
            name: name.to_string(),
            expected: expected_text,
            actual: actual_text.to_string(),
        });
    }

    Ok(())
}

/// Errors from snapshot comparison operations.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// Screenshot pixel comparison exceeded threshold.
    #[error(
        "Snapshot '{name}' mismatch: {diff_percent:.1}% pixels differ \
         (threshold: {threshold}%).\nBaseline: {}\nActual: {}",
        baseline_path.display(),
        actual_path.display()
    )]
    Mismatch {
        /// Name of the snapshot.
        name: String,
        /// Percentage of differing pixels.
        diff_percent: f64,
        /// Configured threshold.
        threshold: f64,
        /// Path to the baseline file.
        baseline_path: PathBuf,
        /// Path to the saved actual file.
        actual_path: PathBuf,
    },

    /// No committed baseline file found (outside update mode).
    #[error(
        "Missing baseline for '{name}'. Run with {UPDATE_ENV_VAR}=1 to create it.\n\
         Expected: {}",
        baseline_path.display()
    )]
    MissingBaseline {
        /// Name of the snapshot.
        name: String,
        /// Expected baseline file path.
        baseline_path: PathBuf,
    },

    /// Semantic frame text did not match baseline.
    #[error("Frame snapshot '{name}' mismatch")]
    FrameMismatch {
        /// Name of the snapshot.
        name: String,
        /// Expected frame text.
        expected: String,
        /// Actual frame text.
        actual: String,
    },

    /// I/O error reading or writing files.
    #[error("I/O error: {0}")]
    IoError(String),

    /// Image format or loading error.
    #[error("Image error: {0}")]
    ImageError(String),
}

/// Compare two PNG screenshots and return the percentage of different pixels.
fn compare_screenshots(
    actual_path: &Path,
    reference_path: &Path,
    pixel_threshold: f64,
) -> Result<f64, SnapshotError> {
    let actual_img =
        image::open(actual_path).map_err(|err| SnapshotError::ImageError(err.to_string()))?;
    let reference_img =
        image::open(reference_path).map_err(|err| SnapshotError::ImageError(err.to_string()))?;

    let (actual_width, actual_height) = actual_img.dimensions();
    let (ref_width, ref_height) = reference_img.dimensions();

    if actual_width != ref_width || actual_height != ref_height {
        return Err(SnapshotError::Mismatch {
            name: "size".to_string(),
            diff_percent: 100.0,
            threshold: 0.0,
            baseline_path: reference_path.to_path_buf(),
            actual_path: actual_path.to_path_buf(),
        });
    }

    let total_pixels = f64::from(actual_width) * f64::from(actual_height);
    let mut different_pixels: f64 = 0.0;

    for pixel_y in 0..actual_height {
        for pixel_x in 0..actual_width {
            let actual_pixel = actual_img.get_pixel(pixel_x, pixel_y);
            let reference_pixel = reference_img.get_pixel(pixel_x, pixel_y);

            let distance = pixel_distance(actual_pixel.0, reference_pixel.0);
            if distance > pixel_threshold {
                different_pixels += 1.0;
            }
        }
    }

    Ok((different_pixels / total_pixels) * 100.0)
}

/// Compute the Euclidean distance between two RGBA pixel values (RGB only).
fn pixel_distance(pixel_a: [u8; 4], pixel_b: [u8; 4]) -> f64 {
    let red_diff = f64::from(pixel_a[0]) - f64::from(pixel_b[0]);
    let green_diff = f64::from(pixel_a[1]) - f64::from(pixel_b[1]);
    let blue_diff = f64::from(pixel_a[2]) - f64::from(pixel_b[2]);

    (red_diff * red_diff + green_diff * green_diff + blue_diff * blue_diff).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_distance_identical_is_zero() {
        // Arrange
        let pixel = [100, 150, 200, 255];

        // Act
        let distance = pixel_distance(pixel, pixel);

        // Assert
        assert!(distance.abs() < f64::EPSILON);
    }

    #[test]
    fn pixel_distance_opposite_colors() {
        // Arrange
        let pixel_a = [0, 0, 0, 255];
        let pixel_b = [255, 255, 255, 255];

        // Act
        let distance = pixel_distance(pixel_a, pixel_b);

        // Assert — sqrt(255^2 * 3) ≈ 441.67
        assert!(distance > 441.0);
        assert!(distance < 442.0);
    }

    #[test]
    fn pixel_distance_ignores_alpha() {
        // Arrange
        let pixel_a = [100, 100, 100, 0];
        let pixel_b = [100, 100, 100, 255];

        // Act
        let distance = pixel_distance(pixel_a, pixel_b);

        // Assert
        assert!(distance.abs() < f64::EPSILON);
    }

    #[test]
    fn frame_snapshot_returns_missing_baseline_error_outside_update_mode() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));

        // Act
        let result = assert_frame_snapshot_matches(&config, "test", "Hello World");

        // Assert
        assert!(
            matches!(result, Err(SnapshotError::MissingBaseline { .. })),
            "expected MissingBaseline error, got {result:?}"
        );
    }

    #[test]
    fn frame_snapshot_matches_identical_content() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));
        let baseline_path = config.baseline_dir.join("test_frame.txt");
        fs::create_dir_all(&config.baseline_dir).expect("failed to create baseline dir");
        fs::write(&baseline_path, "Hello World").expect("failed to write baseline");

        // Act
        let result = assert_frame_snapshot_matches(&config, "test", "Hello World");

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn frame_snapshot_detects_mismatch() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));
        let baseline_path = config.baseline_dir.join("test_frame.txt");
        fs::create_dir_all(&config.baseline_dir).expect("failed to create baseline dir");
        fs::write(&baseline_path, "Hello World").expect("failed to write baseline");

        // Act
        let result = assert_frame_snapshot_matches(&config, "test", "Goodbye World");

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn snapshot_config_with_custom_thresholds() {
        // Arrange / Act
        let config = SnapshotConfig::new("/baselines", "/artifacts").with_thresholds(50.0, 20.0);

        // Assert
        assert!((config.pixel_threshold - 50.0).abs() < f64::EPSILON);
        assert!((config.diff_percent_threshold - 20.0).abs() < f64::EPSILON);
    }
}
