//! Domain model for persisted application setting keys.

use std::fmt;

/// Stable keys used in the `setting` table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingName {
    /// Persists the selected reasoning-effort level.
    ReasoningLevel,
    /// Persists the project or global fast-model selection.
    DefaultFastModel,
    /// Persists the project or global review-model selection.
    DefaultReviewModel,
    /// Persists the project or global smart-model selection.
    DefaultSmartModel,
    /// Persists the configured open-command override.
    OpenCommand,
    /// Persists whether the last used model should become the default.
    LastUsedModelAsDefault,
}

impl SettingName {
    /// Returns the persisted key string for one setting.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReasoningLevel => "ReasoningLevel",
            Self::DefaultFastModel => "DefaultFastModel",
            Self::DefaultReviewModel => "DefaultReviewModel",
            Self::DefaultSmartModel => "DefaultSmartModel",
            Self::OpenCommand => "OpenCommand",
            Self::LastUsedModelAsDefault => "LastUsedModelAsDefault",
        }
    }
}

impl fmt::Display for SettingName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensures every setting key keeps its persisted wire value.
    #[test]
    fn test_as_str_returns_persisted_keys() {
        // Arrange
        let settings = [
            (SettingName::ReasoningLevel, "ReasoningLevel"),
            (SettingName::DefaultFastModel, "DefaultFastModel"),
            (SettingName::DefaultReviewModel, "DefaultReviewModel"),
            (SettingName::DefaultSmartModel, "DefaultSmartModel"),
            (SettingName::OpenCommand, "OpenCommand"),
            (
                SettingName::LastUsedModelAsDefault,
                "LastUsedModelAsDefault",
            ),
        ];

        // Act & Assert
        for (setting_name, expected_key) in settings {
            assert_eq!(setting_name.as_str(), expected_key);
        }
    }

    /// Ensures the display output stays aligned with the persisted key.
    #[test]
    fn test_display_matches_as_str() {
        // Arrange
        let settings = [
            SettingName::ReasoningLevel,
            SettingName::DefaultFastModel,
            SettingName::DefaultReviewModel,
            SettingName::DefaultSmartModel,
            SettingName::OpenCommand,
            SettingName::LastUsedModelAsDefault,
        ];

        // Act & Assert
        for setting_name in settings {
            assert_eq!(setting_name.to_string(), setting_name.as_str());
        }
    }
}
