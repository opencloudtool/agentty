//! VHS-based E2E test harness for agentty.
//!
//! Launches the real `agentty` binary inside a VHS virtual terminal,
//! captures PNG screenshots, and compares them against stored references
//! using pixel-level tolerance.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use image::GenericImageView;
use tempfile::TempDir;

/// Maximum number of VHS retry attempts when the screenshot is not produced.
const MAX_VHS_RETRIES: u8 = 3;

/// Default pixel color-distance threshold. Two pixels whose Euclidean
/// RGB distance exceeds this value count as "different".
const DEFAULT_PIXEL_THRESHOLD: f64 = 30.0;

/// Default percentage of pixels allowed to differ before the comparison
/// fails. Accommodates timestamps, cursor blink, and minor rendering
/// variance while catching layout regressions.
const DEFAULT_DIFF_PERCENT_THRESHOLD: f64 = 10.0;

/// Directory name inside the temp root used as the working directory.
/// Deterministic so the project name rendered in the UI is stable.
const TEST_PROJECT_DIR: &str = "test-project";

/// VHS terminal width in pixels.
const VHS_WIDTH: u16 = 1200;

/// VHS terminal height in pixels.
const VHS_HEIGHT: u16 = 600;

/// VHS font size.
const VHS_FONT_SIZE: u16 = 14;

/// Seconds to wait for agentty to render before taking the screenshot.
const RENDER_WAIT_SECS: u8 = 3;

/// VHS-based test that launches agentty in a virtual terminal.
///
/// Creates an isolated environment with a fresh database and a
/// deterministic working directory, generates a VHS tape, runs it,
/// and captures a PNG screenshot for comparison.
pub(crate) struct VhsTest {
    /// Path to the compiled `agentty` binary.
    binary_path: PathBuf,
    /// Temporary root directory owning all test artifacts.
    temp_root: TempDir,
}

impl VhsTest {
    /// Create a new VHS test with a clean, isolated environment.
    ///
    /// Returns an error if VHS is not installed.
    pub(crate) fn new() -> Result<Self, Box<dyn std::error::Error>> {
        check_vhs_installed()?;

        let binary_path = PathBuf::from(env!("CARGO_BIN_EXE_agentty"));
        let temp_root = TempDir::new()?;

        // Create deterministic working directory.
        let workdir = temp_root.path().join(TEST_PROJECT_DIR);
        fs::create_dir_all(&workdir)?;

        Ok(Self {
            binary_path,
            temp_root,
        })
    }

    /// Run the VHS tape and return the path to the captured screenshot.
    ///
    /// The screenshot PNG lives inside the temp root and remains valid
    /// as long as `self` is alive.
    pub(crate) fn run_and_screenshot(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let screenshot_path = self.temp_root.path().join("screenshot.png");
        let tape_content = self.generate_tape(&screenshot_path);
        let tape_path = self.temp_root.path().join("test.tape");
        fs::write(&tape_path, &tape_content)?;

        let mut last_error = String::new();

        for attempt in 1..=MAX_VHS_RETRIES {
            // Remove stale screenshot from previous attempt.
            let _ = fs::remove_file(&screenshot_path);

            let output = Command::new("vhs").arg(&tape_path).output()?;

            let stdout_text = String::from_utf8_lossy(&output.stdout);
            let stderr_text = String::from_utf8_lossy(&output.stderr);

            if !output.status.success() {
                return Err(format!(
                    "VHS execution failed:\nstdout: {stdout_text}\nstderr: {stderr_text}"
                )
                .into());
            }

            if screenshot_path.exists() {
                return Ok(screenshot_path);
            }

            last_error = format!(
                "Attempt {attempt}/{MAX_VHS_RETRIES}: VHS did not produce screenshot.\nVHS \
                 stdout: {stdout_text}\nVHS stderr: {stderr_text}"
            );
        }

        Err(format!(
            "VHS failed to produce screenshot after {MAX_VHS_RETRIES} attempts.\n{last_error}"
        )
        .into())
    }

