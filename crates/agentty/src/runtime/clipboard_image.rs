//! Clipboard image capture helpers for prompt-mode pasted attachments.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use arboard::Clipboard;
use image::{ExtendedColorType, ImageFormat};

use crate::app::{self, session};

/// Typed error returned by clipboard image capture and persistence operations.
///
/// Wraps clipboard access, filesystem, image encoding, and validation failures
/// so callers can distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClipboardError {
    /// Clipboard access is not available on this system.
    #[error("Clipboard is unavailable: {reason}")]
    Unavailable {
        /// Human-readable reason from the clipboard backend.
        reason: String,
    },

    /// Clipboard does not contain image data or a recognizable PNG path.
    #[error("Clipboard does not contain an image")]
    NoImage,

    /// A referenced PNG path from clipboard text does not exist on disk.
    #[error("Clipboard PNG path does not exist")]
    PngPathNotFound,

    /// Clipboard image dimensions exceed the `u32` range.
    #[error("Clipboard image {dimension} is too large")]
    DimensionOverflow {
        /// Which dimension overflowed (`"width"` or `"height"`).
        dimension: &'static str,
    },

    /// A parent directory for the clipboard image is missing from the path.
    #[error("Missing clipboard image directory")]
    MissingDirectory,

    /// A filesystem operation during image persistence failed.
    #[error("{context}: {source}")]
    Persist {
        /// Human-readable operation label.
        context: &'static str,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// The image encoding or buffer-save operation failed.
    #[error("Failed to write pasted image PNG: {0}")]
    ImageEncode(image::ImageError),

    /// The persisted image path could not be resolved to an absolute path.
    #[error("Failed to resolve pasted image path: {0}")]
    PathResolve(std::io::Error),

    /// The session identifier is empty.
    #[error("Session id is missing for clipboard image temp storage")]
    EmptySessionId,

    /// The system clock returned a pre-Unix-epoch timestamp.
    #[error("System clock is before the Unix epoch: {0}")]
    SystemClock(std::time::SystemTimeError),

    /// The background image capture task panicked or was cancelled.
    #[error("Clipboard image task failed: {0}")]
    TaskJoin(tokio::task::JoinError),
}

/// Persisted clipboard image metadata used by prompt-mode attachment flows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersistedClipboardImage {
    /// PNG file written under `AGENTTY_ROOT/tmp/<session-id>/images/`.
    pub(crate) local_image_path: PathBuf,
}

/// Reads one clipboard image and persists it as a PNG under the session temp
/// image directory.
///
/// # Errors
/// Returns a [`ClipboardError`] when clipboard access fails, the clipboard
/// does not expose an image payload, or the PNG cannot be written.
pub(crate) async fn persist_clipboard_image(
    session_id: &str,
    attachment_number: usize,
) -> Result<PersistedClipboardImage, ClipboardError> {
    let session_id = session_id.to_string();

    tokio::task::spawn_blocking(move || {
        let image_output_path = build_clipboard_image_path(&session_id, attachment_number)?;
        let mut clipboard = Clipboard::new().map_err(|error| ClipboardError::Unavailable {
            reason: error.to_string(),
        })?;

        if let Ok(image_data) = clipboard.get_image() {
            std::fs::create_dir_all(
                image_output_path
                    .parent()
                    .ok_or(ClipboardError::MissingDirectory)?,
            )
            .map_err(|source| ClipboardError::Persist {
                context: "Failed to create clipboard image directory",
                source,
            })?;

            image::save_buffer_with_format(
                &image_output_path,
                image_data.bytes.as_ref(),
                u32::try_from(image_data.width)
                    .map_err(|_| ClipboardError::DimensionOverflow { dimension: "width" })?,
                u32::try_from(image_data.height).map_err(|_| {
                    ClipboardError::DimensionOverflow {
                        dimension: "height",
                    }
                })?,
                ExtendedColorType::Rgba8,
                ImageFormat::Png,
            )
            .map_err(ClipboardError::ImageEncode)?;

            Ok(PersistedClipboardImage {
                local_image_path: canonicalize_persisted_image_path(&image_output_path)?,
            })
        } else {
            try_copy_png_path_from_clipboard_text(&mut clipboard, &image_output_path)?;

            Ok(PersistedClipboardImage {
                local_image_path: canonicalize_persisted_image_path(&image_output_path)?,
            })
        }
    })
    .await
    .map_err(ClipboardError::TaskJoin)?
}

