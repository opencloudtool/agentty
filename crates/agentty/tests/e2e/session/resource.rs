//! Session resource tracking.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use testty::assertion;
use testty::region::Region;

use super::fixture::{E2eResult, seed_project_settings, seed_sessions_tab};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Seeds a completed Gemini turn and host samples that keep reusing its PID.
/// A fixture marker switches the turn to a transport failure on every retry.
fn seed_gemini_resource_runtime(env: &BuilderEnv) -> E2eResult {
    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "gemini"),
            ("DefaultSmartModel", "gemini-3.1-pro-preview"),
            ("DefaultFastAgent", "codex"),
            ("DefaultFastModel", "gpt-5.6-sol"),
        ],
    )?;
    let scripts = [
        (
            "gemini",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'gemini 0.0.0-test\n'; exit 0; fi
if [ -f "$HOME/resource-delay-retry" ] && [ -f "$HOME/resource-retry-started" ]; then sleep 8; fi
printf '%s\n' "$$" > "$HOME/resource-agent-pid"
while IFS= read -r request; do
    request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    case "$request" in
        *'"method":"initialize"'*)
            printf '{"jsonrpc":"2.0","id":"%s","result":{"protocolVersion":1}}\n' "$request_id"
            ;;
        *'"method":"session/new"'*)
            printf '{"jsonrpc":"2.0","id":"%s","result":{"sessionId":"resource-session"}}\n' "$request_id"
            ;;
        *'"method":"session/prompt"'*)
            if [ -f "$HOME/resource-delay-retry" ]; then
                if [ ! -f "$HOME/resource-retry-started" ]; then
                    while [ ! -f "$HOME/resource-sampled" ]; do sleep 0.1; done
                    sleep 2
                    touch "$HOME/resource-retry-started"
                    printf '{"jsonrpc":"2.0","id":"%s","error":{"code":-32000,"message":"Retry resource runtime."}}\n' "$request_id"
                    continue
                fi
                sleep 30
            fi
            if [ -f "$HOME/resource-fail-turn" ]; then
                printf '{"jsonrpc":"2.0","id":"%s","error":{"code":-32000,"message":"Resource runtime failed."}}\n' "$request_id"
                continue
            fi
            printf '{"jsonrpc":"2.0","id":"%s","result":{"response":%s}}\n' "$request_id" '"{\"answer\":\"Resource turn completed.\",\"questions\":[]}"'
            ;;
    esac
done
"#,
        ),
        (
            "ps",
            r#"#!/bin/sh
if [ ! -f "$HOME/resource-agent-pid" ]; then exit 0; fi
read -r agent_pid < "$HOME/resource-agent-pid"
printf '%s 1 90.0 8192 S\n' "$agent_pid"
touch "$HOME/resource-sampled"
"#,
        ),
    ];
    for (name, script) in scripts {
        let path = env.stub_bin.join(name);
        std::fs::write(&path, script)?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))?;
    }

    Ok(())
}

