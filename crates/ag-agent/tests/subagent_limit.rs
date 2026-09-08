//! Verifies native subagent limits at the public provider command boundary.

use std::ffi::OsStr;

use ag_agent::{
    AgentKind, AgentRequestKind, BuildCommandRequest, PermissionMode, ReasoningLevel, SpeedMode,
    create_backend,
};
use tempfile::tempdir;

#[test]
fn native_subagent_limits_apply_to_new_resumed_and_utility_sessions() {
    // Arrange
    let workspace = tempdir().expect("workspace should exist");
    let request_kinds = [
        AgentRequestKind::SessionStart,
        AgentRequestKind::SessionResume,
        AgentRequestKind::UtilityPrompt,
        AgentRequestKind::FocusedReview,
    ];

    for (kind, model) in [
        (AgentKind::Codex, "gpt-5.6-sol"),
        (AgentKind::Claude, "claude-sonnet-5"),
    ] {
        let backend = create_backend(kind);
        for request_kind in &request_kinds {
            for permission_mode in PermissionMode::ALL {
                // Act
                let command = backend
                    .build_command(BuildCommandRequest {
                        attachments: &[],
                        folder: workspace.path(),
                        main_checkout_root: None,
                        model,
                        permission_mode,
                        personality_prompt: None,
                        prompt: "Inspect this project",
                        reasoning_level: ReasoningLevel::default(),
                        replay_transcript: None,
                        request_kind,
                        speed_mode: SpeedMode::default(),
                    })
                    .expect("provider command should build");

                // Assert
                assert_eq!(command.get_current_dir(), Some(workspace.path()));
                if kind == AgentKind::Codex {
                    let arguments = command.get_args().collect::<Vec<_>>();
                    assert!(arguments.windows(2).any(|pair| {
                        pair[0] == "-c" && pair[1] == "agents.max_concurrent_threads_per_session=2"
                    }));
                } else {
                    assert!(command.get_envs().any(|(key, value)| {
                        key == "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS"
                            && value == Some(OsStr::new("2"))
                    }));
                }
            }
        }
    }
}
