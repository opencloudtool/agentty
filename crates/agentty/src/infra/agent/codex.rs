use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, build_resume_prompt};
use crate::domain::permission::PermissionMode;

/// Uses non-interactive Codex commands so Agentty can capture piped output.
///
/// Interactive `codex` requires a TTY and fails in this app with
/// `Error: stdout is not a terminal`, so this backend runs
/// `codex exec --full-auto` and `codex exec resume --last --full-auto`.
pub(super) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) {
        // Codex CLI needs no config files
    }

    fn build_start_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
    ) -> Command {
        let prompt = permission_mode.apply_to_prompt(prompt, is_initial_plan_prompt);
        let approval_flag = Self::approval_flag(permission_mode);
        let mut command = Command::new("codex");
        command
            .arg("exec")
            .arg("--model")
            .arg(model)
            .arg(approval_flag)
            .arg("--json")
            .arg(prompt.as_ref())
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
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
        session_output: Option<String>,
    ) -> Command {
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let prompt = permission_mode.apply_to_prompt(&prompt, is_initial_plan_prompt);
        let approval_flag = Self::approval_flag(permission_mode);
        let mut command = Command::new("codex");
        command
            .arg("exec")
            .arg("resume")
            .arg("--last")
            .arg("--model")
            .arg(model)
            .arg(approval_flag)
            .arg("--json")
            .arg(prompt.as_ref())
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command
    }
}

impl CodexBackend {
    fn approval_flag(permission_mode: PermissionMode) -> &'static str {
        match permission_mode {
            PermissionMode::AutoEdit | PermissionMode::Plan => "--full-auto",
            PermissionMode::Autonomous => "--yolo",
        }
    }
}
