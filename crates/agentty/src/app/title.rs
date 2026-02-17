//! Session title and agent/model normalization helpers.

use crate::agent::{AgentKind, AgentModel};
use crate::model::Session;

/// Stateless helpers for session title and agent/model normalization.
pub(super) struct TitleService;

impl TitleService {
    /// Resolves the persisted agent/model strings into concrete runtime values.
    pub(super) fn resolve_session_agent_and_model(session: &Session) -> (AgentKind, AgentModel) {
        let session_agent = session
            .agent
            .parse::<AgentKind>()
            .unwrap_or(AgentKind::Gemini);
        let session_model = session_agent
            .parse_model(&session.model)
            .unwrap_or_else(|| session_agent.default_model());

        (session_agent, session_model)
    }

    /// Summarizes a prompt into a short single-line session title.
    pub(super) fn summarize_title(prompt: &str) -> String {
        let first_line = prompt.lines().next().unwrap_or(prompt).trim();
        if first_line.len() <= 30 {
            return first_line.to_string();
        }

        let truncated = &first_line[..30];
        if let Some(last_space) = truncated.rfind(' ') {
            format!("{}…", &first_line[..last_space])
        } else {
            format!("{truncated}…")
        }
    }
}