/// Normalizes one [`ClipboardError`] into short prompt-mode status text.
#[must_use]
pub(crate) fn normalize_clipboard_image_error(error: &ClipboardError) -> String {
    match error {
        ClipboardError::Unavailable { .. } => {
            "Clipboard is unavailable. Try again after granting clipboard access.".to_string()
        }
        ClipboardError::NoImage => "Clipboard does not contain an image.".to_string(),
        ClipboardError::PngPathNotFound => "Clipboard PNG path does not exist.".to_string(),
        ClipboardError::Persist { .. }
        | ClipboardError::ImageEncode(_)
        | ClipboardError::PathResolve(_)
        | ClipboardError::MissingDirectory => {
            "Failed to persist pasted image from the clipboard.".to_string()
        }
        ClipboardError::TaskJoin(_) => "Clipboard image capture failed.".to_string(),
        ClipboardError::DimensionOverflow { .. }
        | ClipboardError::EmptySessionId
        | ClipboardError::SystemClock(_) => error.to_string(),
    }
}

/// Returns the temp directory used for pasted prompt images for one session
/// identifier.
///
/// # Errors
/// Returns [`ClipboardError::EmptySessionId`] when `session_id` is empty.
pub(crate) fn clipboard_image_directory(session_id: &str) -> Result<PathBuf, ClipboardError> {
    let session_id = session_temp_directory_name(session_id)?;
    let agentty_root = app::agentty_home();

    Ok(agentty_root.join("tmp").join(session_id).join("images"))
}

/// Builds a stable unique PNG path for one pasted image capture.
///
/// # Errors
/// Returns an error when the session id cannot be used as a temp directory
/// name.
pub(crate) fn build_clipboard_image_path(
    session_id: &str,
    attachment_number: usize,
) -> Result<PathBuf, ClipboardError> {
    let clock = session::RealClock;

    build_clipboard_image_path_with_clock(session_id, attachment_number, &clock)
}

/// Builds a stable unique PNG path for one pasted image capture using the
/// provided time source.
///
/// # Errors
/// Returns an error when the session id cannot be used as a temp directory
/// name or when the clock returns a pre-Unix-epoch timestamp.
fn build_clipboard_image_path_with_clock(
    session_id: &str,
    attachment_number: usize,
    clock: &dyn session::Clock,
) -> Result<PathBuf, ClipboardError> {
    let timestamp_millis = clock
        .now_system_time()
        .duration_since(UNIX_EPOCH)
        .map_err(ClipboardError::SystemClock)?
        .as_millis();
    let file_name = format!("image-{attachment_number:03}-{timestamp_millis}.png");

    Ok(clipboard_image_directory(session_id)?.join(file_name))
}

/// Returns the directory-name fragment used for one session image temp root.
///
/// # Errors
/// Returns [`ClipboardError::EmptySessionId`] when the session id is empty.
fn session_temp_directory_name(session_id: &str) -> Result<&str, ClipboardError> {
    if session_id.is_empty() {
        return Err(ClipboardError::EmptySessionId);
    }

    Ok(session_id)
}

/// Copies a PNG file path exposed as clipboard text into the target image
/// path.
///
/// # Errors
/// Returns an error when clipboard text is unavailable, is not a PNG path, or
/// the file copy fails.
fn try_copy_png_path_from_clipboard_text(
    clipboard: &mut Clipboard,
    image_output_path: &Path,
) -> Result<(), ClipboardError> {
    let clipboard_text = clipboard.get_text().map_err(|_| ClipboardError::NoImage)?;
    let source_image_path = PathBuf::from(clipboard_text.trim());

    if source_image_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("png")
    {
        return Err(ClipboardError::NoImage);
    }

    if !source_image_path.is_file() {
        return Err(ClipboardError::PngPathNotFound);
    }

    std::fs::create_dir_all(
        image_output_path
            .parent()
            .ok_or(ClipboardError::MissingDirectory)?,
    )
    .map_err(|source| ClipboardError::Persist {
        context: "Failed to create clipboard image directory",
        source,
    })?;
    std::fs::copy(&source_image_path, image_output_path).map_err(|source| {
        ClipboardError::Persist {
            context: "Failed to copy clipboard PNG file",
            source,
        }
    })?;

    Ok(())
}

