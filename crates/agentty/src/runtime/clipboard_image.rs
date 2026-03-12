//! Clipboard image capture helpers for prompt-mode pasted attachments.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arboard::Clipboard;
use image::{ExtendedColorType, ImageFormat};

use crate::app;

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
/// Returns an error when clipboard access fails, the clipboard does not expose
/// an image payload, or the PNG cannot be written.
pub(crate) async fn persist_clipboard_image(
    session_id: &str,
    attachment_number: usize,
) -> Result<PersistedClipboardImage, String> {
    let session_id = session_id.to_string();

    tokio::task::spawn_blocking(move || {
        let image_output_path = build_clipboard_image_path(&session_id, attachment_number)?;
        let mut clipboard =
            Clipboard::new().map_err(|error| format!("Clipboard is unavailable: {error}"))?;

        if let Ok(image_data) = clipboard.get_image() {
            std::fs::create_dir_all(
                image_output_path
                    .parent()
                    .ok_or_else(|| "Missing clipboard image directory".to_string())?,
            )
            .map_err(|error| format!("Failed to create clipboard image directory: {error}"))?;

            image::save_buffer_with_format(
                &image_output_path,
                image_data.bytes.as_ref(),
                u32::try_from(image_data.width)
                    .map_err(|_| "Clipboard image width is too large".to_string())?,
                u32::try_from(image_data.height)
                    .map_err(|_| "Clipboard image height is too large".to_string())?,
                ExtendedColorType::Rgba8,
                ImageFormat::Png,
            )
            .map_err(|error| format!("Failed to write pasted image PNG: {error}"))?;

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
    .map_err(|error| format!("Clipboard image task failed: {error}"))?
}

/// Returns the temp directory used for pasted prompt images for one session
/// identifier.
///
/// # Errors
/// Returns an error when `session_id` is empty.
pub(crate) fn clipboard_image_directory(session_id: &str) -> Result<PathBuf, String> {
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
) -> Result<PathBuf, String> {
    let timestamp_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("System clock is before the Unix epoch: {error}"))?
        .as_millis();
    let file_name = format!("image-{attachment_number:03}-{timestamp_millis}.png");

    Ok(clipboard_image_directory(session_id)?.join(file_name))
}

/// Returns the directory-name fragment used for one session image temp root.
///
/// # Errors
/// Returns an error when the session id is empty.
fn session_temp_directory_name(session_id: &str) -> Result<&str, String> {
    if session_id.is_empty() {
        return Err("Session id is missing for clipboard image temp storage".to_string());
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
) -> Result<(), String> {
    let clipboard_text = clipboard
        .get_text()
        .map_err(|_| "Clipboard does not contain an image".to_string())?;
    let source_image_path = PathBuf::from(clipboard_text.trim());

    if source_image_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("png")
    {
        return Err("Clipboard does not contain an image".to_string());
    }

    if !source_image_path.is_file() {
        return Err("Clipboard PNG path does not exist".to_string());
    }

    std::fs::create_dir_all(
        image_output_path
            .parent()
            .ok_or_else(|| "Missing clipboard image directory".to_string())?,
    )
    .map_err(|error| format!("Failed to create clipboard image directory: {error}"))?;
    std::fs::copy(&source_image_path, image_output_path)
        .map_err(|error| format!("Failed to copy clipboard PNG file: {error}"))?;

    Ok(())
}

/// Resolves one persisted image path to the exact absolute filesystem path
/// that downstream transports should reference.
///
/// # Errors
/// Returns an error when the persisted file cannot be resolved from disk.
fn canonicalize_persisted_image_path(image_output_path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(image_output_path)
        .map_err(|error| format!("Failed to resolve pasted image path: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // Act
        let image_path =
            build_clipboard_image_path(session_id, 2).expect("image path should resolve");

        // Assert
        assert_eq!(image_path.parent(), Some(expected_directory.as_path()));
        assert!(image_path.file_name().is_some_and(|name| {
            name.to_string_lossy().starts_with("image-002-")
                && name.to_string_lossy().ends_with(".png")
        }));
    }

    #[test]
    fn test_clipboard_image_directory_rejects_empty_session_id() {
        // Arrange
        let session_id = "";

        // Act
        let result = clipboard_image_directory(session_id);

        // Assert
        assert_eq!(
            result,
            Err("Session id is missing for clipboard image temp storage".to_string())
        );
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
}
