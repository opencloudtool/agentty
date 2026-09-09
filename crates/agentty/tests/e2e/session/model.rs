//! Model selection and retired model replacement.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use testty::assertion;
use testty::region::Region;

use super::fixture::E2eResult;
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Adds a Gemini CLI stub that intentionally exits with failure.
///
/// Picker tests only need the executable to exist on `PATH`; using a failing
/// stub keeps accidental provider execution from looking successful.
fn seed_failing_gemini_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("gemini");
    std::fs::write(&stub_agent_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Adds an Antigravity CLI stub that intentionally exits with failure.
///
/// Picker tests only need the executable to exist on `PATH`; using a failing
/// stub keeps accidental provider execution from looking successful.
fn seed_failing_antigravity_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("agy");
    std::fs::write(
        &stub_agent_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'agy 1.2.0\\n'; exit 0; fi\nexit \
         1\n",
    )?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Adds an outdated Antigravity CLI stub and one supported fallback provider.
fn seed_outdated_antigravity_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let antigravity_path = env.stub_bin.join("agy");
    std::fs::write(
        &antigravity_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'agy 1.1.17\\n'; exit 0; fi\nexit \
         1\n",
    )?;
    let codex_path = env.stub_bin.join("codex");
    std::fs::write(&codex_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    {
        std::fs::set_permissions(&antigravity_path, std::fs::Permissions::from_mode(0o750))?;
        std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o750))?;
    }

    Ok(())
}

/// Adds a Codex CLI stub that intentionally exits with failure.
///
/// Picker tests only need the executable to exist on `PATH`; using a failing
/// stub keeps accidental provider execution from looking successful.
fn seed_failing_codex_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("codex");
    std::fs::write(&stub_agent_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Adds Gemini and Antigravity CLI stubs so both Google-backed providers
/// appear in stable `/model` picker positions.
fn seed_model_picker_cli_stubs(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_failing_gemini_cli_stub(env)?;
    seed_failing_antigravity_cli_stub(env)?;

    Ok(())
}

/// Adds all agent CLI stubs so provider picker tests have stable ordering.
fn seed_all_model_picker_cli_stubs(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_model_picker_cli_stubs(env)?;
    seed_failing_codex_cli_stub(env)?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes the current Gemini models
/// when the Gemini CLI is locally available.
#[test]
fn gemini_model_picker_lists_current_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("gemini_model_picker_lists_current_models")
        .with_git()
        .setup(seed_failing_gemini_cli_stub)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Enter")
                    .wait_for_text("gemini-3.8-flash", 3000)
                    .capture_labeled(
                        "gemini_model_picker",
                        "Gemini model picker lists current models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gemini-3.8-flash", &full);
                assertion::assert_text_in_region(frame, "gemini-3.5-flash-lite", &full);
            },
        )?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes the current Claude models
/// when the Claude CLI is locally available.
#[test]
fn claude_model_picker_lists_current_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("claude_model_picker_lists_current_models")
        .with_git()
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("claude-opus-5", 3000)
                    .capture_labeled(
                        "claude_model_picker",
                        "Claude model picker lists current models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "claude-fable-5", &full);
                assertion::assert_text_in_region(frame, "claude-opus-5", &full);
                assertion::assert_text_in_region(frame, "claude-sonnet-5", &full);
                assertion::assert_text_in_region(frame, "claude-haiku-4-5-20251001", &full);
            },
        )?;

    Ok(())
}

/// Seeds one still-active review session whose persisted model id has been
/// retired in favor of `gemini-3.5-flash-lite`.
fn seed_active_session_with_retired_model(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("retired-model-0001", "gemini-3.5-flash", "main", "Review")
            .with_title("Retired model"),
    )?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("retired-"))?;

    Ok(())
}

/// Verify that a still-active session stored on a retired model id is
/// switched automatically to the replacement model.
#[test]
fn retired_model_session_switches_to_replacement() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("retired_model_session_switches_to_replacement")
        .with_git()
        .setup(seed_active_session_with_retired_model)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("gemini-3.5-flash-lite", 5000)
                    .capture_labeled(
                        "retired_model_replacement",
                        "Active session switched to the replacement model",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Retired model", &full);
                assertion::assert_text_in_region(frame, "gemini-3.5-flash-lite", &full);
                assertion::assert_match_count(frame, "gemini-3.5-flash [medium]", 0);
            },
        )?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes the current Codex models in
/// the expected order.
#[test]
fn codex_model_picker_lists_current_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("codex_model_picker_lists_current_models")
        .with_git()
        .setup(seed_all_model_picker_cli_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("gpt-6-astra", 3000)
                    .capture_labeled(
                        "codex_model_picker",
                        "Codex model picker lists current models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-6-astra", &full);
                assertion::assert_text_in_region(frame, "gpt-5.6-sol", &full);
                assertion::assert_text_in_region(frame, "gpt-5.6-terra", &full);
                assertion::assert_text_in_region(frame, "gpt-5.6-luna", &full);
                assertion::assert_text_in_region(frame, "gpt-5.3-codex-spark", &full);
            },
        )?;

    Ok(())
}

/// Verify that the prompt `/model` picker exposes Gemini model choices for
/// Antigravity when `agy` is locally available.
#[test]
fn antigravity_model_picker_includes_gemini_models() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("antigravity_model_picker_includes_gemini_models")
        .with_git()
        .setup(seed_model_picker_cli_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Enter")
                    .wait_for_text("gemini-3.8-flash", 3000)
                    .capture_labeled(
                        "antigravity_model_picker",
                        "Antigravity model picker includes Gemini models",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gemini-3.1-pro-preview", &full);
                assertion::assert_text_in_region(frame, "gemini-3.8-flash", &full);
                assertion::assert_text_in_region(frame, "gemini-3.5-flash-lite", &full);
            },
        )?;

    Ok(())
}

/// Verify outdated Antigravity installations are excluded from the provider
/// picker while supported fallback providers remain selectable.
#[test]
fn antigravity_model_picker_excludes_outdated_cli() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("antigravity_model_picker_excludes_outdated_cli")
        .with_git()
        .setup(seed_outdated_antigravity_cli_stub)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .capture_labeled(
                        "supported_agent_picker",
                        "Provider picker excludes outdated Antigravity",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Codex CLI", &full);
                assertion::assert_not_visible(frame, "Antigravity CLI");
            },
        )?;

    Ok(())
}
