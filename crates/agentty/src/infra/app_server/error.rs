/// Typed error returned by app-server infrastructure operations.
///
/// Wraps transport failures, prompt rendering issues, lock errors, and
/// provider-specific failures so callers can distinguish error categories
/// without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum AppServerError {
    /// The session registry mutex is poisoned.
    #[error("Failed to lock {provider} app-server session map")]
    LockPoisoned {
        /// Provider label for diagnostics.
        provider: &'static str,
    },

    /// A prompt template or protocol instruction rendering failed.
    #[error("{0}")]
    PromptRender(String),

    /// A provider-specific runtime startup or turn execution failure.
    #[error("{0}")]
    Provider(String),

    /// Both the initial attempt and one retry after restart failed.
    #[error(
        "{provider} app-server failed, then retry failed after restart: first error: \
         {first_error}; retry error: {retry_error}"
    )]
    RetryExhausted {
        /// Provider label for diagnostics.
        provider: &'static str,
        /// Error message from the first failed attempt.
        first_error: String,
        /// Error message from the retry attempt after restart.
        retry_error: String,
    },

    /// An stdio transport or process communication failure.
    #[error("{0}")]
    Transport(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_poisoned_display_includes_provider_name() {
        // Arrange
        let error = AppServerError::LockPoisoned { provider: "Codex" };

        // Act
        let display = error.to_string();

        // Assert
        assert_eq!(display, "Failed to lock Codex app-server session map");
    }

    #[test]
    fn prompt_render_display_shows_message() {
        // Arrange
        let error = AppServerError::PromptRender("template syntax error".to_string());

        // Act / Assert
        assert_eq!(error.to_string(), "template syntax error");
    }

    #[test]
    fn provider_display_shows_message() {
        // Arrange
        let error = AppServerError::Provider("runtime crashed".to_string());

        // Act / Assert
        assert_eq!(error.to_string(), "runtime crashed");
    }

    #[test]
    fn retry_exhausted_display_includes_both_errors() {
        // Arrange
        let error = AppServerError::RetryExhausted {
            provider: "Gemini ACP",
            first_error: "connection reset".to_string(),
            retry_error: "timeout".to_string(),
        };

        // Act
        let display = error.to_string();

        // Assert
        assert_eq!(
            display,
            "Gemini ACP app-server failed, then retry failed after restart: first error: \
             connection reset; retry error: timeout"
        );
    }

    #[test]
    fn transport_display_shows_message() {
        // Arrange
        let error = AppServerError::Transport("stdin write failed".to_string());

        // Act / Assert
        assert_eq!(error.to_string(), "stdin write failed");
    }
}
