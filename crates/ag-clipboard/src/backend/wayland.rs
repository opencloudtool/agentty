#[cfg(target_os = "linux")]
use std::io;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

use image::ImageFormat;

use super::ClipboardBackend;
use crate::{ClipboardError, RgbaImageData, format, uri};

const IMAGE_PNG_MIME: &str = "image/png";
const TEXT_MIME_CANDIDATES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "text/plain",
    "UTF8_STRING",
    "TEXT",
    "STRING",
];
const TEXT_URI_LIST_MIME: &str = "text/uri-list";
#[cfg(target_os = "linux")]
const WL_PASTE_COMMAND: &str = "wl-paste";
const WL_PASTE_LIST_TYPES_ARGS: &[&str] = &["--list-types"];
#[cfg(target_os = "linux")]
const WL_PASTE_VERSION_ARGS: &[&str] = &["--version"];

pub(crate) struct WaylandClipboard {
    runner: Box<dyn WaylandCommandRunner>,
}

impl WaylandClipboard {
    #[cfg(target_os = "linux")]
    pub(crate) fn new() -> Result<Self, ClipboardError> {
        let clipboard = Self::with_runner(Box::new(SystemWaylandCommandRunner));
        clipboard.ensure_wl_paste_available()?;

        Ok(clipboard)
    }

    fn with_runner(runner: Box<dyn WaylandCommandRunner>) -> Self {
        Self { runner }
    }

    #[cfg(target_os = "linux")]
    fn ensure_wl_paste_available(&self) -> Result<(), ClipboardError> {
        let output = self.run_wl_paste(WL_PASTE_VERSION_ARGS)?;
        if !output.status_success {
            return Err(ClipboardError::Unavailable {
                reason: "`wl-paste --version` failed; install the `wl-clipboard` package"
                    .to_string(),
            });
        }

        Ok(())
    }

    fn available_mime_types(&self) -> Result<Vec<String>, ClipboardError> {
        let stdout = self.run_successful(
            WL_PASTE_LIST_TYPES_ARGS,
            "failed to list Wayland clipboard types",
        )?;
        let mime_types = Self::parse_mime_types(&stdout);
        if mime_types.is_empty() {
            return Err(ClipboardError::ContentUnavailable);
        }

        Ok(mime_types)
    }

    fn read_clipboard_bytes_for_mime(&self, mime_type: &str) -> Result<Vec<u8>, ClipboardError> {
        let args = ["--no-newline", "--type", mime_type];

        self.run_successful(&args, "failed to read Wayland clipboard payload")
    }

    fn run_successful(
        &self,
        args: &[&str],
        context: &'static str,
    ) -> Result<Vec<u8>, ClipboardError> {
        let output = self.run_wl_paste(args)?;
        if !output.status_success {
            return Err(ClipboardError::Backend {
                reason: Self::wl_paste_failure_reason(context, &output.stderr),
            });
        }

        Ok(output.stdout)
    }

    fn run_wl_paste(&self, args: &[&str]) -> Result<WaylandCommandOutput, ClipboardError> {
        let owned_args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();

        self.runner.run(&owned_args)
    }

    fn parse_mime_types(stdout: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn wl_paste_failure_reason(context: &str, stderr: &[u8]) -> String {
        let stderr = String::from_utf8_lossy(stderr).trim().to_string();
        if stderr.is_empty() {
            return format!("{context}: `wl-paste` exited unsuccessfully");
        }

        format!("{context}: {stderr}")
    }

    fn select_text_mime_type(mime_types: &[String]) -> Option<&str> {
        TEXT_MIME_CANDIDATES
            .iter()
            .find(|candidate| mime_types.iter().any(|mime_type| mime_type == **candidate))
            .copied()
    }
}

impl ClipboardBackend for WaylandClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        let mime_types = self.available_mime_types()?;
        let mime_type =
            Self::select_text_mime_type(&mime_types).ok_or(ClipboardError::ContentUnavailable)?;
        let bytes = self.read_clipboard_bytes_for_mime(mime_type)?;

        String::from_utf8(bytes).map_err(|error| {
            ClipboardError::backend("failed to decode Wayland clipboard text as UTF-8", error)
        })
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        let mime_types = self.available_mime_types()?;
        if !mime_types
            .iter()
            .any(|mime_type| mime_type == TEXT_URI_LIST_MIME)
        {
            return Err(ClipboardError::ContentUnavailable);
        }
        let bytes = self.read_clipboard_bytes_for_mime(TEXT_URI_LIST_MIME)?;
        let paths = uri::paths_from_uri_list(&bytes);
        if paths.is_empty() {
            return Err(ClipboardError::ContentUnavailable);
        }

        Ok(paths)
    }

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        let mime_types = self.available_mime_types()?;
        if !mime_types
            .iter()
            .any(|mime_type| mime_type == IMAGE_PNG_MIME)
        {
            return Err(ClipboardError::ContentUnavailable);
        }
        let bytes = self.read_clipboard_bytes_for_mime(IMAGE_PNG_MIME)?;

        format::decode_image_rgba(&bytes, ImageFormat::Png)
    }
}

struct WaylandCommandOutput {
    status_success: bool,
    stderr: Vec<u8>,
    stdout: Vec<u8>,
}

#[cfg_attr(test, mockall::automock)]
trait WaylandCommandRunner {
    fn run(&self, args: &[String]) -> Result<WaylandCommandOutput, ClipboardError>;
}

#[cfg(target_os = "linux")]
struct SystemWaylandCommandRunner;

#[cfg(target_os = "linux")]
impl WaylandCommandRunner for SystemWaylandCommandRunner {
    fn run(&self, args: &[String]) -> Result<WaylandCommandOutput, ClipboardError> {
        let output = Command::new(WL_PASTE_COMMAND)
            .args(args)
            .output()
            .map_err(map_wl_paste_spawn_error)?;

        Ok(WaylandCommandOutput {
            status_success: output.status.success(),
            stderr: output.stderr,
            stdout: output.stdout,
        })
    }
}

#[cfg(target_os = "linux")]
fn map_wl_paste_spawn_error(error: io::Error) -> ClipboardError {
    if error.kind() == io::ErrorKind::NotFound {
        return ClipboardError::Unavailable {
            reason: "Wayland clipboard image paste requires `wl-paste`; install the \
                     `wl-clipboard` package"
                .to_string(),
        };
    }

    ClipboardError::backend("failed to run `wl-paste`", error)
}

#[cfg(test)]
#[path = "wayland_test.rs"]
mod tests;