/// Shows unavailable accounting before launch and deterministic process-tree
/// totals while an isolated CLI turn is running.
#[test]
fn test_session_resources() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_resources")
        .with_git()
        .setup(|env| {
            seed_sessions_tab(env)?;
            seed_project_settings(
                env,
                &[
                    ("DefaultSmartAgent", "claude"),
                    ("DefaultSmartModel", "claude-haiku-4-5-20251001"),
                    ("DefaultFastAgent", "codex"),
                    ("DefaultFastModel", "gpt-5.6-sol"),
                ],
            )?;
            let scripts = [
                (
                    "claude",
                    r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
printf '%s\n' "$$" > "$HOME/resource-agent-pid"
cat >/dev/null
sleep 30
"#,
                ),
                (
                    "ps",
                    r#"#!/bin/sh
if [ ! -f "$HOME/resource-agent-pid" ]; then exit 0; fi
read -r agent_pid < "$HOME/resource-agent-pid"
printf '%s 1 12.5 2048 S\n2147483640 %s 2.5 1024 S\n2147483639 1 90.0 8192 S\n' "$agent_pid" "$agent_pid"
"#,
                ),
            ];
            for (name, script) in scripts {
                let path = env.stub_bin.join(name);
                std::fs::write(&path, script)?;
                #[cfg(unix)]
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))?;
            }

            Ok(())
        })
        .zola(
            "Session resources",
            "Inspect agent process count, CPU usage, and resident memory in session chat.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 5000)
                    .write_text("Measure session resources")
                    .press_key("Enter")
                    .step(testty::step::Step::eventually(
                        Duration::from_secs(15),
                        Duration::from_millis(50),
                        |frame| {
                            assertion::match_text_in_region(
                                frame,
                                "Processes: 2  CPU: 15.0%  Memory: 3.0 MiB",
                                &Region::full(frame.cols(), frame.rows()),
                            )
                        },
                    ))
                    .capture_labeled("resources", "Tracked agent and child process usage")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Processes: 2  CPU: 15.0%  Memory: 3.0 MiB",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Seeds a retained Codex runtime and deterministic accounting. A marker
/// switches idle exit into a failing commit hook repaired by a one-shot
/// runtime.
fn seed_retained_resource_runtime(env: &BuilderEnv) -> E2eResult {
    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "codex"),
            ("DefaultSmartModel", "gpt-5.6-sol"),
            ("DefaultFastAgent", "claude"),
            ("DefaultFastModel", "claude-haiku-4-5-20251001"),
        ],
    )?;
    let scripts = [
        (
            "claude",
            r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat >/dev/null
printf '%s\n' '{"type":"result","subtype":"success","result":"","structured_output":{"answer":"fix: retain runtime resources","questions":[]},"usage":{"input_tokens":1,"output_tokens":1}}'
"#,
        ),
        (
            "codex",
            r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.146.0\n'; exit 0; fi
# Publish the root before initialization can announce it to the monitor.
retained_runtime=false
if [ ! -f "$HOME/resource-agent-pid" ]; then
    printf '%s\n' "$$" > "$HOME/resource-agent-pid"
    retained_runtime=true
fi
while IFS= read -r request; do
    request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    case "$request" in
        *'"method":"initialize"'*)
            printf '{"id":"%s","result":{}}\n' "$request_id"
            ;;
        *'"method":"thread/start"'*)
            printf '{"id":"%s","result":{"thread":{"id":"resource-thread"}}}\n' "$request_id"
            ;;
        *'"method":"turn/start"'*)
            answer='"{\"answer\":\"Retained turn completed.\",\"questions\":[]}"'
            if [ "$retained_runtime" = true ]; then
                while [ ! -f "$HOME/resource-sampled" ]; do sleep 0.1; done
                if [ -f "$HOME/resource-commit-assist" ]; then
                    printf 'pending resource change\n' > generated.txt
                    touch "$HOME/resource-hook-ready"
                fi
            else
                sleep 3
                touch "$HOME/resource-hook-repaired"
                answer='"{\"answer\":\"Commit assistance completed.\",\"questions\":[]}"'
            fi
            printf '{"id":"%s","result":{"turn":{"id":"resource-turn"}}}\n' "$request_id"
            printf '{"method":"item/completed","params":{"threadId":"resource-thread","turnId":"resource-turn","item":{"type":"agentMessage","id":"final-item","text":%s,"phase":"final_answer"}}}\n' "$answer"
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"resource-thread","turn":{"id":"resource-turn","status":"completed","items":[]}}}'
            if [ ! -f "$HOME/resource-commit-assist" ]; then
                sleep 3
                touch "$HOME/resource-exited"
                exit 0
            fi
            ;;
    esac
done
"#,
        ),
        (
            "ps",
            r#"#!/bin/sh
if [ ! -f "$HOME/resource-agent-pid" ]; then exit 0; fi
read -r agent_pid < "$HOME/resource-agent-pid"
if [ ! -f "$HOME/resource-exited" ]; then
    printf '%s 1 12.5 2048 S\n' "$agent_pid"
    touch "$HOME/resource-sampled"
elif [ ! -f "$HOME/resource-zombie-sampled" ]; then
    printf '%s 1 12.5 0 Z\n' "$agent_pid"
    touch "$HOME/resource-zombie-sampled"
else
    printf '%s 1 90.0 8192 S\n' "$agent_pid"
fi
"#,
        ),
    ];
    for (name, script) in scripts {
        let path = env.stub_bin.join(name);
        std::fs::write(&path, script)?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))?;
    }

    Ok(())
}

/// An idle retained runtime becomes unavailable after exit, even when later
/// host snapshots reuse its numeric PID for another process.
#[test]
fn session_resources_after_retained_runtime_exit() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_resources_after_retained_runtime_exit")
        .with_git()
        .setup(seed_retained_resource_runtime)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 5000)
                    .write_text("Complete and exit while idle")
                    .press_key("Enter")
                    .wait_for_text("Retained turn completed.", 15000)
                    .wait_for_text("Processes: 1  CPU: 12.5%  Memory: 2.0 MiB", 5000)
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 15000)
                    .sleep_ms(4500)
                    .capture_labeled("exited", "Idle exit and PID reuse remain unavailable")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Retained turn completed.", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Processes: --  CPU: --  Memory: --",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// A separate auto-commit assist runtime must not clear the live chat
