#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

struct TestClipboardBackend;

struct TestClipboardBackendFactory {
    error: Option<ClipboardError>,
}

impl TestClipboardBackendFactory {
    fn successful() -> Self {
        Self { error: None }
    }

    fn unavailable(reason: &str) -> Self {
        Self {
            error: Some(ClipboardError::Unavailable {
                reason: reason.to_string(),
            }),
        }
    }

    fn backend_error(reason: &str) -> Self {
        Self {
            error: Some(ClipboardError::Backend {
                reason: reason.to_string(),
            }),
        }
    }
}

impl ClipboardBackendFactory for TestClipboardBackendFactory {
    fn create(&mut self) -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }

        Ok(Box::new(TestClipboardBackend))
    }
}

#[cfg(target_os = "linux")]
struct TestExecutableDirectory {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
impl TestExecutableDirectory {
    fn new() -> Self {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let workspace_target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("clipboard crate should belong to the workspace")
            .join("target");
        let path = workspace_target.join(format!(
            "agentty-clipboard-{}-{unique_suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("clipboard executable directory should be created");

        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(target_os = "linux")]
impl Drop for TestExecutableDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("clipboard executable directory should be removed");
    }
}

impl ClipboardBackend for TestClipboardBackend {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        Ok(String::new())
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        Ok(Vec::new())
    }

    fn read_image_rgba(&mut self) -> Result<crate::RgbaImageData, ClipboardError> {
        Ok(crate::RgbaImageData {
            height: 0,
            rgba_bytes: Vec::new(),
            width: 0,
        })
    }
}

#[test]
fn test_clipboard_backend_returns_empty_fixture_content() {
    // Arrange
    let mut backend = TestClipboardBackend;

    // Act
    let text = backend.read_text();
    let files = backend.read_file_list();
    let image = backend.read_image_rgba();

    // Assert
    assert_eq!(text.expect("fixture text should load"), "");
    assert_eq!(
        files.expect("fixture files should load"),
        [] as [std::path::PathBuf; 0]
    );
    assert_eq!(
        image.expect("fixture image should load"),
        crate::RgbaImageData {
            height: 0,
            rgba_bytes: Vec::new(),
            width: 0,
        }
    );
}

#[test]
fn new_backend_prefers_successful_wayland_without_opening_x11() {
    // Arrange
    let environment = LinuxClipboardEnvironment {
        display: Some(":0".to_string()),
        wayland_display: Some("wayland-0".to_string()),
    };
    // Act
    let result = new_backend_with_factories(
        &environment,
        TestClipboardBackendFactory::successful(),
        TestClipboardBackendFactory::unavailable("X11 should not be opened"),
    );

    // Assert
    assert!(result.is_ok());
}

#[test]
fn new_backend_falls_back_to_x11_when_wayland_initialization_fails() {
    // Arrange
    let environment = LinuxClipboardEnvironment {
        display: Some(":0".to_string()),
        wayland_display: Some("wayland-0".to_string()),
    };

    // Act
    let result = new_backend_with_factories(
        &environment,
        TestClipboardBackendFactory::unavailable("wl-paste is unavailable"),
        TestClipboardBackendFactory::successful(),
    );

    // Assert
    assert!(result.is_ok());
}

#[test]
fn new_backend_reports_both_initialization_failures() {
    // Arrange
    let environment = LinuxClipboardEnvironment {
        display: Some(":0".to_string()),
        wayland_display: Some("wayland-0".to_string()),
    };

    // Act
    let result = new_backend_with_factories(
        &environment,
        TestClipboardBackendFactory::unavailable("wl-paste is unavailable"),
        TestClipboardBackendFactory::unavailable("X11 connection failed"),
    );

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Unavailable { reason })
            if reason.contains("wl-paste is unavailable")
                && reason.contains("X11 connection failed")
    ));
}

#[test]
fn new_backend_preserves_wayland_error_without_x11_display() {
    // Arrange
    let environment = LinuxClipboardEnvironment {
        display: None,
        wayland_display: Some("wayland-0".to_string()),
    };

    // Act
    let result = new_backend_with_factories(
        &environment,
        TestClipboardBackendFactory::backend_error("Wayland compositor rejected the connection"),
        TestClipboardBackendFactory::successful(),
    );

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason == "Wayland compositor rejected the connection"
    ));
}