/// Resolves one persisted image path to the exact absolute filesystem path
/// that downstream transports should reference.
///
/// # Errors
/// Returns [`ClipboardError::PathResolve`] when the persisted file cannot be
/// resolved from disk.
fn canonicalize_persisted_image_path(image_output_path: &Path) -> Result<PathBuf, ClipboardError> {
    std::fs::canonicalize(image_output_path).map_err(ClipboardError::PathResolve)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock {
        system_time: std::time::SystemTime,
    }

    impl session::Clock for FixedClock {
        fn now_instant(&self) -> std::time::Instant {
            std::time::Instant::now()
        }

        fn now_system_time(&self) -> std::time::SystemTime {
            self.system_time
        }
    }

    #[test]
    fn test_clipboard_image_directory_uses_agentty_tmp_path_for_session_id() {
        // Arrange
        let session_id = "session-123";
        let agentty_root = app::agentty_home();

        // Act
        let image_directory =
            clipboard_image_directory(session_id).expect("image directory should resolve");

        // Assert
        assert_eq!(
            image_directory,
            agentty_root.join("tmp").join("session-123").join("images")
        );
    }

    #[test]
    fn test_build_clipboard_image_path_uses_png_extension_in_images_directory() {
        // Arrange
        let session_id = "session-123";
        let expected_directory = app::agentty_home()
            .join("tmp")
            .join("session-123")
            .join("images");
        let clock = FixedClock {
            system_time: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(42),
        };

        // Act
        let image_path = build_clipboard_image_path_with_clock(session_id, 2, &clock)
            .expect("image path should resolve");

        // Assert
        assert_eq!(image_path.parent(), Some(expected_directory.as_path()));
        assert!(
            image_path
                .file_name()
                .is_some_and(|name| { name.to_string_lossy() == "image-002-42.png" })
        );
    }

    #[test]
    fn test_build_clipboard_image_path_rejects_pre_epoch_clock_values() {
        // Arrange
        let session_id = "session-123";
        let clock = FixedClock {
            system_time: std::time::SystemTime::UNIX_EPOCH - std::time::Duration::from_secs(1),
        };

        // Act
        let result = build_clipboard_image_path_with_clock(session_id, 2, &clock);

        // Assert
        assert!(matches!(result, Err(ClipboardError::SystemClock(_))));
    }

    #[test]
    fn test_clipboard_image_directory_rejects_empty_session_id() {
        // Arrange
        let session_id = "";

        // Act
        let result = clipboard_image_directory(session_id);

        // Assert
        assert!(matches!(result, Err(ClipboardError::EmptySessionId)));
    }

    #[test]
    fn test_canonicalize_persisted_image_path_returns_absolute_file_path() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("temp dir should exist");
        let image_path = temp_dir.path().join("image.png");
        std::fs::write(&image_path, b"png").expect("image file should be written");

        // Act
        let canonicalized_path =
            canonicalize_persisted_image_path(&image_path).expect("image path should canonicalize");

        // Assert
        assert_eq!(
            canonicalized_path,
            std::fs::canonicalize(&image_path).expect("std canonicalize should succeed")
        );
    }

    #[test]
    fn test_normalize_clipboard_image_error_maps_unavailable_to_actionable_status() {
        // Arrange
        let error = ClipboardError::Unavailable {
            reason: "permission denied".to_string(),
        };

        // Act
        let normalized_error = normalize_clipboard_image_error(&error);

        // Assert
        assert_eq!(
            normalized_error,
            "Clipboard is unavailable. Try again after granting clipboard access."
        );
    }

    #[test]
    fn test_normalize_clipboard_image_error_maps_no_image_to_short_status() {
        // Arrange
        let error = ClipboardError::NoImage;

        // Act
        let normalized_error = normalize_clipboard_image_error(&error);

        // Assert
        assert_eq!(normalized_error, "Clipboard does not contain an image.");
    }

    #[test]
    fn test_normalize_clipboard_image_error_maps_encode_failure_to_persist_status() {
        // Arrange
        let error = ClipboardError::ImageEncode(image::ImageError::IoError(std::io::Error::other(
            "encoder failed",
        )));

        // Act
        let normalized_error = normalize_clipboard_image_error(&error);

        // Assert
        assert_eq!(
            normalized_error,
            "Failed to persist pasted image from the clipboard."
        );
    }

    #[test]
    fn test_normalize_clipboard_image_error_maps_task_join_to_capture_status() {
        // Arrange / Act
        let error =
            ClipboardError::TaskJoin(tokio::runtime::Runtime::new().expect("runtime").block_on(
                async {
                    tokio::task::spawn_blocking(|| panic!("test"))
                        .await
                        .expect_err("should panic")
                },
            ));
        let normalized_error = normalize_clipboard_image_error(&error);

        // Assert
        assert_eq!(normalized_error, "Clipboard image capture failed.");
    }

    #[test]
    fn test_normalize_clipboard_image_error_passes_through_dimension_overflow() {
        // Arrange
        let error = ClipboardError::DimensionOverflow { dimension: "width" };

        // Act
        let normalized_error = normalize_clipboard_image_error(&error);

        // Assert
        assert_eq!(normalized_error, "Clipboard image width is too large");
    }
}