/// runtime's resource root, including after its one-shot cleanup finishes.
#[test]
fn session_resources_after_auto_commit_assistance() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_resources_after_auto_commit_assistance")
        .with_git()
        .setup(|env| {
            seed_retained_resource_runtime(env)?;
            std::fs::write(env.home_dir.join("resource-commit-assist"), "assist")?;
            let hook = env.workdir.join(".git/hooks/pre-commit");
            std::fs::write(
                &hook,
                r#"#!/bin/sh
if [ -f "$HOME/resource-hook-ready" ] && [ ! -f "$HOME/resource-hook-repaired" ]; then
    printf 'Resource commit hook blocked.\n' >&2
    exit 1
fi
exit 0
"#,
            )?;
            #[cfg(unix)]
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o750))?;

            Ok(())
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 5000)
                    .write_text("Recover a failed commit and retain the chat runtime")
                    .press_key("Enter")
                    .wait_for_text("Retained turn completed.", 15000)
                    .wait_for_text("Commit assistance completed.", 30000)
                    .sleep_ms(2500)
                    .capture_labeled(
                        "assisted",
                        "Retained runtime resources after commit assistance",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Commit assistance completed.", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Processes: 1  CPU: 12.5%  Memory: 2.0 MiB",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Completed Gemini turns must stop accounting for their terminated runtime,
/// even when a later host snapshot contains the same PID.
#[test]
fn session_resources_after_gemini_completion() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_resources_after_gemini_completion")
        .with_git()
        .setup(seed_gemini_resource_runtime)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 5000)
                    .write_text("Finish this resource check")
                    .press_key("Enter")
                    .wait_for_text("Resource turn completed.", 15000)
                    .sleep_ms(2500)
                    .capture_labeled("completed", "Terminated runtime has no tracked resources")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Resource turn completed.", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Processes: --  CPU: --  Memory: --",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Failed and retried Gemini runtimes must stop contributing resources even
/// when the host keeps reporting their last PID as a live unrelated process.
#[test]
fn session_resources_after_gemini_failure() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_resources_after_gemini_failure")
        .with_git()
        .setup(|env| {
            seed_gemini_resource_runtime(env)?;
            std::fs::write(env.home_dir.join("resource-fail-turn"), "fail")?;

            Ok(())
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 5000)
                    .write_text("Run the failing resource check")
                    .press_key("Enter")
                    .wait_for_text("Resource runtime failed.", 15000)
                    .sleep_ms(2500)
                    .capture_labeled("failed", "Failed runtime has no tracked resources")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Resource runtime failed.", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Processes: --  CPU: --  Memory: --",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Accounting ignores a recycled PID throughout delayed retry startup and
/// resumes only when the replacement runtime announces its own PID.
#[test]
fn session_resources_during_delayed_retry() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_resources_during_delayed_retry")
        .with_git()
        .setup(|env| {
            seed_gemini_resource_runtime(env)?;
            std::fs::write(env.home_dir.join("resource-delay-retry"), "delay")?;

            Ok(())
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 5000)
                    .write_text("Retry the resource check")
                    .press_key("Enter")
                    .wait_for_text("Processes: 1  CPU: 90.0%  Memory: 8.0 MiB", 15000)
                    .wait_for_text("Processes: --  CPU: --  Memory: --", 10000)
                    .sleep_ms(2500)
                    .capture_labeled(
                        "restarting",
                        "Exited PID excluded during replacement startup",
                    )
                    .wait_for_text("Processes: 1  CPU: 90.0%  Memory: 8.0 MiB", 15000)
                    .capture_labeled("replacement", "Replacement runtime resources")
            },
            |frame, report| {
                let restarting = common::frame_from_capture(&report.captures[0]);
                assertion::assert_text_in_region(
                    &restarting,
                    "Processes: --  CPU: --  Memory: --",
                    &Region::full(restarting.cols(), restarting.rows()),
                );
                assertion::assert_text_in_region(
                    frame,
                    "Processes: 1  CPU: 90.0%  Memory: 8.0 MiB",
                    &Region::full(frame.cols(), frame.rows()),
                );
            },
        )?;

    Ok(())
}
