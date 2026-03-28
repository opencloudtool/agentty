//! Proof backend trait for swappable proof output formats.
//!
//! All proof rendering flows through [`ProofBackend`], allowing new visual
//! formats (text, strip, GIF, HTML) to be added without modifying
//! [`ProofReport`](super::report::ProofReport) or scenario code.

use std::path::Path;

use super::report::{ProofError, ProofReport};

/// Trait for rendering a [`ProofReport`] to a file.
///
/// Each implementation produces a different visual format. The report
/// is passed by reference so multiple backends can render the same data.
pub trait ProofBackend {
    /// Render the proof report and write the output to `path`.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or I/O fails.
    fn render(&self, report: &ProofReport, output: &Path) -> Result<(), ProofError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal backend implementation for testing the trait contract.
    struct StubBackend {
        should_fail: bool,
    }

    impl ProofBackend for StubBackend {
        fn render(&self, _report: &ProofReport, _output: &Path) -> Result<(), ProofError> {
            if self.should_fail {
                return Err(ProofError::Format("stub failure".to_string()));
            }

            Ok(())
        }
    }

    #[test]
    fn stub_backend_succeeds() {
        // Arrange
        let backend = StubBackend { should_fail: false };
        let report = ProofReport::new("test");

        // Act
        let result = backend.render(&report, Path::new("/tmp/test.txt"));

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn stub_backend_returns_error() {
        // Arrange
        let backend = StubBackend { should_fail: true };
        let report = ProofReport::new("test");

        // Act
        let result = backend.render(&report, Path::new("/tmp/test.txt"));

        // Assert
        assert!(result.is_err());
    }
}
