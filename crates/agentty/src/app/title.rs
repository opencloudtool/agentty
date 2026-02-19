//! Session title and agent/model normalization helpers.

/// Stateless helpers for session title and agent/model normalization.
pub(super) struct TitleService;

impl TitleService {
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