    /// Generate the VHS tape content with all paths substituted.
    fn generate_tape(&self, screenshot_path: &Path) -> String {
        let agentty_root = self.temp_root.path().join("agentty_root");
        let workdir = self.temp_root.path().join(TEST_PROJECT_DIR);

        let gif_path = self.temp_root.path().join("recording.gif");

        format!(
            r#"Set Shell "bash"
Set FontSize {font_size}
Set Width {width}
Set Height {height}
Set Padding 0
Set TypingSpeed 0

Output "{gif}"

Hide
Type "export AGENTTY_ROOT='{agentty_root}'"
Enter
Sleep 200ms
Type "cd '{workdir}'"
Enter
Sleep 200ms
Show

Type "{binary}"
Enter
Sleep {wait}s

Sleep 500ms
Screenshot "{screenshot}"

Hide
Type "q"
Sleep 1s
"#,
            font_size = VHS_FONT_SIZE,
            width = VHS_WIDTH,
            height = VHS_HEIGHT,
            gif = gif_path.display(),
            agentty_root = agentty_root.display(),
            workdir = workdir.display(),
            binary = self.binary_path.display(),
            wait = RENDER_WAIT_SECS,
            screenshot = screenshot_path.display(),
        )
    }
}

/// Verify that VHS is installed and available on `PATH`.
fn check_vhs_installed() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("vhs")
        .arg("--version")
        .output()
        .map_err(|_| "VHS is not installed. Install with: brew install vhs")?;

    if !output.status.success() {
        return Err("VHS is installed but returned an error".into());
    }

    Ok(())
}

/// Compare a screenshot against its stored reference image.
///
/// On first run (or when `AGENTTY_E2E_UPDATE=1` is set), saves the
/// screenshot as the new reference. On subsequent runs, loads both
/// images and compares them pixel-by-pixel with tolerance.
///
/// # Errors
///
/// Returns an error if the images differ beyond the tolerance threshold,
/// saving the actual screenshot alongside the reference for review.
pub(crate) fn assert_screenshot_matches(
    actual_path: &Path,
    reference_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let reference_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("e2e_screenshots");
    let reference_path = reference_dir.join(format!("{reference_name}.png"));

    let update_mode = env::var("AGENTTY_E2E_UPDATE").is_ok();

    if update_mode || !reference_path.exists() {
        fs::create_dir_all(&reference_dir)?;
        fs::copy(actual_path, &reference_path)?;

        return Ok(());
    }

    let diff_percent = compare_screenshots(actual_path, &reference_path)?;

    if diff_percent > DEFAULT_DIFF_PERCENT_THRESHOLD {
        let actual_save = reference_dir.join(format!("{reference_name}_actual.png"));
        fs::copy(actual_path, &actual_save)?;

        return Err(format!(
            "Screenshot mismatch: {diff_percent:.1}% pixels differ (threshold: \
             {DEFAULT_DIFF_PERCENT_THRESHOLD}%).\nReference: {}\nActual:    {}\nRun with \
             AGENTTY_E2E_UPDATE=1 to update the reference.",
            reference_path.display(),
            actual_save.display(),
        )
        .into());
    }

    Ok(())
}

/// Compare two PNG screenshots and return the percentage of different pixels.
///
/// Two pixels are considered different when their Euclidean RGB distance
/// exceeds [`DEFAULT_PIXEL_THRESHOLD`].
fn compare_screenshots(
    actual_path: &Path,
    reference_path: &Path,
) -> Result<f64, Box<dyn std::error::Error>> {
    let actual_img = image::open(actual_path)?;
    let reference_img = image::open(reference_path)?;

    let (actual_width, actual_height) = actual_img.dimensions();
    let (ref_width, ref_height) = reference_img.dimensions();

    if actual_width != ref_width || actual_height != ref_height {
        return Err(format!(
            "Screenshot dimensions differ: actual {actual_width}x{actual_height} vs reference \
             {ref_width}x{ref_height}"
        )
        .into());
    }

    let total_pixels = f64::from(actual_width) * f64::from(actual_height);
    let mut different_pixels: f64 = 0.0;

    for pixel_y in 0..actual_height {
        for pixel_x in 0..actual_width {
            let actual_pixel = actual_img.get_pixel(pixel_x, pixel_y);
            let reference_pixel = reference_img.get_pixel(pixel_x, pixel_y);

            let distance = pixel_distance(actual_pixel.0, reference_pixel.0);
            if distance > DEFAULT_PIXEL_THRESHOLD {
                different_pixels += 1.0;
            }
        }
    }

    let percent = (different_pixels / total_pixels) * 100.0;

    Ok(percent)
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
    fn pixel_distance_different_channels() {
        // Arrange
        let pixel_a = [0, 0, 0, 255];
        let pixel_b = [255, 255, 255, 255];

        // Act
        let distance = pixel_distance(pixel_a, pixel_b);

        // Assert — sqrt(255^2 + 255^2 + 255^2) ≈ 441.67
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
}
