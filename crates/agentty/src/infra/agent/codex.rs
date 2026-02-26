use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, build_resume_prompt};

/// Codex config override that forces high reasoning effort per invocation.
const CODEX_REASONING_EFFORT_CONFIG: &str = r#"model_reasoning_effort="high""#;

/// Uses non-interactive Codex commands so Agentty can capture piped output.
///
/// Interactive `codex` requires a TTY and fails in this app with
/// `Error: stdout is not a terminal`, so this backend runs
/// `codex exec --full-auto`. Resume uses `codex exec resume --last --full-auto`
/// only when replay history is not injected into the prompt.
pub(super) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) {
        // Codex CLI needs no config files
    }

    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let prompt = prepend_root_instructions_if_available(prompt, folder);
        let mut command = Command::new("codex");
        command
            .arg("exec")
            .arg("-c")
            .arg(CODEX_REASONING_EFFORT_CONFIG)
            .arg("--model")
            .arg(model)
            .arg("--full-auto")
            .arg("--json")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command
    }

    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        session_output: Option<String>,
    ) -> Command {
        let has_history_replay = session_output
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let prompt = prepend_root_instructions_if_available(&prompt, folder);
        let mut command = Command::new("codex");
        command.arg("exec").arg("resume");

        if !has_history_replay {
            command.arg("--last");
        }

        command
            .arg("-c")
            .arg(CODEX_REASONING_EFFORT_CONFIG)
            .arg("--model")
            .arg(model)
            .arg("--full-auto")
            .arg("--json")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command
    }
}

/// Prefixes a user prompt with worktree root instructions when `AGENTS.md`
/// exists and is non-empty.
fn prepend_root_instructions_if_available(prompt: &str, folder: &Path) -> String {
    let Some(instructions) = load_root_agents_instructions(folder) else {
        return prompt.to_string();
    };

    format!("Project instructions from AGENTS.md:\n\n{instructions}\n\nUser prompt:\n{prompt}")
}

fn load_root_agents_instructions(folder: &Path) -> Option<String> {
    let agents_markdown = folder.join("AGENTS.md");

    fs::read_to_string(agents_markdown)
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn build_start_command_appends_root_instructions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;
        let instructions = "Follow project rules";
        std::fs::write(temp_directory.path().join("AGENTS.md"), instructions)
            .expect("failed to write test instructions");

        // Act
        let command = AgentBackend::build_start_command(
            &backend,
            temp_directory.path(),
            "Run checks",
            "gpt-5.3-codex",
        );
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("Project instructions from AGENTS.md"));
        assert!(debug_command.contains(instructions));
        assert!(debug_command.contains("-c"));
        assert!(debug_command.contains("model_reasoning_effort"));
        assert!(debug_command.contains("high"));
        assert!(
            debug_command.contains("User prompt:\nRun checks")
                || debug_command.contains("User prompt:\\nRun checks")
        );
    }

    /// Verifies resume command composes replay-based prompt content when
    /// session output is available.
    #[test]
    fn build_resume_command_appends_root_instructions_and_session_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;
        let instructions = "Follow project rules";
        std::fs::write(temp_directory.path().join("AGENTS.md"), instructions)
            .expect("failed to write test instructions");

        // Act
        let command = AgentBackend::build_resume_command(
            &backend,
            temp_directory.path(),
            "Continue edits",
            "gpt-5.3-codex",
            Some("previous assistant output".to_string()),
        );
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("Project instructions from AGENTS.md"));
        assert!(debug_command.contains(instructions));
        assert!(debug_command.contains("-c"));
        assert!(debug_command.contains("model_reasoning_effort"));
        assert!(debug_command.contains("high"));
        assert!(debug_command.contains("Continue this session using the full transcript below."));
        assert!(debug_command.contains("previous assistant output"));
    }

    /// Verifies resume command keeps a plain user prompt when no session output
    /// is available for replay.
    #[test]
    fn build_resume_command_uses_plain_prompt_without_session_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;
        let instructions = "Follow project rules";
        std::fs::write(temp_directory.path().join("AGENTS.md"), instructions)
            .expect("failed to write test instructions");

        // Act
        let command = AgentBackend::build_resume_command(
            &backend,
            temp_directory.path(),
            "Continue edits",
            "gpt-5.3-codex",
            None,
        );
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("exec"));
        assert!(debug_command.contains("resume"));
        assert!(debug_command.contains("--last"));
        assert!(debug_command.contains("Project instructions from AGENTS.md"));
        assert!(debug_command.contains(instructions));
        assert!(debug_command.contains("-c"));
        assert!(debug_command.contains("model_reasoning_effort"));
        assert!(debug_command.contains("high"));
        assert!(
            debug_command.contains("User prompt:\nContinue edits")
                || debug_command.contains("User prompt:\\nContinue edits")
        );
        assert!(!debug_command.contains("Continue this session using the full transcript below."));
    }
}
