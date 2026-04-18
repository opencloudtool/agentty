use std::fmt;
use std::str::FromStr;

/// Supported agent provider families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Claude Opus model backed by `claude-opus-4-7`.
    ClaudeOpus47,
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
            Self::ClaudeOpus47 => "claude-opus-4-7",
            Self::ClaudeSonnet46 => "claude-sonnet-4-6",
            Self::ClaudeHaiku4520251001 => "claude-haiku-4-5-20251001",
        }
    }

    /// Parses one persisted model identifier and upgrades retired aliases.
    ///
    /// Stored `claude-opus-4-6` values are migrated forward to
    /// `claude-opus-4-7` so existing projects and sessions continue loading
    /// after `claude-opus-4-6` support is removed from the active model list.
    pub(crate) fn parse_persisted(value: &str) -> Result<Self, String> {
        match value {
            "claude-opus-4-6" => Ok(Self::ClaudeOpus47),
            _ => value.parse(),
        }
    }

    /// Returns the owning provider family for this model.
    pub fn kind(self) -> AgentKind {
        match self {
            Self::Gemini3FlashPreview | Self::Gemini31ProPreview => AgentKind::Gemini,
            Self::Gpt54 | Self::Gpt53CodexSpark => AgentKind::Codex,
            Self::ClaudeOpus47 | Self::ClaudeSonnet46 | Self::ClaudeHaiku4520251001 => {
                AgentKind::Claude
            }
        }
    }
}

/// Returns all selectable models owned by the provided agent kinds in stable
/// settings and slash-menu order.
#[must_use]
pub fn selectable_models_for_agent_kinds(agent_kinds: &[AgentKind]) -> Vec<AgentModel> {
    agent_kinds
        .iter()
        .flat_map(|agent_kind| agent_kind.models())
        .copied()
        .collect()
}

/// Resolves one model against the currently available agent kinds.
///
/// When `model` belongs to an unavailable provider, this prefers
/// `fallback_model` when its provider is available and otherwise falls back to
/// the first available provider default in `agent_kinds`. When no providers
/// are available, it returns `fallback_model` unchanged.
#[must_use]
pub fn resolve_model_for_available_agent_kinds(
    model: AgentModel,
    agent_kinds: &[AgentKind],
    fallback_model: AgentModel,
) -> AgentModel {
    if agent_kinds.contains(&model.kind()) {
        return model;
    }

    if agent_kinds.contains(&fallback_model.kind()) {
        return fallback_model;
    }

    agent_kinds
        .first()
        .copied()
        .map_or(fallback_model, AgentKind::default_model)
}

/// Resolves the agent kind used for prompt-side `/model` selection.
///
/// This preserves `session_agent_kind` when that backend is still available
/// and otherwise falls back to the first available backend. When no backends
/// are available, it returns `None`.
#[must_use]
pub fn resolve_prompt_model_agent_kind(
    session_agent_kind: AgentKind,
    agent_kinds: &[AgentKind],
) -> Option<AgentKind> {
    if agent_kinds.contains(&session_agent_kind) {
        return Some(session_agent_kind);
    }

    agent_kinds.first().copied()
}

impl ReasoningLevel {
    /// All selectable reasoning-effort levels in UI cycle order.
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::XHigh];

    /// Returns the stable persisted identifier for this level.
    ///
    /// This value is stored in the database and remains independent from any
    /// provider-specific transport string changes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }

    /// Returns the Codex reasoning-effort identifier for this level.
    pub fn codex(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }

    /// Returns the Claude `--effort` value for this level.
    ///
    /// Maps `XHigh` to `"max"`, which is currently only supported on
    /// `claude-opus-4-7`. The Claude CLI enforces this restriction and will
    /// surface an error for other models.
    pub fn claude(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "max",
        }
    }

    /// Returns a short UI description for this reasoning level.
    pub fn description(self) -> &'static str {
        match self {
            Self::Low => "Fastest responses with lighter reasoning.",
            Self::Medium => "Balanced speed and reasoning depth.",
            Self::High => "Deeper reasoning for tougher tasks.",
            Self::XHigh => "Maximum reasoning effort for the hardest tasks.",
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
            "gpt-5.3-codex-spark" => Ok(Self::Gpt53CodexSpark),
            "claude-opus-4-7" => Ok(Self::ClaudeOpus47),
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
            Self::ClaudeOpus47 => "Latest Claude Opus model for complex tasks.",
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
            Self::Claude => AgentModel::ClaudeOpus47,
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
            AgentModel::ClaudeOpus47,
            AgentModel::ClaudeSonnet46,
            AgentModel::ClaudeHaiku4520251001,
        ];
        const CODEX_MODELS: &[AgentModel] = &[AgentModel::Gpt54, AgentModel::Gpt53CodexSpark];

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

