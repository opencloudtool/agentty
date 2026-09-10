use std::process::Command;

use super::*;

const DISABLED_CONSTRUCTOR_CHILD_ENV: &str = "AGENTTY_TEST_DISABLED_CONSTRUCTOR_CHILD";

struct TestClipboardBackend;

impl backend::ClipboardBackend for TestClipboardBackend {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        Ok("test clipboard text".to_string())
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        Ok(vec![PathBuf::from("/tmp/test.txt")])
    }

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        Ok(RgbaImageData {
            height: 1,
            rgba_bytes: vec![0, 0, 0, 255],
            width: 1,
        })
    }
}

#[test]
fn test_new_respects_disable_environment_variable_in_child_process() {
    if std::env::var_os(DISABLED_CONSTRUCTOR_CHILD_ENV).is_some() {
        // Arrange
        let expected_environment_variable = DISABLE_CLIPBOARD_ENV;

        // Act
        let result = Clipboard::new();

        // Assert
        assert!(matches!(
            result,
            Err(ClipboardError::Unavailable { reason })
                if reason.contains(expected_environment_variable)
        ));

        return;
    }

    // Arrange
    let current_test_binary =
        std::env::current_exe().expect("current test binary path should be available");

    // Act
    let output = Command::new(current_test_binary)
        .arg("--exact")
        .arg("tests::test_new_respects_disable_environment_variable_in_child_process")
        .arg("--nocapture")
        .env(DISABLED_CONSTRUCTOR_CHILD_ENV, "1")
        .env(DISABLE_CLIPBOARD_ENV, "1")
        .output()
        .expect("disabled constructor child test should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Assert
    assert!(
        output.status.success(),
        "disabled constructor child test failed: {stderr}"
    );
}

#[test]
fn test_new_with_backend_respects_disabled_error() {
    // Arrange
    let disabled_error = ClipboardError::Unavailable {
        reason: "test clipboard is disabled".to_string(),
    };
    let backend_factory = unavailable_backend_factory;

    // Act
    let result = Clipboard::new_with_backend(Some(disabled_error), backend_factory);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Unavailable { reason })
            if reason == "test clipboard is disabled"
    ));
}

#[test]
fn test_new_with_backend_accepts_infallible_backend() {
    // Arrange
    let backend_factory = || Box::new(TestClipboardBackend) as Box<dyn backend::ClipboardBackend>;

    // Act
    let mut clipboard = Clipboard::new_with_backend(None, backend_factory)
        .expect("infallible test backend should initialize");
    let text = clipboard
        .read_text()
        .expect("test backend should return text");
    let paths = clipboard
        .read_file_list()
        .expect("test backend should return a file list");
    let image = clipboard
        .read_image_rgba()
        .expect("test backend should return image data");

    // Assert
    assert_eq!(text, "test clipboard text");
    assert_eq!(paths, vec![PathBuf::from("/tmp/test.txt")]);
    assert_eq!(image.width, 1);
    assert_eq!(image.height, 1);
    assert_eq!(image.rgba_bytes, vec![0, 0, 0, 255]);
}

#[test]
fn test_new_with_backend_propagates_backend_error() {
    // Arrange
    let backend_factory = unavailable_backend_factory;

    // Act
    let result = Clipboard::new_with_backend(None, backend_factory);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Unavailable { reason })
            if reason == "test backend is unavailable"
    ));
}

fn unavailable_backend_factory() -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
    Err(ClipboardError::Unavailable {
        reason: "test backend is unavailable".to_string(),
    })
}
