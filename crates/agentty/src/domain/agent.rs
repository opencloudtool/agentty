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
    /// Fast Gemini preview model backed by `gemini-3-flash-preview`.
    Gemini3FlashPreview,
    /// Higher-quality Gemini preview model backed by `gemini-3.1-pro-preview`.
    Gemini31ProPreview,
    /// Codex model backed by `gpt-5.4`.
    Gpt54,
    /// Codex spark model backed by `gpt-5.3-codex-spark`.
    Gpt53CodexSpark,
    /// Codex model backed by `gpt-5.3-codex`.
    Gpt53Codex,
    /// Claude Opus model backed by `claude-opus-4-6`.
    ClaudeOpus46,
    /// Claude Sonnet model backed by `claude-sonnet-4-6`.
    ClaudeSonnet46,
    /// Claude Haiku model backed by `claude-haiku-4-5-20251001`.
    ClaudeHaiku4520251001,
}

/// Supported reasoning-effort levels for task execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningLevel {
    /// Low reasoning effort for faster responses.
    Low,
    /// Medium reasoning effort.
    Medium,
    /// High reasoning effort for deeper reasoning.
    #[default]
    High,
    /// Extra-high reasoning effort for deeper analysis.
    XHigh,
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
            Self::Gpt54 => "gpt-5.4",
            Self::Gpt53CodexSpark => "gpt-5.3-codex-spark",
            Self::Gpt53Codex => "gpt-5.3-codex",
            Self::ClaudeOpus46 => "claude-opus-4-6",
            Self::ClaudeSonnet46 => "claude-sonnet-4-6",
            Self::ClaudeHaiku4520251001 => "claude-haiku-4-5-20251001",
        }
    }

    /// Returns the owning provider family for this model.
    pub fn kind(self) -> AgentKind {
        match self {
            Self::Gemini3FlashPreview | Self::Gemini31ProPreview => AgentKind::Gemini,
            Self::Gpt54 | Self::Gpt53CodexSpark | Self::Gpt53Codex => AgentKind::Codex,
            Self::ClaudeOpus46 | Self::ClaudeSonnet46 | Self::ClaudeHaiku4520251001 => {
                AgentKind::Claude
            }
        }
    }
}

impl ReasoningLevel {
    /// All selectable reasoning-effort levels in UI cycle order.
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::XHigh];

    /// Returns the Codex reasoning-effort identifier for this level.
    pub fn codex(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

impl FromStr for ReasoningLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            other => Err(format!("unknown reasoning level: {other}")),
        }
    }
}

impl FromStr for AgentModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gemini-3-flash-preview" => Ok(Self::Gemini3FlashPreview),
            "gemini-3.1-pro-preview" => Ok(Self::Gemini31ProPreview),
            "gpt-5.4" => Ok(Self::Gpt54),
            // Legacy alias retained so persisted settings migrate to Spark.
            "gpt-5.3-codex-spark" | "gpt-5.2-codex" => Ok(Self::Gpt53CodexSpark),
            "gpt-5.3-codex" => Ok(Self::Gpt53Codex),
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
            Self::Gpt54 => "Latest Codex model for coding quality.",
            Self::Gpt53CodexSpark => "Codex spark model for quick coding iterations.",
            Self::Gpt53Codex => "Previous Codex model for coding quality.",
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
            Self::Gemini => AgentModel::Gemini31ProPreview,
            Self::Claude => AgentModel::ClaudeOpus46,
            Self::Codex => AgentModel::Gpt54,
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
            AgentModel::Gemini31ProPreview,
            AgentModel::Gemini3FlashPreview,
        ];
        const CLAUDE_MODELS: &[AgentModel] = &[
            AgentModel::ClaudeOpus46,
            AgentModel::ClaudeSonnet46,
            AgentModel::ClaudeHaiku4520251001,
        ];
        const CODEX_MODELS: &[AgentModel] = &[
            AgentModel::Gpt54,
            AgentModel::Gpt53Codex,
            AgentModel::Gpt53CodexSpark,
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

    #[test]
    /// Ensures `gpt-5.4` parses as a Codex model.
    fn test_parse_model_parses_gpt_54() {
        // Arrange
        let codex_kind = AgentKind::Codex;

        // Act
        let parsed_model = codex_kind.parse_model("gpt-5.4");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gpt54));
    }

    #[test]
    /// Ensures the removed `gpt-5.2-codex` id maps to Spark for compatibility.
    fn test_parse_model_maps_legacy_gpt_52_codex_to_codex_spark() {
        // Arrange
        let codex_kind = AgentKind::Codex;

        // Act
        let parsed_model = codex_kind.parse_model("gpt-5.2-codex");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gpt53CodexSpark));
    }

    #[test]
    /// Ensures reasoning-level parsing accepts all supported persisted values.
    fn test_reasoning_level_from_str_parses_supported_values() {
        // Arrange

        // Act
        let low_level = "low".parse::<ReasoningLevel>();
        let medium_level = "medium".parse::<ReasoningLevel>();
        let high_level = "high".parse::<ReasoningLevel>();
        let xhigh_level = "xhigh".parse::<ReasoningLevel>();

        // Assert
        assert_eq!(low_level, Ok(ReasoningLevel::Low));
        assert_eq!(medium_level, Ok(ReasoningLevel::Medium));
        assert_eq!(high_level, Ok(ReasoningLevel::High));
        assert_eq!(xhigh_level, Ok(ReasoningLevel::XHigh));
    }

    #[test]
    /// Ensures unsupported reasoning values return a parse error.
    fn test_reasoning_level_from_str_rejects_unknown_values() {
        // Arrange

        // Act
        let parse_result = "minimal".parse::<ReasoningLevel>();

        // Assert
        assert!(parse_result.is_err());
    }

    #[test]
    /// Ensures Codex models still resolve their owning provider correctly.
    fn test_codex_model_kind_is_codex() {
        // Arrange
        let model = AgentModel::Gpt54;

        // Act
        let kind = model.kind();

        // Assert
        assert_eq!(kind, AgentKind::Codex);
    }

    #[test]
    /// Ensures Claude models still resolve their owning provider correctly.
    fn test_claude_model_kind_is_claude() {
        // Arrange
        let model = AgentModel::ClaudeSonnet46;

        // Act
        let kind = model.kind();

        // Assert
        assert_eq!(kind, AgentKind::Claude);
    }
}