impl AgentSelectionMetadata for ReasoningLevel {
    fn name(&self) -> &'static str {
        (*self).as_str()
    }

    fn description(&self) -> &'static str {
        (*self).description()
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
    /// Ensures retired Claude models no longer parse as selectable models.
    fn test_parse_model_rejects_retired_claude_opus_46() {
        // Arrange
        let claude_kind = AgentKind::Claude;

        // Act
        let parsed_model = claude_kind.parse_model("claude-opus-4-6");

        // Assert
        assert_eq!(parsed_model, None);
    }

    #[test]
    /// Ensures persisted retired Claude model ids migrate to the supported
    /// replacement model.
    fn test_parse_persisted_maps_retired_claude_opus_46_to_claude_opus_47() {
        // Arrange

        // Act
        let parsed_model = AgentModel::parse_persisted("claude-opus-4-6");

        // Assert
        assert_eq!(parsed_model, Ok(AgentModel::ClaudeOpus47));
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

    #[test]
    /// Ensures `ReasoningLevel::claude()` maps all levels to the correct
    /// Claude `--effort` values, including `XHigh` → `"max"`.
    fn test_reasoning_level_claude_maps_all_levels() {
        // Arrange / Act / Assert
        assert_eq!(ReasoningLevel::Low.claude(), "low");
        assert_eq!(ReasoningLevel::Medium.claude(), "medium");
        assert_eq!(ReasoningLevel::High.claude(), "high");
        assert_eq!(ReasoningLevel::XHigh.claude(), "max");
    }

    #[test]
    /// Ensures persisted reasoning identifiers stay stable even if provider
    /// transport names change in the future.
    fn test_reasoning_level_as_str_returns_stable_persisted_values() {
        // Arrange / Act / Assert
        assert_eq!(ReasoningLevel::Low.as_str(), "low");
        assert_eq!(ReasoningLevel::Medium.as_str(), "medium");
        assert_eq!(ReasoningLevel::High.as_str(), "high");
        assert_eq!(ReasoningLevel::XHigh.as_str(), "xhigh");
    }

    #[test]
    /// Ensures selectable-model ordering follows the provided provider order.
    fn test_selectable_models_for_agent_kinds_uses_provider_order() {
        // Arrange
        let agent_kinds = [AgentKind::Codex, AgentKind::Gemini];

        // Act
        let selectable_models = selectable_models_for_agent_kinds(&agent_kinds);

        // Assert
        assert_eq!(
            selectable_models,
            vec![
                AgentModel::Gpt54,
                AgentModel::Gpt53CodexSpark,
                AgentModel::Gemini31ProPreview,
                AgentModel::Gemini3FlashPreview,
            ]
        );
    }

    #[test]
    /// Ensures unavailable models fall back to an available preferred model
    /// when possible.
    fn test_resolve_model_for_available_agent_kinds_prefers_available_fallback() {
        // Arrange
        let unavailable_model = AgentModel::ClaudeOpus47;
        let available_agent_kinds = [AgentKind::Codex, AgentKind::Gemini];
        let fallback_model = AgentModel::Gpt54;

        // Act
        let resolved_model = resolve_model_for_available_agent_kinds(
            unavailable_model,
            &available_agent_kinds,
            fallback_model,
        );

        // Assert
        assert_eq!(resolved_model, AgentModel::Gpt54);
    }

    #[test]
    /// Ensures unavailable models fall back to the first available provider
    /// default when the preferred fallback is also unavailable.
    fn test_resolve_model_for_available_agent_kinds_uses_first_available_default() {
        // Arrange
        let unavailable_model = AgentModel::ClaudeOpus47;
        let available_agent_kinds = [AgentKind::Codex, AgentKind::Gemini];
        let unavailable_fallback_model = AgentModel::ClaudeSonnet46;

        // Act
        let resolved_model = resolve_model_for_available_agent_kinds(
            unavailable_model,
            &available_agent_kinds,
            unavailable_fallback_model,
        );

        // Assert
        assert_eq!(resolved_model, AgentKind::Codex.default_model());
    }

    #[test]
    /// Ensures prompt model selection keeps the current backend when it is
    /// still available locally.
    fn test_resolve_prompt_model_agent_kind_prefers_current_agent() {
        // Arrange
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Codex];

        // Act
        let resolved_agent_kind =
            resolve_prompt_model_agent_kind(AgentKind::Codex, &available_agent_kinds);

        // Assert
        assert_eq!(resolved_agent_kind, Some(AgentKind::Codex));
    }

    #[test]
    /// Ensures prompt model selection falls back to the first locally
    /// available backend when the current backend is unavailable.
    fn test_resolve_prompt_model_agent_kind_uses_first_available_agent() {
        // Arrange
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Codex];

        // Act
        let resolved_agent_kind =
            resolve_prompt_model_agent_kind(AgentKind::Claude, &available_agent_kinds);

        // Assert
        assert_eq!(resolved_agent_kind, Some(AgentKind::Gemini));
    }
}
