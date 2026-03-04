//! Domain model for persisted application setting keys.

/// Stable keys used in the `setting` table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingName {
    ReasoningLevel,
    DefaultFastModel,
    DefaultReviewModel,
    DefaultSmartModel,
    OpenCommand,
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
