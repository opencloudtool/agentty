#[cfg(target_os = "linux")]
use std::env;

use super::ClipboardBackend;
#[cfg(target_os = "linux")]
use super::{wayland, x11};
use crate::ClipboardError;

#[cfg(target_os = "linux")]
pub(crate) fn new_backend() -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
    new_backend_with_factories(
        &LinuxClipboardEnvironment::from_env(),
        WaylandClipboardFactory,
        X11ClipboardFactory,
    )
}

#[derive(Debug, Default, Eq, PartialEq)]
struct LinuxClipboardEnvironment {
    display: Option<String>,
    wayland_display: Option<String>,
}

impl LinuxClipboardEnvironment {
    #[cfg(target_os = "linux")]
    fn from_env() -> Self {
        Self {
            display: env::var("DISPLAY").ok(),
            wayland_display: env::var("WAYLAND_DISPLAY").ok(),
        }
    }
}

trait ClipboardBackendFactory {
    fn create(&mut self) -> Result<Box<dyn ClipboardBackend>, ClipboardError>;
}

#[cfg(target_os = "linux")]
struct WaylandClipboardFactory;

#[cfg(target_os = "linux")]
impl ClipboardBackendFactory for WaylandClipboardFactory {
    fn create(&mut self) -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
        Ok(Box::new(wayland::WaylandClipboard::new()?))
    }
}

#[cfg(target_os = "linux")]
struct X11ClipboardFactory;

#[cfg(target_os = "linux")]
impl ClipboardBackendFactory for X11ClipboardFactory {
    fn create(&mut self) -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
        Ok(Box::new(x11::X11Clipboard::new()?))
    }
}

fn new_backend_with_factories<WaylandFactory, X11Factory>(
    environment: &LinuxClipboardEnvironment,
    mut wayland_factory: WaylandFactory,
    mut x11_factory: X11Factory,
) -> Result<Box<dyn ClipboardBackend>, ClipboardError>
where
    WaylandFactory: ClipboardBackendFactory,
    X11Factory: ClipboardBackendFactory,
{
    let has_wayland_display = environment
        .wayland_display
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    let has_x11_display = environment
        .display
        .as_deref()
        .is_some_and(|value| !value.is_empty());

    if has_wayland_display {
        match wayland_factory.create() {
            Ok(backend) => return Ok(backend),
            Err(wayland_error) if has_x11_display => {
                return x11_factory
                    .create()
                    .map_err(|x11_error| ClipboardError::Unavailable {
                        reason: format!(
                            "Wayland clipboard backend failed: {wayland_error}; X11 clipboard \
                             backend failed: {x11_error}"
                        ),
                    });
            }
            Err(error) => return Err(error),
        }
    }

    if has_x11_display {
        return x11_factory.create();
    }

    Err(ClipboardError::Unavailable {
        reason: "no supported Linux clipboard display was detected".to_string(),
    })
}

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