#[test]
fn new_backend_uses_x11_when_only_x11_display_is_set() {
    // Arrange
    let environment = LinuxClipboardEnvironment {
        display: Some(":0".to_string()),
        wayland_display: None,
    };

    // Act
    let result = new_backend_with_factories(
        &environment,
        TestClipboardBackendFactory::unavailable("Wayland should not be opened"),
        TestClipboardBackendFactory::successful(),
    );

    // Assert
    assert!(result.is_ok());
}

#[test]
fn new_backend_reports_unavailable_without_display_variables() {
    // Arrange
    let environment = LinuxClipboardEnvironment {
        display: None,
        wayland_display: None,
    };

    // Act
    let result = new_backend_with_factories(
        &environment,
        TestClipboardBackendFactory::successful(),
        TestClipboardBackendFactory::successful(),
    );

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Unavailable { reason })
            if reason == "no supported Linux clipboard display was detected"
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn new_backend_uses_detected_linux_platform_backends() {
    const CHILD_MODE_ENVIRONMENT: &str = "AGENTTY_CLIPBOARD_TEST_CHILD_MODE";

    // Arrange
    if let Ok(mode) = env::var(CHILD_MODE_ENVIRONMENT) {
        // Act
        let result = new_backend();

        // Assert
        if mode == "no-display" {
            assert!(matches!(
                result,
                Err(ClipboardError::Unavailable { reason })
                    if reason == "no supported Linux clipboard display was detected"
            ));
        } else if mode == "wayland" {
            assert!(result.is_ok());
        } else {
            assert_eq!(mode, "wayland-x11-failure");
            assert!(matches!(
                result,
                Err(ClipboardError::Unavailable { reason })
                    if reason.contains("Wayland clipboard backend failed")
                        && reason.contains("X11 clipboard backend failed")
            ));
        }

        return;
    }

    let temp_dir = TestExecutableDirectory::new();
    let wl_paste_path = temp_dir.path().join("wl-paste");
    fs::write(
        &wl_paste_path,
        "#!/bin/sh\nexit \"${AGENTTY_WL_PASTE_EXIT_CODE:-0}\"\n",
    )
    .expect("wl-paste test executable should be written");
    fs::set_permissions(&wl_paste_path, fs::Permissions::from_mode(0o750))
        .expect("wl-paste test executable should be executable");
    let executable_search_path = executable_search_path(temp_dir.path());

    // Act
    let no_display = run_backend_child(
        CHILD_MODE_ENVIRONMENT,
        "no-display",
        &executable_search_path,
        None,
        None,
        "0",
    );
    let wayland = run_backend_child(
        CHILD_MODE_ENVIRONMENT,
        "wayland",
        &executable_search_path,
        None,
        Some("wayland-0"),
        "0",
    );
    let wayland_x11_failure = run_backend_child(
        CHILD_MODE_ENVIRONMENT,
        "wayland-x11-failure",
        &executable_search_path,
        Some("invalid-display"),
        Some("wayland-0"),
        "1",
    );

    // Assert
    assert!(no_display.success());
    assert!(wayland.success());
    assert!(wayland_x11_failure.success());
}

#[cfg(target_os = "linux")]
fn executable_search_path(test_executable_directory: &std::path::Path) -> OsString {
    let system_paths = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();

    env::join_paths(std::iter::once(test_executable_directory.to_path_buf()).chain(system_paths))
        .expect("clipboard executable search path should be valid")
}

#[cfg(target_os = "linux")]
fn run_backend_child(
    child_mode_environment: &str,
    child_mode: &str,
    executable_search_path: &OsString,
    display: Option<&str>,
    wayland_display: Option<&str>,
    wl_paste_exit_code: &str,
) -> std::process::ExitStatus {
    let mut command = Command::new(env::current_exe().expect("test executable should resolve"));
    command
        .args([
            "--exact",
            "backend::linux::tests::new_backend_uses_detected_linux_platform_backends",
            "--nocapture",
        ])
        .env(child_mode_environment, child_mode)
        .env("AGENTTY_WL_PASTE_EXIT_CODE", wl_paste_exit_code)
        .env("PATH", executable_search_path)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    if let Some(display) = display {
        command.env("DISPLAY", display);
    }
    if let Some(wayland_display) = wayland_display {
        command.env("WAYLAND_DISPLAY", wayland_display);
    }

    command.status().expect("clipboard child test should run")
}
