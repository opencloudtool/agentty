use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Supported agent provider families.
pub enum AgentKind {
    /// Google Gemini CLI/backend.
    Gemini,
    /// Anthropic Claude Code CLI/backend.
    Claude,
    /// `OpenAI` Codex CLI/backend.
    Codex,
}

/// Supported agent model names across all providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentModel {
    Gemini3FlashPreview,
    /// Higher-quality Gemini preview model backed by `gemini-3.1-pro-preview`.
    Gemini31ProPreview,
    Gpt53CodexSpark,
    Gpt53Codex,
    Gpt52Codex,
    ClaudeOpus46,
    ClaudeSonnet46,
    ClaudeHaiku4520251001,
}

/// Human-readable metadata for slash-menu selectable items.
pub trait AgentSelectionMetadata {
    /// Returns a stable item name shown in menus.
    fn name(&self) -> &'static str;

    /// Returns a short descriptive subtitle shown in menus.
    fn description(&self) -> &'static str;
}

impl AgentModel {
    /// Returns the stable wire/model identifier used in persistence and CLI
    /// invocations.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemini3FlashPreview => "gemini-3-flash-preview",
            Self::Gemini31ProPreview => "gemini-3.1-pro-preview",
            Self::Gpt53CodexSpark => "gpt-5.3-codex-spark",
            Self::Gpt53Codex => "gpt-5.3-codex",
            Self::Gpt52Codex => "gpt-5.2-codex",
            Self::ClaudeOpus46 => "claude-opus-4-6",
            Self::ClaudeSonnet46 => "claude-sonnet-4-6",
            Self::ClaudeHaiku4520251001 => "claude-haiku-4-5-20251001",
        }
    }

    /// Returns the owning provider family for this model.
    pub fn kind(self) -> AgentKind {
        match self {
            Self::Gemini3FlashPreview | Self::Gemini31ProPreview => AgentKind::Gemini,
            Self::Gpt53CodexSpark | Self::Gpt53Codex | Self::Gpt52Codex => AgentKind::Codex,
            Self::ClaudeOpus46 | Self::ClaudeSonnet46 | Self::ClaudeHaiku4520251001 => {
                AgentKind::Claude
            }
        }
    }
}

impl FromStr for AgentModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gemini-3-flash-preview" => Ok(Self::Gemini3FlashPreview),
            "gemini-3.1-pro-preview" => Ok(Self::Gemini31ProPreview),
            "gpt-5.3-codex-spark" => Ok(Self::Gpt53CodexSpark),
            "gpt-5.3-codex" => Ok(Self::Gpt53Codex),
            "gpt-5.2-codex" => Ok(Self::Gpt52Codex),
            "claude-opus-4-6" => Ok(Self::ClaudeOpus46),
            "claude-sonnet-4-6" => Ok(Self::ClaudeSonnet46),
            "claude-haiku-4-5-20251001" => Ok(Self::ClaudeHaiku4520251001),
            other => Err(format!("unknown model: {other}")),
        }
    }
}

impl AgentSelectionMetadata for AgentModel {
    fn name(&self) -> &'static str {
        (*self).as_str()
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Gemini3FlashPreview => "Fast Gemini model for quick iterations.",
            Self::Gemini31ProPreview => "Higher-quality Gemini model for deeper reasoning.",
            Self::Gpt53CodexSpark => "Latest Codex spark model for coding quality.",
            Self::Gpt53Codex => "Latest Codex model for coding quality.",
            Self::Gpt52Codex => "Faster Codex model with lower cost.",
            Self::ClaudeOpus46 => "Top-tier Claude model for complex tasks.",
            Self::ClaudeSonnet46 => "Balanced Claude model for quality and latency.",
            Self::ClaudeHaiku4520251001 => "Fast Claude model for lighter tasks.",
        }
    }
}

impl AgentKind {
    /// All available agent kinds, in display order.
    pub const ALL: &[AgentKind] = &[AgentKind::Gemini, AgentKind::Claude, AgentKind::Codex];

    /// Returns the default model for this agent kind.
    pub fn default_model(self) -> AgentModel {
        match self {
            Self::Gemini => AgentModel::Gemini3FlashPreview,
            Self::Claude => AgentModel::ClaudeOpus46,
            Self::Codex => AgentModel::Gpt53Codex,
        }
    }

    /// Returns the model string when it belongs to this agent kind.
    pub fn model_str(self, model: AgentModel) -> Option<&'static str> {
        if model.kind() != self {
            return None;
        }

        Some(model.as_str())
    }

    /// Returns the curated model list for this agent kind.
    pub fn models(self) -> &'static [AgentModel] {
        const GEMINI_MODELS: &[AgentModel] = &[
            AgentModel::Gemini3FlashPreview,
            AgentModel::Gemini31ProPreview,
        ];
        const CLAUDE_MODELS: &[AgentModel] = &[
            AgentModel::ClaudeOpus46,
            AgentModel::ClaudeSonnet46,
            AgentModel::ClaudeHaiku4520251001,
        ];
        const CODEX_MODELS: &[AgentModel] = &[
            AgentModel::Gpt53Codex,
            AgentModel::Gpt53CodexSpark,
            AgentModel::Gpt52Codex,
        ];

        match self {
            Self::Gemini => GEMINI_MODELS,
            Self::Claude => CLAUDE_MODELS,
            Self::Codex => CODEX_MODELS,
        }
    }

    /// Parses a provider-specific model string for this agent kind.
    pub fn parse_model(self, value: &str) -> Option<AgentModel> {
        let model = value.parse::<AgentModel>().ok()?;
        if model.kind() != self {
            return None;
        }

        Some(model)
    }
}

impl AgentSelectionMetadata for AgentKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Gemini => "Google Gemini CLI agent.",
            Self::Claude => "Anthropic Claude Code agent.",
            Self::Codex => "OpenAI Codex CLI agent.",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl FromStr for AgentKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "gemini" => Ok(Self::Gemini),
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => Err(format!("unknown agent kind: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Ensures model parsing is constrained to the selected provider.
    fn test_parse_model_returns_none_for_models_from_other_providers() {
        // Arrange
        let claude_kind = AgentKind::Claude;
        let gemini_model = AgentModel::Gemini3FlashPreview.as_str();

        // Act
        let parsed_model = claude_kind.parse_model(gemini_model);

        // Assert
        assert_eq!(parsed_model, None);
    }
}
