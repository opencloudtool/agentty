//! Narrow read-only clipboard access used by Agentty prompt image capture.

mod backend;
mod error;
mod format;
mod image_data;
mod uri;

use std::path::PathBuf;

pub use error::ClipboardError;
pub use image_data::RgbaImageData;

const DISABLE_CLIPBOARD_ENV: &str = "AGENTTY_DISABLE_CLIPBOARD";

/// System clipboard reader for text, copied files, and RGBA image data.
pub struct Clipboard {
    backend: Box<dyn backend::ClipboardBackend>,
}

impl Clipboard {
    /// Opens the best clipboard backend available on the current platform.
    ///
    /// # Errors
    /// Returns [`ClipboardError::Unavailable`] when no supported clipboard
    /// backend is available for the current display server or operating
    /// system.
    pub fn new() -> Result<Self, ClipboardError> {
        Self::new_with_backend(Self::disabled_by_env(), backend::new_backend)
    }

    /// Reads clipboard text.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the clipboard has no text or the
    /// backend cannot complete the read.
    pub fn read_text(&mut self) -> Result<String, ClipboardError> {
        self.backend.read_text()
    }

    /// Reads copied filesystem paths from the clipboard.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the clipboard has no file-list payload
    /// or the backend cannot complete the read.
    pub fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        self.backend.read_file_list()
    }

    /// Reads clipboard image data as RGBA pixels.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the clipboard has no image payload,
    /// image decoding fails, or the backend cannot complete the read.
    pub fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        self.backend.read_image_rgba()
    }

    fn new_with_backend<B>(
        disabled_error: Option<ClipboardError>,
        backend_factory: impl FnOnce() -> B,
    ) -> Result<Self, ClipboardError>
    where
        B: IntoBackendResult,
    {
        if let Some(error) = disabled_error {
            return Err(error);
        }

        let backend = backend_factory().into_backend_result()?;

        Ok(Self { backend })
    }

    fn disabled_by_env() -> Option<ClipboardError> {
        std::env::var_os(DISABLE_CLIPBOARD_ENV).map(|_| ClipboardError::Unavailable {
            reason: format!("clipboard access is disabled by `{DISABLE_CLIPBOARD_ENV}`"),
        })
    }
}

trait IntoBackendResult {
    fn into_backend_result(self) -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError>;
}

impl IntoBackendResult for Box<dyn backend::ClipboardBackend> {
    fn into_backend_result(self) -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
        Ok(self)
    }
}

impl IntoBackendResult for Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
    fn into_backend_result(self) -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
        self
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
